//! Transcripts on disk (`*.words.json`): finding them and the scripts that use
//! them, edits across many at once that keep those scripts in step (so captions
//! still line up word for word), and picking which to clean up over a size limit.

use crate::{resolve, words_path};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use vg_core::Transcript;
use vg_core::paths::{canon, relative_path};
use vg_core::transcript::Segment;

pub const SUFFIX: &str = ".words.json";
/// Files a transcript can be made from: audio, and video with a sound track.
pub const AUDIO_EXTS: &[&str] = &["wav", "flac", "mp3", "ogg", "m4a", "opus", "mp4", "mkv", "mov"];

#[derive(Clone, Debug)]
pub struct Entry {
    pub path: PathBuf,
    pub transcript: Transcript,
    /// Why it couldn't be read (it's still listed, so it can be removed).
    pub error: Option<String>,
    /// The recording it came from, if it's next to it.
    pub audio: Option<PathBuf>,
    /// Scripts whose `@words` points at it.
    pub scripts: Vec<PathBuf>,
    pub modified: Option<SystemTime>,
    pub bytes: u64,
    pub audio_bytes: u64,
}

impl Entry {
    /// `take3` for `take3.words.json`.
    pub fn name(&self) -> String {
        name_of(&self.path)
    }

    pub fn word_count(&self) -> usize {
        self.transcript.segments.iter().map(|s| s.words.len()).sum()
    }

    pub fn text(&self) -> String {
        self.transcript.segments.iter().flat_map(|s| s.words.iter().map(|w| w.word.trim())).collect::<Vec<_>>().join(" ")
    }
}

pub fn name_of(path: &Path) -> String {
    let n = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    n.strip_suffix(SUFFIX).map(str::to_string).unwrap_or(n)
}

/// Every transcript directly inside `dirs` (not their subfolders), by name.
pub fn scan(dirs: &[PathBuf]) -> Vec<Entry> {
    let mut seen_dirs: Vec<PathBuf> = vec![];
    for d in dirs {
        let c = canon(d);
        if c.is_dir() && !seen_dirs.contains(&c) {
            seen_dirs.push(c);
        }
    }
    let mut files: Vec<PathBuf> = vec![];
    let mut scripts: Vec<(PathBuf, PathBuf)> = vec![]; // (script, the words file it uses)
    for d in &seen_dirs {
        let Ok(rd) = std::fs::read_dir(d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if name.ends_with(SUFFIX) && p.is_file() {
                files.push(p);
            } else if name.ends_with(".md")
                && let Some(w) = std::fs::read_to_string(&p).ok().and_then(|t| words_path(&t, d)) {
                    scripts.push((p, canon(&w)));
                }
        }
    }
    let mut out: Vec<Entry> = files
        .into_iter()
        .map(|path| {
            let (transcript, error) = match Transcript::load(&path) {
                Ok(t) => (t, None),
                Err(e) => (Transcript::default(), Some(format!("{e:#}"))),
            };
            let meta = std::fs::metadata(&path).ok();
            let audio = audio_for(&path);
            let c = canon(&path);
            let mut used: Vec<PathBuf> = scripts.iter().filter(|(_, w)| *w == c).map(|(s, _)| s.clone()).collect();
            used.sort();
            Entry {
                audio_bytes: audio.as_ref().and_then(|a| std::fs::metadata(a).ok()).map_or(0, |m| m.len()),
                audio,
                scripts: used,
                modified: meta.as_ref().and_then(|m| m.modified().ok()),
                bytes: meta.map_or(0, |m| m.len()),
                transcript,
                error,
                path,
            }
        })
        .collect();
    // By name, so the list doesn't reorder as transcripts are edited.
    out.sort_by(|a, b| a.name().to_lowercase().cmp(&b.name().to_lowercase()).then_with(|| a.path.cmp(&b.path)));
    out
}

/// The recording next to a transcript: `take3.wav` for `take3.words.json`.
pub fn audio_for(words: &Path) -> Option<PathBuf> {
    let stem = name_of(words);
    let dir = words.parent()?;
    AUDIO_EXTS.iter().map(|e| dir.join(format!("{stem}.{e}"))).find(|p| p.is_file())
}

// ---- matching words ---------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MatchOpts {
    /// Match whole words only (otherwise a one-word search also matches inside words).
    pub whole_word: bool,
    pub match_case: bool,
}

impl Default for MatchOpts {
    fn default() -> Self {
        MatchOpts { whole_word: true, match_case: false }
    }
}

/// A word split into leading punctuation, its core and trailing punctuation.
fn split_punct(w: &str) -> (&str, &str, &str) {
    let is_core = |c: char| c.is_alphanumeric() || c == '\'' || c == '’';
    let start = w.find(is_core).unwrap_or(w.len());
    let end = w.rfind(is_core).map(|i| i + w[i..].chars().next().unwrap().len_utf8()).unwrap_or(start);
    (&w[..start], &w[start..end], &w[end..])
}

fn eq(a: &str, b: &str, case: bool) -> bool {
    if case { a == b } else { a.to_lowercase() == b.to_lowercase() }
}

/// One replacement: the tokens `range` become `with`.
struct Edit {
    range: std::ops::Range<usize>,
    with: Vec<String>,
}

/// Where `find` occurs in `tokens` and what each occurrence becomes.
fn plan(tokens: &[String], find: &str, with: &str, m: MatchOpts) -> Vec<Edit> {
    let f: Vec<&str> = find.split_whitespace().collect();
    if f.is_empty() {
        return vec![];
    }
    let w: Vec<&str> = with.split_whitespace().collect();
    let mut edits = vec![];
    let mut i = 0;
    while i < tokens.len() {
        // Inside a word: one-word searches only.
        if !m.whole_word && f.len() == 1 {
            let tok = &tokens[i];
            let hay = if m.match_case { tok.clone() } else { tok.to_lowercase() };
            let needle = if m.match_case { f[0].to_string() } else { f[0].to_lowercase() };
            if !needle.is_empty() && hay.contains(&needle) && hay.len() == tok.len() {
                let mut out = String::new();
                let mut rest = tok.as_str();
                let mut hrest = hay.as_str();
                while let Some(at) = hrest.find(&needle) {
                    out += &rest[..at];
                    out += with;
                    rest = &rest[at + needle.len()..];
                    hrest = &hrest[at + needle.len()..];
                }
                out += rest;
                let with: Vec<String> = out.split_whitespace().map(str::to_string).collect();
                edits.push(Edit { range: i..i + 1, with });
            }
            i += 1;
            continue;
        }
        if i + f.len() <= tokens.len() && f.iter().enumerate().all(|(k, fw)| eq(split_punct(&tokens[i + k]).1, split_punct(fw).1, m.match_case)) {
            let (lead, core, _) = split_punct(&tokens[i]);
            let (_, _, trail) = split_punct(&tokens[i + f.len() - 1]);
            let mut new: Vec<String> = w.iter().map(|s| s.to_string()).collect();
            if let Some(first) = new.first_mut() {
                // "Um" at the start of a sentence → "So", not "so".
                if !m.match_case && core.chars().next().is_some_and(char::is_uppercase) && first.chars().all(|c| !c.is_uppercase()) {
                    let mut cs = first.chars();
                    *first = cs.next().map(|c| c.to_uppercase().collect::<String>() + cs.as_str()).unwrap_or_default();
                }
                *first = format!("{lead}{first}");
            }
            if let Some(last) = new.last_mut() {
                *last = format!("{last}{trail}");
            }
            edits.push(Edit { range: i..i + f.len(), with: new });
            i += f.len();
        } else {
            i += 1;
        }
    }
    edits
}

/// Apply `edits` to `tokens`. Deleting a word that ends a sentence moves its
/// full stop onto the word before ("works, um." → "works.").
fn apply_tokens(tokens: &[String], edits: &[Edit]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(tokens.len());
    let mut i = 0;
    for e in edits {
        out.extend(tokens[i..e.range.start].iter().cloned());
        if e.with.is_empty() {
            let (_, _, trail) = split_punct(&tokens[e.range.end - 1]);
            if trail.contains(['.', '!', '?', '…'])
                && let Some(prev) = out.last_mut() {
                    let t = prev.trim_end_matches([',', ';', ':']).to_string();
                    if !t.ends_with(['.', '!', '?', '…']) {
                        *prev = t + trail.trim_start_matches([',', ';', ':']);
                    }
                }
        } else {
            out.extend(e.with.iter().cloned());
        }
        i = e.range.end;
    }
    out.extend(tokens[i..].iter().cloned());
    out
}

/// Replace `find` with `with` in a transcript's words. Multi-word phrases match
/// across words; the replacement's words share the matched words' time. An empty
/// `with` deletes the words. Returns the number of replacements.
pub fn replace(t: &mut Transcript, find: &str, with: &str, m: MatchOpts) -> usize {
    let words = t.words();
    let tokens: Vec<String> = words.iter().map(|w| w.word.trim().to_string()).collect();
    let edits = plan(&tokens, find, with, m);
    if edits.is_empty() {
        return 0;
    }
    let mut out: Vec<vg_core::Word> = vec![];
    let mut i = 0;
    for e in &edits {
        out.extend(words[i..e.range.start].iter().cloned());
        let (a, b) = (words[e.range.start].start, words[e.range.end - 1].end);
        let piece = apply_tokens(&tokens[e.range.clone()], &[Edit { range: 0..e.range.len(), with: e.with.clone() }]);
        if piece.is_empty() {
            // Carry a sentence end onto the word before, as in the script.
            if let (Some(prev), Some(last)) = (out.last_mut(), tokens.get(e.range.end - 1)) {
                let fixed = apply_tokens(&[prev.word.clone(), last.clone()], &[Edit { range: 1..2, with: vec![] }]);
                prev.word = fixed[0].clone();
            }
        } else {
            let total: usize = piece.iter().map(|p| p.chars().count().max(1)).sum();
            let mut acc = 0;
            for p in &piece {
                let s = a + (b - a) * acc as f64 / total as f64;
                acc += p.chars().count().max(1);
                let e_ = a + (b - a) * acc as f64 / total as f64;
                out.push(vg_core::Word { start: s, end: e_, word: p.clone() });
            }
        }
        i = e.range.end;
    }
    out.extend(words[i..].iter().cloned());
    set_words(t, out);
    edits.len()
}

/// Delete each of `fillers` ("um", "uh", …) wherever it's a whole word.
pub fn remove(t: &mut Transcript, fillers: &[String]) -> usize {
    fillers.iter().map(|f| replace(t, f, "", MatchOpts::default())).sum()
}

/// Move every word by `secs` (never before 0).
pub fn shift(t: &mut Transcript, secs: f64) -> usize {
    let mut words = t.words();
    for w in &mut words {
        w.start = (w.start + secs).max(0.0);
        w.end = (w.end + secs).max(w.start);
    }
    let n = words.len();
    set_words(t, words);
    n
}

/// Put `words` back into the transcript's segments (keeping the segment
/// boundaries where the words still fall inside them).
fn set_words(t: &mut Transcript, words: Vec<vg_core::Word>) {
    let bounds: Vec<(f64, f64)> = t.segments.iter().map(|s| (s.start, s.end)).collect();
    let mut segs: Vec<Segment> = bounds.iter().map(|&(start, end)| Segment { start, end, text: String::new(), words: vec![] }).collect();
    for w in words {
        // The last segment starting at or before the word.
        let k = bounds.iter().rposition(|&(s, _)| s <= w.start + 1e-6).unwrap_or(0);
        match segs.get_mut(k) {
            Some(s) => s.words.push(w),
            None => segs.push(Segment { start: w.start, end: w.end, text: String::new(), words: vec![w] }),
        }
    }
    segs.retain(|s| !s.words.is_empty());
    for s in &mut segs {
        s.start = s.words.first().map_or(s.start, |w| w.start.min(s.start));
        s.end = s.words.last().map_or(s.end, |w| w.end.max(s.end));
        s.text = s.words.iter().map(|w| w.word.trim()).collect::<Vec<_>>().join(" ");
    }
    t.segments = segs;
}

// ---- scripts ----------------------------------------------------------------------

/// The same replacement in a script's spoken text: directives, comments and
/// `[tags]` are left alone, so its words keep matching the transcript's.
pub fn replace_in_script(text: &str, find: &str, with: &str, m: MatchOpts) -> (String, usize) {
    let mut n = 0;
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let body = line.trim_end_matches(['\n', '\r']);
        let nl = &line[body.len()..];
        let t = body.trim_start();
        if t.is_empty() || t.starts_with('@') || t.starts_with('#') {
            out += line;
            continue;
        }
        // Split into bracketed parts (kept) and the text between them.
        let mut rebuilt = String::new();
        let mut rest = body;
        loop {
            let (text_part, after) = match rest.find('[') {
                Some(o) => (&rest[..o], Some(&rest[o..])),
                None => (rest, None),
            };
            let tokens: Vec<String> = text_part.split_whitespace().map(str::to_string).collect();
            let edits = plan(&tokens, find, with, m);
            if edits.is_empty() {
                rebuilt += text_part;
            } else {
                n += edits.len();
                let lead = &text_part[..text_part.len() - text_part.trim_start().len()];
                let trail = &text_part[text_part.trim_end().len()..];
                let joined = apply_tokens(&tokens, &edits).join(" ");
                rebuilt += lead;
                rebuilt += &joined;
                rebuilt += if joined.is_empty() && !lead.is_empty() { "" } else { trail };
            }
            let Some(after) = after else { break };
            match after.find(']') {
                Some(c) => {
                    rebuilt += &after[..=c];
                    rest = &after[c + 1..];
                }
                None => {
                    rebuilt += after;
                    break;
                }
            }
        }
        out += &rebuilt;
        out += nl;
    }
    (out, n)
}

pub fn remove_in_script(text: &str, fillers: &[String]) -> (String, usize) {
    let mut text = text.to_string();
    let mut n = 0;
    for f in fillers {
        let (t, k) = replace_in_script(&text, f, "", MatchOpts::default());
        text = t;
        n += k;
    }
    (text, n)
}

/// Point every `@words` line in `scripts` that names `old` at `new` instead.
/// Returns the scripts' new texts (not yet written).
pub fn repoint_scripts(scripts: &[PathBuf], old: &Path, new: &Path) -> Result<Vec<(PathBuf, String)>> {
    let old_c = canon(old);
    let mut out = vec![];
    for s in scripts {
        let dir = s.parent().unwrap_or(Path::new("."));
        let text = std::fs::read_to_string(s).with_context(|| format!("reading {}", s.display()))?;
        let mut changed = false;
        let lines: Vec<String> = text
            .split_inclusive('\n')
            .map(|line| {
                let body = line.trim_end_matches(['\r', '\n']);
                if let Some((v, tail)) = crate::directive(body, "words")
                    && canon(&resolve(dir, Path::new(v))) == old_c
                {
                    changed = true;
                    // Keep the line's indent, spacing, comment and line ending.
                    let indent = &body[..body.len() - body.trim_start().len()];
                    return format!("{indent}@words {}{tail}{}", relative_path(new, dir).display(), &line[body.len()..]);
                }
                line.to_string()
            })
            .collect();
        if changed {
            out.push((s.clone(), lines.concat()));
        }
    }
    Ok(out)
}

/// A transcript's new path for `new_name` (the same folder).
pub fn renamed_path(path: &Path, new_name: &str) -> Result<PathBuf> {
    let n = new_name.trim().trim_end_matches(SUFFIX);
    if n.is_empty() || n.contains(['/', '\\']) {
        bail!("give it a plain name, without folders");
    }
    Ok(path.with_file_name(format!("{n}{SUFFIX}")))
}

// ---- the size limit ----------------------------------------------------------------

/// Which transcripts to remove, oldest first, to bring the total under `limit`
/// bytes. A transcript counts its own size, plus its recording's when
/// `with_audio`. `keep` is never removed (the open script's, pinned ones…).
pub fn over_limit<'a>(entries: &'a [Entry], limit: u64, with_audio: bool, keep: &dyn Fn(&Entry) -> bool) -> Vec<&'a Entry> {
    let size = |e: &Entry| e.bytes + if with_audio { e.audio_bytes } else { 0 };
    let mut total: u64 = entries.iter().map(size).sum();
    let mut oldest: Vec<&Entry> = entries.iter().collect();
    oldest.sort_by(|a, b| a.modified.cmp(&b.modified).then_with(|| a.path.cmp(&b.path)));
    let mut out = vec![];
    for e in oldest {
        if total <= limit {
            break;
        }
        if keep(e) {
            continue;
        }
        total -= size(e);
        out.push(e);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use vg_core::Word;

    fn tr(words: &[(f64, &str)]) -> Transcript {
        let ws: Vec<Word> = words.iter().map(|&(s, w)| Word { start: s, end: s + 0.4, word: w.into() }).collect();
        Transcript { language: "en".into(), duration: 10.0, segments: vec![Segment { start: 0.0, end: 10.0, text: String::new(), words: ws }] }
    }

    fn text(t: &Transcript) -> String {
        t.words().iter().map(|w| w.word.as_str()).collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn replaces_phrases_and_keeps_times_and_punctuation() {
        let mut t = tr(&[(0.0, "I"), (0.5, "love"), (1.0, "code"), (1.5, "bullet,"), (2.0, "really.")]);
        assert_eq!(replace(&mut t, "Code Bullet", "CodeBullet", MatchOpts::default()), 1);
        assert_eq!(text(&t), "I love CodeBullet, really.");
        let w = &t.words()[2];
        assert_eq!((w.start, w.end), (1.0, 1.9));
        // Words out of one: the time is shared.
        replace(&mut t, "CodeBullet", "Code Bullet", MatchOpts::default());
        assert_eq!(text(&t), "I love Code Bullet, really.");
        assert!(t.words()[3].start > 1.0 && t.words()[3].end == 1.9);
        assert_eq!(t.segments[0].text, "I love Code Bullet, really.");
    }

    #[test]
    fn removes_fillers_and_keeps_the_sentence_end() {
        let mut t = tr(&[(0.0, "Um,"), (0.5, "it"), (1.0, "works,"), (1.5, "uh."), (2.0, "Nice")]);
        assert_eq!(remove(&mut t, &["um".into(), "uh".into()]), 2);
        assert_eq!(text(&t), "it works. Nice");
    }

    #[test]
    fn script_edits_skip_tags_and_directives() {
        let src = "# um a comment\n@audio um.wav\n[0:01] [wave] Um, hello code bullet fans\n[0:03] So [facepalm] um. it broke\n";
        let (out, n) = remove_in_script(src, &["um".into()]);
        assert_eq!(n, 2);
        assert_eq!(out, "# um a comment\n@audio um.wav\n[0:01] [wave] hello code bullet fans\n[0:03] So [facepalm] it broke\n");
        let (out, n) = replace_in_script(&out, "code bullet", "CodeBullet", MatchOpts::default());
        assert_eq!(n, 1);
        assert!(out.contains("[wave] hello CodeBullet fans"));
        // Inside words, when asked.
        let (out, n) = replace_in_script("[0:00] teh thetehs\n", "teh", "the", MatchOpts { whole_word: false, match_case: false });
        assert_eq!((out.as_str(), n), ("[0:00] the thethes\n", 2));
    }

    #[test]
    fn picks_the_oldest_unprotected_over_the_limit() {
        let e = |name: &str, age: u64, bytes: u64| Entry {
            path: PathBuf::from(format!("{name}.words.json")),
            transcript: Transcript::default(),
            error: None,
            audio: None,
            scripts: vec![],
            modified: Some(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1000 - age)),
            bytes,
            audio_bytes: 100,
        };
        let all = vec![e("new", 1, 10), e("mid", 5, 10), e("old", 9, 10), e("kept", 20, 10)];
        let names = |v: Vec<&Entry>| v.iter().map(|e| e.name()).collect::<Vec<_>>();
        assert_eq!(names(over_limit(&all, 25, false, &|e| e.name() == "kept")), ["old", "mid"]);
        assert!(over_limit(&all, 40, false, &|_| false).is_empty());
        // Counting the recordings, only one may stay.
        assert_eq!(names(over_limit(&all, 150, true, &|_| false)), ["kept", "old", "mid"]);
    }

    #[test]
    fn repointing_keeps_comments_and_spacing() {
        let dir = std::env::temp_dir().join(format!("vg-repoint-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (old, new) = (dir.join("take.words.json"), dir.join("renamed.words.json"));
        std::fs::write(&old, "{}").unwrap();
        let script = dir.join("s.md");
        std::fs::write(&script, "@words take.words.json   # timings\r\n  @words\ttake.words.json\n@words other.words.json\n[0:00] hi\n").unwrap();
        let edits = repoint_scripts(std::slice::from_ref(&script), &old, &new).unwrap();
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].1, "@words renamed.words.json   # timings\r\n  @words renamed.words.json\n@words other.words.json\n[0:00] hi\n");
        assert_eq!(words_path(&edits[0].1, &dir), Some(dir.join("renamed.words.json")));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
