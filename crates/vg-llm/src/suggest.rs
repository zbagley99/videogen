//! Pose suggestions for a script: batches of numbered lines go to the LLM with
//! the pose library; the answers are validated and repaired against both.

use crate::client::Client;
use crate::json::parse_lenient;
use anyhow::{Result, bail};
use serde_json::{Value, json};
use vg_core::cues::{CueSheet, Item, Placement, line_words, set_pose_tags};
use vg_core::PoseSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Suggest for lines without pose tags; keep existing tags.
    Untagged,
    /// Suggest for every line; applying replaces the existing pose tags.
    Replace,
}

#[derive(Clone, Debug)]
pub struct SuggestOptions {
    pub mode: Mode,
    /// Lines per request (small models do better with fewer).
    pub batch: usize,
    pub temperature: f32,
    /// Extra direction from the user ("more reactions", "calmer", …).
    pub style: String,
}

impl Default for SuggestOptions {
    fn default() -> Self {
        SuggestOptions { mode: Mode::Untagged, batch: 20, temperature: 0.4, style: String::new() }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Suggestion {
    /// 1-based line number in the script file.
    pub line_no: usize,
    /// The line's spoken text (for display).
    pub line_text: String,
    /// Word index the pose starts at (0 = start of the line) and that word.
    pub word: usize,
    pub word_text: String,
    /// Pose index in the library.
    pub pose: usize,
    pub reason: String,
}

/// Something that went wrong in the answers and what was done about it.
#[derive(Clone, Debug, PartialEq)]
pub struct Note {
    pub line_no: Option<usize>,
    pub text: String,
    /// `true` if it was repaired; `false` if the suggestion was dropped.
    pub fixed: bool,
}

struct Line {
    line_no: usize,
    words: Vec<String>,
    /// Pose tags already on the line (names as written).
    tags: Vec<String>,
}

fn norm(w: &str) -> String {
    w.chars().filter(|c| c.is_alphanumeric() || *c == '\'').collect::<String>().to_lowercase()
}

fn library_text(poses: &PoseSet) -> String {
    let mut s = String::new();
    for p in &poses.poses {
        s += &format!("- {}", p.tag);
        if !p.aliases.is_empty() {
            s += &format!(" (also: {})", p.aliases.join(", "));
        }
        if !p.description.is_empty() {
            s += &format!(": {}", p.description.trim());
        }
        if !p.use_when.is_empty() {
            s += &format!(" Good for: {}.", p.use_when.join("; "));
        }
        s.push('\n');
    }
    s
}

fn system_prompt(poses: &PoseSet, style: &str) -> String {
    let mut s = String::from(
        "You direct a cartoon avatar that reacts while a YouTuber narrates, in the style of CodeBullet, Dani or Fireship: expressive, comedic, quick reactions.\n\
         Pick poses for the numbered narration lines from this pose library:\n\n",
    );
    s += &library_text(poses);
    s += "\nRules:\n\
          - Use only pose tags from the library, spelled exactly as listed (the first name on each line).\n\
          - Change pose on emotional beats: jokes, failures, surprises, wins, questions, explanations. Roughly one change every one to three lines; not every line needs one.\n\
          - Never pick the pose that is already showing.\n\
          - at_word: ONE word from that line (copied exactly) where the pose switches on, e.g. the word that carries the joke or surprise; use \"\" to switch at the start of the line.\n\
          - reason: a few words (at most 8).\n\
          - Only suggest for lines listed under \"Lines to tag\".\n\
          Answer with JSON only: {\"suggestions\": [{\"line\": 3, \"pose\": \"<tag>\", \"at_word\": \"<word or empty>\", \"reason\": \"...\"}]}";
    if !style.trim().is_empty() {
        s += &format!("\n\nDirection from the narrator: {}", style.trim());
    }
    s
}

/// One schema branch per line to tag: the line number is fixed and `at_word`
/// can only be one of that line's words (or "" for its start), so a server that
/// enforces schemas can't produce a line or word that doesn't exist.
fn schema(poses: &PoseSet, targets: &[(usize, &Line)]) -> Value {
    let tags: Vec<&str> = poses.poses.iter().map(|p| p.tag.as_str()).collect();
    let branches: Vec<Value> = targets
        .iter()
        .map(|(n, l)| {
            let mut words: Vec<String> = vec![String::new()];
            for w in &l.words {
                if !words.contains(w) {
                    words.push(w.clone());
                }
            }
            json!({
                "type": "object",
                "properties": {
                    "line": {"type": "integer", "const": n},
                    "pose": {"type": "string", "enum": tags},
                    "at_word": {"type": "string", "enum": words},
                    "reason": {"type": "string", "maxLength": 60}
                },
                "required": ["line", "pose", "at_word", "reason"],
                "additionalProperties": false
            })
        })
        .collect();
    json!({
        "type": "object",
        "properties": {"suggestions": {"type": "array", "items": {"anyOf": branches}, "maxItems": targets.len()}},
        "required": ["suggestions"],
        "additionalProperties": false
    })
}

/// Resolve a pose name the model wrote: exact tag or alias, a normalised
/// form (`Face Palm` → `face_palm`), or the closest name.
fn resolve_pose(poses: &PoseSet, name: &str) -> Option<(usize, bool)> {
    if let Some(i) = poses.find(name) {
        return Some((i, false));
    }
    let n: String = name.trim().trim_matches(['[', ']', '`', '"']).to_lowercase().chars().map(|c| if c.is_alphanumeric() { c } else { '_' }).collect();
    let n = n.trim_matches('_');
    if let Some(i) = poses.find(n) {
        return Some((i, true));
    }
    let mut best = (0.0, 0);
    for (i, p) in poses.poses.iter().enumerate() {
        for cand in std::iter::once(&p.tag).chain(&p.aliases) {
            let s = strsim::jaro_winkler(n, cand);
            if s > best.0 {
                best = (s, i);
            }
        }
    }
    (best.0 >= 0.85).then_some((best.1, true))
}

/// Find the word the model named in a line: exact, then prefix/contained, then closest.
fn find_word(words: &[String], w: &str) -> Option<(usize, bool)> {
    let target = norm(w.split_whitespace().next().unwrap_or(""));
    if target.is_empty() {
        return Some((0, false));
    }
    let ws: Vec<String> = words.iter().map(|w| norm(w)).collect();
    if let Some(i) = ws.iter().position(|x| *x == target) {
        return Some((i, false));
    }
    if let Some(i) = ws.iter().position(|x| !x.is_empty() && (x.starts_with(&target) || target.starts_with(x.as_str()) && x.len() >= 3)) {
        return Some((i, true));
    }
    let (s, i) = ws.iter().enumerate().map(|(i, x)| (strsim::jaro_winkler(x, &target), i)).fold((0.0, 0), |a, b| if b.0 > a.0 { b } else { a });
    (s >= 0.85).then_some((i, true))
}

/// Ask for suggestions. `progress(done, total)` counts batches; `cancel` stops
/// between requests.
pub fn suggest(
    client: &mut Client,
    script: &str,
    poses: &PoseSet,
    opts: &SuggestOptions,
    progress: &mut dyn FnMut(usize, usize),
    cancel: &dyn Fn() -> bool,
) -> Result<(Vec<Suggestion>, Vec<Note>)> {
    if poses.poses.is_empty() {
        bail!("the pose library is empty");
    }
    let sheet = CueSheet::parse(script).map_err(|e| anyhow::anyhow!("the script has an error: {e}"))?;
    let raw: Vec<&str> = script.lines().collect();
    let lines: Vec<Line> = sheet
        .lines
        .iter()
        .map(|l| Line {
            line_no: l.line_no,
            words: line_words(raw.get(l.line_no - 1).copied().unwrap_or("")).into_iter().map(|w| w.1.to_string()).collect(),
            tags: l.items.iter().filter_map(|it| if let Item::Tag(t) = it { (t.name != "hide" && t.name != "show").then(|| t.name.clone()) } else { None }).collect(),
        })
        .filter(|l| !l.words.is_empty())
        .collect();
    if lines.is_empty() {
        bail!("the script has no lines to tag");
    }
    let target = |l: &Line| opts.mode == Mode::Replace || l.tags.is_empty();
    let system = system_prompt(poses, &opts.style);
    let mut notes = vec![];
    let mut raw_items: Vec<(usize, Value)> = vec![]; // (line index, item)
    let batches: Vec<std::ops::Range<usize>> = (0..lines.len()).step_by(opts.batch.max(1)).map(|a| a..(a + opts.batch.max(1)).min(lines.len())).collect();
    let mut showing: Option<String> = sheet.start_pose.clone();
    for (bi, range) in batches.iter().enumerate() {
        progress(bi, batches.len());
        if cancel() {
            bail!("cancelled");
        }
        let (first, last) = (range.start + 1, range.end);
        if !lines[range.clone()].iter().any(target) {
            // Nothing to tag here; just follow the poses along.
            for l in &lines[range.clone()] {
                if let Some(t) = l.tags.last() {
                    showing = Some(t.clone());
                }
            }
            continue;
        }
        let mut user = String::new();
        if range.start > 0 {
            user += "Earlier lines, for context:\n";
            for (i, l) in lines.iter().enumerate().take(range.start).skip(range.start.saturating_sub(3)) {
                user += &format!("{}: {}\n", i + 1, l.words.join(" "));
            }
            user.push('\n');
        }
        user += &format!("Pose showing before line {first}: {}\n\nLines to tag:\n", showing.as_deref().unwrap_or("none yet"));
        for (i, l) in lines.iter().enumerate().take(range.end).skip(range.start) {
            if target(l) {
                user += &format!("{}: {}\n", i + 1, l.words.join(" "));
            } else {
                user += &format!("{}: {}   (already tagged {}; leave it)\n", i + 1, l.words.join(" "), l.tags.join(", "));
            }
        }
        if range.start == 0 {
            user += "\nThe first line opens the video.";
        }
        let targets: Vec<(usize, &Line)> = lines.iter().enumerate().take(range.end).skip(range.start).filter(|(_, l)| target(l)).map(|(i, l)| (i + 1, l)).collect();
        let schema = schema(poses, &targets);
        let mut messages = vec![json!({"role": "system", "content": system}), json!({"role": "user", "content": user})];
        // Room for a reasoning model to think, then about 60 tokens a line.
        let max_tokens = (range.len() as u32) * 60 + 1500;
        // Ask, and if the JSON is broken, show the model its mistake (twice at most).
        let mut parsed = None;
        for attempt in 0..3 {
            let answer = match client.chat(&messages, Some(&schema), max_tokens, opts.temperature) {
                Ok(a) => a,
                Err(e) if attempt < 2 => {
                    notes.push(Note { line_no: None, text: format!("lines {first}–{last}: {e:#}; retrying"), fixed: true });
                    continue;
                }
                Err(e) => return Err(e),
            };
            match parse_lenient(&answer) {
                Ok(v) => {
                    if attempt > 0 {
                        notes.push(Note { line_no: None, text: format!("lines {first}–{last}: the answer was repaired after {attempt} retr{}", if attempt == 1 { "y" } else { "ies" }), fixed: true });
                    }
                    parsed = Some(v);
                    break;
                }
                Err(why) => {
                    messages.push(json!({"role": "assistant", "content": answer}));
                    messages.push(json!({"role": "user", "content": format!("That answer can't be used: {why}. Reply again with only the JSON object, exactly in this shape: {{\"suggestions\": [{{\"line\": {first}, \"pose\": \"<tag>\", \"at_word\": \"\", \"reason\": \"...\"}}]}}")}));
                }
            }
        }
        let Some(v) = parsed else {
            notes.push(Note { line_no: None, text: format!("lines {first}–{last}: no usable answer after 3 tries; skipped"), fixed: false });
            continue;
        };
        // The list itself: under "suggestions", a bare array, or the first array found.
        let items = match &v {
            Value::Array(a) => Some(a.clone()),
            Value::Object(o) => o.get("suggestions").and_then(Value::as_array).cloned().or_else(|| o.values().find_map(|x| x.as_array().cloned())),
            _ => None,
        };
        let Some(items) = items else {
            notes.push(Note { line_no: None, text: format!("lines {first}–{last}: the answer had no list of suggestions; skipped"), fixed: false });
            continue;
        };
        for it in items {
            // "line": 12, "12" or "line 12".
            let n = it.get("line").and_then(|l| l.as_u64().or_else(|| l.as_str().and_then(|s| s.chars().filter(char::is_ascii_digit).collect::<String>().parse().ok())));
            match n.map(|n| n as usize) {
                Some(n) if n >= first && n <= last => raw_items.push((n - 1, it)),
                Some(n) => notes.push(Note { line_no: None, text: format!("a suggestion for line {n}, outside lines {first}–{last}, was dropped"), fixed: false }),
                None => notes.push(Note { line_no: None, text: format!("a suggestion without a line number was dropped: {it}"), fixed: false }),
            }
        }
        // Follow the pose along for the next batch's context.
        for (i, l) in lines.iter().enumerate().take(range.end).skip(range.start) {
            let last_suggested = raw_items.iter().rev().find(|(li, _)| *li == i).and_then(|(_, it)| it["pose"].as_str()).map(String::from);
            if let Some(p) = last_suggested.or_else(|| if target(l) { None } else { l.tags.last().cloned() }) {
                showing = Some(p);
            }
        }
    }
    progress(batches.len(), batches.len());

    // Check each suggestion against the script and the library.
    let mut out: Vec<Suggestion> = vec![];
    for (li, it) in raw_items {
        let l = &lines[li];
        if !target(l) {
            notes.push(Note { line_no: Some(l.line_no), text: format!("line {} already has pose tags; its suggestion was dropped", li + 1), fixed: false });
            continue;
        }
        let name = it.get("pose").and_then(Value::as_str).unwrap_or("");
        let Some((pose, fixed)) = resolve_pose(poses, name) else {
            notes.push(Note { line_no: Some(l.line_no), text: format!("line {}: there is no pose called \"{name}\"; dropped", li + 1), fixed: false });
            continue;
        };
        if fixed {
            notes.push(Note { line_no: Some(l.line_no), text: format!("line {}: \"{name}\" read as {}", li + 1, poses.poses[pose].tag), fixed: true });
        }
        let at = it.get("at_word").or_else(|| it.get("word")).and_then(Value::as_str).unwrap_or("");
        let (word, wfixed) = find_word(&l.words, at).unwrap_or_else(|| {
            notes.push(Note { line_no: Some(l.line_no), text: format!("line {}: \"{at}\" isn't in the line; the pose starts with the line", li + 1), fixed: true });
            (0, false)
        });
        if wfixed {
            notes.push(Note { line_no: Some(l.line_no), text: format!("line {}: \"{at}\" matched to \"{}\"", li + 1, l.words[word]), fixed: true });
        }
        let reason: String = it.get("reason").and_then(Value::as_str).unwrap_or("").chars().take(140).collect();
        out.push(Suggestion { line_no: l.line_no, line_text: l.words.join(" "), word, word_text: l.words.get(word).cloned().unwrap_or_default(), pose, reason });
    }
    out.sort_by_key(|s| (s.line_no, s.word));
    out.dedup_by(|b, a| a.line_no == b.line_no && a.word == b.word);
    // Drop suggestions for the pose that's already showing (walking the script in order).
    let mut showing: Option<usize> = sheet.start_pose.as_deref().and_then(|p| poses.find(p));
    let mut kept = vec![];
    let mut si = out.into_iter().peekable();
    for l in &lines {
        if !target(l)
            && let Some(t) = l.tags.last() {
                showing = poses.find(t);
            }
        while si.peek().is_some_and(|s| s.line_no == l.line_no) {
            let s = si.next().unwrap();
            if Some(s.pose) == showing {
                notes.push(Note { line_no: Some(s.line_no), text: format!("{} is already showing at line {}; dropped", poses.poses[s.pose].tag, lines.iter().position(|x| x.line_no == s.line_no).unwrap_or(0) + 1), fixed: false });
                continue;
            }
            showing = Some(s.pose);
            kept.push(s);
        }
    }
    Ok((kept, notes))
}

/// Write the suggestions into the script.
pub fn apply(script: &str, suggestions: &[Suggestion], poses: &PoseSet, mode: Mode) -> Result<String> {
    let placements: Vec<Placement> = suggestions.iter().map(|s| Placement { line_no: s.line_no, word: s.word, tag: poses.poses[s.pose].short_name().to_string() }).collect();
    set_pose_tags(script, &placements, mode == Mode::Replace).map_err(|e| anyhow::anyhow!("{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use vg_core::Pose;

    fn lib() -> PoseSet {
        let p = |t: &str, a: &[&str]| Pose { tag: t.into(), aliases: a.iter().map(|s| s.to_string()).collect(), ..Default::default() };
        PoseSet { poses: vec![p("friendly_wave", &["wave"]), p("facepalm", &[]), p("victory", &["win"])], base: Default::default() }
    }

    #[test]
    fn resolves_sloppy_names_and_words() {
        let l = lib();
        assert_eq!(resolve_pose(&l, "facepalm"), Some((1, false)));
        assert_eq!(resolve_pose(&l, "wave"), Some((0, false)));
        assert_eq!(resolve_pose(&l, "Face Palm"), Some((1, true)));
        assert_eq!(resolve_pose(&l, "[victroy]"), Some((2, true)));
        assert_eq!(resolve_pose(&l, "dancing"), None);
        let words: Vec<String> = "It crashed, again!".split(' ').map(String::from).collect();
        assert_eq!(find_word(&words, "crashed"), Some((1, false)));
        assert_eq!(find_word(&words, "Again"), Some((2, false)));
        assert_eq!(find_word(&words, "crash"), Some((1, true)));
        assert_eq!(find_word(&words, ""), Some((0, false)));
        assert_eq!(find_word(&words, "banana"), None);
    }

    #[test]
    fn applies_with_short_names() {
        let l = lib();
        let s = |line_no, word, pose| Suggestion { line_no, line_text: String::new(), word, word_text: String::new(), pose, reason: String::new() };
        let out = apply("[0:00] Hey all\n[0:02] It crashed\n", &[s(1, 0, 0), s(2, 1, 1)], &l, Mode::Untagged).unwrap();
        assert_eq!(out, "[0:00] [wave] Hey all\n[0:02] It [facepalm] crashed\n");
    }
}
