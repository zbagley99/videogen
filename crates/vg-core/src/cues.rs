//! The cue sheet: a plain-text narration script with pose tags.
//!
//! ```text
//! # Teaching a car to drive          <- comments / headings are ignored
//! @audio narration.wav                <- directives (@audio, @words, @duration, @start)
//! [0:00.0] [wave] Hey everyone!       <- optional line time, then text with tags
//! [0:03.2] So I built a neural net. [excited] And it worked!
//! It immediately drove into a wall. [facepalm@0:09.5]
//! ```
//!
//! Tags are `[name]`, `[name@<time>]` (absolute) or `[name@+<secs>]` (relative to
//! the line). `[hide]` and `[show]` take the avatar off and back on screen.
//! A line time is `[<time>]` at the very start of a line, where `<time>` is
//! `ss.s`, `m:ss.s` or `h:mm:ss.s`.

use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq)]
pub enum TagTime {
    Abs(f64),
    Rel(f64),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tag {
    pub name: String,
    pub at: Option<TagTime>,
    /// Byte range of the whole `[...]` within its source line, so editors can rewrite it.
    pub range: (usize, usize),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Text(String),
    Tag(Tag),
}

#[derive(Clone, Debug, PartialEq)]
pub struct CueLine {
    /// 1-based line number in the source, for error messages and GUI highlighting.
    pub line_no: usize,
    pub time: Option<f64>,
    pub items: Vec<Item>,
}

impl CueLine {
    /// The spoken text with tags removed.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for item in &self.items {
            if let Item::Text(t) = item {
                out.push_str(t);
            }
        }
        out.split_whitespace().collect::<Vec<_>>().join(" ")
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CueSheet {
    pub audio: Option<PathBuf>,
    /// Word timings (JSON from `vg transcribe`), used to place mid-line tags on the right word.
    pub words: Option<PathBuf>,
    pub duration: Option<f64>,
    /// Pose shown before the first tag.
    pub start_pose: Option<String>,
    pub lines: Vec<CueLine>,
}

/// A problem on a specific line; the GUI underlines these.
#[derive(Clone, Debug, PartialEq)]
pub struct CueError {
    pub line_no: usize,
    pub message: String,
}

impl std::fmt::Display for CueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.line_no, self.message)
    }
}

impl std::error::Error for CueError {}

pub fn parse_time(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let mut total = 0.0;
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() > 3 {
        return None;
    }
    for (i, part) in parts.iter().enumerate() {
        let v: f64 = part.parse().ok()?;
        if v < 0.0 || !v.is_finite() || (i > 0 && v >= 60.0) {
            return None;
        }
        total = total * 60.0 + v;
    }
    Some(total)
}

/// `m:ss.ss`, or `h:mm:ss.ss` past an hour.
pub fn format_time(t: f64) -> String {
    let t = t.max(0.0);
    let h = (t / 3600.0).floor();
    let m = ((t - h * 3600.0) / 60.0).floor();
    let s = t - h * 3600.0 - m * 60.0;
    if h > 0.0 { format!("{h}:{m:02}:{s:05.2}") } else { format!("{m}:{s:05.2}") }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

impl CueSheet {
    pub fn parse(src: &str) -> Result<Self, CueError> {
        let mut sheet = CueSheet::default();
        for (i, raw) in src.lines().enumerate() {
            let line_no = i + 1;
            let err = |message: String| CueError { line_no, message };
            let line = raw.trim_end();
            let indent = line.len() - line.trim_start().len();
            let line = line.trim_start();
            if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
                continue;
            }
            if let Some(rest) = line.strip_prefix('@') {
                let (key, value) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
                // `@audio a.wav   # note`: a ` #` starts a trailing comment.
                let value = value.split(" #").next().unwrap_or("").trim();
                match key {
                    "audio" => sheet.audio = Some(PathBuf::from(value)),
                    "words" => sheet.words = Some(PathBuf::from(value)),
                    "duration" => {
                        sheet.duration = Some(parse_time(value).ok_or_else(|| err(format!("bad duration `{value}`")))?)
                    }
                    "start" => sheet.start_pose = Some(value.to_string()),
                    _ => return Err(err(format!("unknown directive `@{key}` (expected @audio, @words, @duration or @start)"))),
                }
                continue;
            }
            sheet.lines.push(parse_line(line, indent, line_no)?);
        }
        Ok(sheet)
    }

    /// Canonical text form (what `vg transcribe` writes and the GUI saves).
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        if let Some(a) = &self.audio {
            out += &format!("@audio {}\n", a.display());
        }
        if let Some(w) = &self.words {
            out += &format!("@words {}\n", w.display());
        }
        if let Some(d) = self.duration {
            out += &format!("@duration {}\n", format_time(d));
        }
        if let Some(p) = &self.start_pose {
            out += &format!("@start {p}\n");
        }
        if !out.is_empty() {
            out.push('\n');
        }
        for line in &self.lines {
            let mut s = String::new();
            if let Some(t) = line.time {
                s += &format!("[{}] ", format_time(t));
            }
            for item in &line.items {
                match item {
                    Item::Text(t) => s += t,
                    Item::Tag(tag) => {
                        s += &format!("[{}", tag.name);
                        match tag.at {
                            Some(TagTime::Abs(t)) => s += &format!("@{}", format_time(t)),
                            Some(TagTime::Rel(t)) => s += &format!("@+{t}"),
                            None => {}
                        }
                        s.push(']');
                    }
                }
            }
            out += s.trim_end();
            out.push('\n');
        }
        out
    }
}

/// Rewrite every absolute time in a cue sheet (line times and `[tag@time]`)
/// through `f`, leaving everything else (comments, spacing, relative times) as
/// written. Used when audio edits move the narration.
pub fn remap_times(src: &str, f: impl Fn(f64) -> f64) -> Result<String, CueError> {
    let sheet = CueSheet::parse(src)?;
    let by_line: std::collections::HashMap<usize, &CueLine> = sheet.lines.iter().map(|l| (l.line_no, l)).collect();
    let mut out = String::with_capacity(src.len());
    for (i, raw) in src.split_inclusive('\n').enumerate() {
        let Some(line) = by_line.get(&(i + 1)) else {
            out += raw;
            continue;
        };
        let mut edits: Vec<(usize, usize, String)> = vec![];
        if let Some(t) = line.time {
            let start = raw.len() - raw.trim_start().len();
            if let Some(close) = raw[start..].find(']') {
                edits.push((start, start + close + 1, format!("[{}]", format_time(f(t)))));
            }
        }
        for item in &line.items {
            if let Item::Tag(Tag { at: Some(TagTime::Abs(t)), range: (a, b), .. }) = item {
                let name = raw[*a + 1..*b - 1].split('@').next().unwrap_or("").trim();
                edits.push((*a, *b, format!("[{name}@{}]", format_time(f(*t)))));
            }
        }
        let mut line_out = raw.to_string();
        edits.sort_by_key(|e| std::cmp::Reverse(e.0));
        for (a, b, text) in edits {
            line_out.replace_range(a..b, &text);
        }
        out += &line_out;
    }
    Ok(out)
}

/// The spoken words of a raw script line with their byte offsets: the line time
/// and `[tags]` are skipped.
pub fn line_words(raw: &str) -> Vec<(usize, &str)> {
    let mut out = vec![];
    let bytes = raw.as_bytes();
    let mut i = 0;
    let mut first = true;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'[' {
            let end = raw[i..].find(']').map(|e| i + e + 1).unwrap_or(bytes.len());
            first = false;
            i = end;
            continue;
        }
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'[' {
            i += 1;
        }
        // A directive or comment line has no words.
        if first && (bytes[start] == b'@' || bytes[start] == b'#') {
            return vec![];
        }
        first = false;
        out.push((start, &raw[start..i]));
    }
    out
}

/// A pose tag to put in front of word `word` (0 = the start of the text) of
/// the line numbered `line_no` (1-based, as in [`CueLine::line_no`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Placement {
    pub line_no: usize,
    pub word: usize,
    pub tag: String,
}

/// Insert pose tags. With `replace`, the existing pose tags on script lines go
/// first (`[hide]` and `[show]` stay).
pub fn set_pose_tags(src: &str, placements: &[Placement], replace: bool) -> Result<String, CueError> {
    let sheet = CueSheet::parse(src)?;
    let lines: std::collections::HashMap<usize, &CueLine> = sheet.lines.iter().map(|l| (l.line_no, l)).collect();
    let mut out = String::with_capacity(src.len() + placements.len() * 12);
    for (i, raw) in src.split_inclusive('\n').enumerate() {
        let line_no = i + 1;
        let Some(line) = lines.get(&line_no) else {
            out += raw;
            continue;
        };
        let mut text = raw.to_string();
        if replace {
            let mut ranges: Vec<(usize, usize)> = line
                .items
                .iter()
                .filter_map(|it| match it {
                    Item::Tag(t) if t.name != "hide" && t.name != "show" => Some(t.range),
                    _ => None,
                })
                .collect();
            ranges.sort_by_key(|r| std::cmp::Reverse(r.0));
            for (a, b) in ranges {
                // Take one neighbouring space with the tag.
                let b2 = if text[b..].starts_with(' ') { b + 1 } else { b };
                let a2 = if b2 == b && a > 0 && text[..a].ends_with(' ') { a - 1 } else { a };
                text.replace_range(a2..b2, "");
            }
        }
        let mut here: Vec<&Placement> = placements.iter().filter(|p| p.line_no == line_no).collect();
        if !here.is_empty() {
            let words: Vec<usize> = line_words(&text).into_iter().map(|w| w.0).collect();
            here.sort_by_key(|p| std::cmp::Reverse(p.word));
            for p in here {
                let at = match words.get(p.word) {
                    Some(pos) => *pos,
                    // Past the last word: the end of the text (before the newline).
                    None => text.trim_end_matches(['\n', '\r']).trim_end().len(),
                };
                let tag = if at >= text.trim_end().len() { format!(" [{}]", p.tag) } else { format!("[{}] ", p.tag) };
                text.insert_str(at, &tag);
            }
        }
        out += &text;
    }
    Ok(out)
}

/// `indent` is the byte offset of `line` within the raw source line.
fn parse_line(line: &str, indent: usize, line_no: usize) -> Result<CueLine, CueError> {
    let err = |message: String| CueError { line_no, message };
    let mut rest = line;
    let offset_of = |s: &str| indent + (s.as_ptr() as usize - line.as_ptr() as usize);
    let mut time = None;
    // A leading bracket holding only a time is the line time.
    if let Some(inner) = rest.strip_prefix('[').and_then(|r| r.split_once(']'))
        && let Some(t) = parse_time(inner.0) {
            time = Some(t);
            rest = inner.1.trim_start();
        }
    let mut items = Vec::new();
    while !rest.is_empty() {
        match rest.find('[') {
            None => {
                items.push(Item::Text(rest.to_string()));
                break;
            }
            Some(open) => {
                if open > 0 {
                    items.push(Item::Text(rest[..open].to_string()));
                }
                let Some(close) = rest[open..].find(']') else {
                    return Err(err("unclosed `[`".into()));
                };
                let inner = rest[open + 1..open + close].trim();
                let (name, at) = match inner.split_once('@') {
                    None => (inner, None),
                    Some((n, t)) => {
                        let t = t.trim();
                        let at = if let Some(r) = t.strip_prefix('+') {
                            TagTime::Rel(r.parse().map_err(|_| err(format!("bad relative time `{t}`")))?)
                        } else {
                            TagTime::Abs(parse_time(t).ok_or_else(|| err(format!("bad time `{t}`")))?)
                        };
                        (n.trim(), Some(at))
                    }
                };
                if !valid_name(name) {
                    return Err(err(format!("bad tag `[{inner}]` (names use letters, digits, _ and -)")));
                }
                let start = offset_of(rest) + open;
                items.push(Item::Tag(Tag { name: name.to_lowercase(), at, range: (start, start + close + 1) }));
                rest = &rest[open + close + 1..];
            }
        }
    }
    Ok(CueLine { line_no, time, items })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_skip_times_and_tags() {
        let w: Vec<&str> = line_words("[0:03.2] So I [thumbs @ 0:12] built it. [x]").into_iter().map(|w| w.1).collect();
        assert_eq!(w, vec!["So", "I", "built", "it."]);
        assert!(line_words("@audio a.wav").is_empty());
    }

    #[test]
    fn pose_tags_go_in_front_of_words() {
        let src = "@audio a.wav\n[0:00] [wave] Hey everyone!\n[0:02] It crashed [hide] again.\n";
        let p = |line_no, word, tag: &str| Placement { line_no, word, tag: tag.into() };
        let out = set_pose_tags(src, &[p(2, 0, "casual"), p(3, 1, "facepalm"), p(3, 9, "shrug")], false).unwrap();
        // Right before the word, so the new tag is the one that takes effect.
        assert_eq!(out, "@audio a.wav\n[0:00] [wave] [casual] Hey everyone!\n[0:02] It [facepalm] crashed [hide] again. [shrug]\n");
        let out = set_pose_tags(src, &[p(3, 0, "panic")], true).unwrap();
        assert_eq!(out, "@audio a.wav\n[0:00] Hey everyone!\n[0:02] [panic] It crashed [hide] again.\n");
        assert!(CueSheet::parse(&out).is_ok());
    }

    #[test]
    fn remap_rewrites_only_absolute_times() {
        let src = "# intro\n@audio a.wav\n[0:04.0] Hi [Wave@0:05.00] there [x@+1.5]\n  [0:10] Bye [thumbs @ 0:12]\nno time [hide]\n";
        let out = remap_times(src, |t| t - 1.0).unwrap();
        assert_eq!(out, "# intro\n@audio a.wav\n[0:03.00] Hi [Wave@0:04.00] there [x@+1.5]\n  [0:09.00] Bye [thumbs@0:11.00]\nno time [hide]\n");
        assert_eq!(CueSheet::parse(&out).unwrap().lines.len(), 3);
    }

    #[test]
    fn times() {
        assert_eq!(parse_time("12.5"), Some(12.5));
        assert_eq!(parse_time("1:02.5"), Some(62.5));
        assert_eq!(parse_time("1:00:00"), Some(3600.0));
        assert_eq!(parse_time("1:75"), None);
        assert_eq!(parse_time("wave"), None);
        assert_eq!(format_time(62.5), "1:02.50");
        assert_eq!(format_time(3725.0), "1:02:05.00");
    }

    #[test]
    fn parses_lines_tags_and_directives() {
        let src = "# title\n@audio n.wav\n@start wave\n\n[0:03.2] So I built it. [excited] It worked!\nNo time here [facepalm@9.5] [shrug@+0.5]\n";
        let s = CueSheet::parse(src).unwrap();
        assert_eq!(s.audio, Some(PathBuf::from("n.wav")));
        assert_eq!(s.start_pose.as_deref(), Some("wave"));
        assert_eq!(s.lines.len(), 2);
        assert_eq!(s.lines[0].time, Some(3.2));
        assert_eq!(s.lines[0].line_no, 5);
        assert_eq!(s.lines[0].text(), "So I built it. It worked!");
        assert_eq!(s.lines[0].items[1], Item::Tag(Tag { name: "excited".into(), at: None, range: (24, 33) }));
        assert_eq!(s.lines[1].time, None);
        assert_eq!(s.lines[1].items[1], Item::Tag(Tag { name: "facepalm".into(), at: Some(TagTime::Abs(9.5)), range: (13, 27) }));
        assert_eq!(s.lines[1].items[3], Item::Tag(Tag { name: "shrug".into(), at: Some(TagTime::Rel(0.5)), range: (28, 40) }));
    }

    #[test]
    fn tag_ranges_index_the_raw_line() {
        let raw = "  [1.0] hi [wave@+0.5] there";
        let s = CueSheet::parse(raw).unwrap();
        let Item::Tag(t) = &s.lines[0].items[1] else { panic!() };
        assert_eq!(&raw[t.range.0..t.range.1], "[wave@+0.5]");
    }

    #[test]
    fn errors_carry_line_numbers() {
        let e = CueSheet::parse("ok\n[wave oops").unwrap_err();
        assert_eq!(e.line_no, 2);
        let e = CueSheet::parse("[bad tag!]").unwrap_err();
        assert!(e.message.contains("bad tag"));
        assert!(CueSheet::parse("@frobnicate 3").is_err());
    }

    #[test]
    fn readme_example_parses() {
        let src = "# Comments\n@audio narration.wav            # narration\n@words narration.words.json     # timings\n@duration 1:30   # len\n@start wave  # pose\n\n[0:00.00] [wave] Hey everyone, welcome back!\n[0:06.18] [surprised] Wait. [panic@0:06.62] Oh no.\nLines without a time. [shrug@+0.5]\n";
        let s = CueSheet::parse(src).unwrap();
        assert_eq!(s.audio, Some(PathBuf::from("narration.wav")));
        assert_eq!(s.words, Some(PathBuf::from("narration.words.json")));
        assert_eq!(s.duration, Some(90.0));
        assert_eq!(s.start_pose.as_deref(), Some("wave"));
        assert_eq!(s.lines.len(), 3);
    }

    #[test]
    fn round_trips() {
        let src = "@audio n.wav\n@start wave\n\n[0:03.20] So I built it. [excited] It worked!\nPlain [facepalm@0:09.50] line [shrug@+0.5]\n";
        let s = CueSheet::parse(src).unwrap();
        assert_eq!(s.to_text(), src);
        assert_eq!(CueSheet::parse(&s.to_text()).unwrap(), s);
    }
}
