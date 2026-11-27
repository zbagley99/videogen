//! Word-by-word captions: short chunks of words with the spoken word highlighted,
//! written as an `.ass` subtitle file (importable in Kdenlive, burnable by ffmpeg)
//! and queryable per frame for the GUI preview.

use crate::timeline::Timeline;
use crate::transcript::{Word, line_window};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct CaptionStyle {
    pub enabled: bool,
    pub font: String,
    /// Font size as a fraction of the canvas height.
    pub size: f32,
    /// Horizontal centre of the caption, as a fraction of the canvas width.
    pub x: f32,
    /// Vertical centre of the caption, as a fraction of the canvas height.
    pub y: f32,
    /// Widest a caption line gets before wrapping, as a fraction of the canvas width.
    pub width: f32,
    /// Most words shown at once.
    pub max_words: usize,
    pub uppercase: bool,
    /// `0xRRGGBB`.
    pub color: u32,
    /// Colour of the word being spoken (`0xRRGGBB`); same as `color` to disable.
    pub highlight: u32,
    /// Outline thickness as a fraction of the font size.
    pub outline: f32,
    /// Scale-in pop when a chunk appears (0 = none).
    pub pop: f32,
}

impl Default for CaptionStyle {
    fn default() -> Self {
        CaptionStyle {
            enabled: true,
            font: "Noto Sans".into(),
            size: 0.045,
            x: 0.5,
            y: 0.62,
            width: 0.88,
            max_words: 3,
            uppercase: true,
            color: 0xFFFFFF,
            highlight: 0xFFB300,
            outline: 0.09,
            pop: 0.12,
        }
    }
}

impl CaptionStyle {
    /// Subtitle-like captions for 16:9.
    pub fn long_form() -> Self {
        CaptionStyle { size: 0.055, y: 0.88, max_words: 7, uppercase: false, pop: 0.0, ..Default::default() }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CaptionWord {
    pub text: String,
    pub start: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Chunk {
    pub start: f64,
    pub end: f64,
    pub words: Vec<CaptionWord>,
}

/// The words of one cue line with the times they're spoken.
#[derive(Clone, Debug, PartialEq)]
pub struct LineWords {
    /// Index into `Timeline::spans`.
    pub span: usize,
    /// The line's words as written (punctuation kept), timed.
    pub words: Vec<Word>,
    /// `true` when the times come from the transcript; `false` when estimated
    /// (no transcript, or the line was edited so the words no longer match).
    pub from_transcript: bool,
}

/// Timed words per line: transcript timings where the line's words match them,
/// otherwise spread by character count over the spoken part of the line.
pub fn timed_words(tl: &Timeline, words: &[Word]) -> Vec<LineWords> {
    let mut out = Vec::new();
    for (si, span) in tl.spans.iter().enumerate() {
        let text: Vec<&str> = span.text.split_whitespace().filter(|w| !clean(w, false).is_empty()).collect();
        if text.is_empty() {
            continue;
        }
        let spoken = &words[line_window(words, span.start, span.end)];
        if spoken.len() == text.len() {
            let words = text.iter().zip(spoken).map(|(t, w)| Word { start: w.start, end: w.end, word: t.to_string() }).collect();
            out.push(LineWords { span: si, words, from_transcript: true });
            continue;
        }
        let (a, b) = match (spoken.first(), spoken.last()) {
            (Some(f), Some(l)) => (f.start, l.end),
            _ => (span.start, span.end),
        };
        let total: usize = text.iter().map(|w| w.chars().count() + 1).sum();
        let mut acc = 0usize;
        let starts: Vec<f64> = text
            .iter()
            .map(|w| {
                let t = a + (b - a) * acc as f64 / total as f64;
                acc += w.chars().count() + 1;
                t
            })
            .collect();
        let words = text
            .iter()
            .enumerate()
            .map(|(i, t)| Word { start: starts[i], end: starts.get(i + 1).copied().unwrap_or(b), word: t.to_string() })
            .collect();
        out.push(LineWords { span: si, words, from_transcript: false });
    }
    out
}

/// Replace the transcript words of `span` with `new` (e.g. after retiming in the
/// editor). Keeps `all` sorted; new starts are clamped into the line.
pub fn set_line_words(all: &mut Vec<Word>, span: &crate::timeline::Span, new: &[Word]) {
    let range = line_window(all, span.start, span.end);
    let clamped: Vec<Word> = new
        .iter()
        .map(|w| {
            let start = w.start.clamp(span.start, (span.end - 0.01).max(span.start));
            Word { start, end: w.end.max(start + 0.01), word: w.word.clone() }
        })
        .collect();
    all.splice(range, clamped);
    all.sort_by(|a, b| a.start.total_cmp(&b.start));
}

/// Caption chunks for a timeline (see [`timed_words`] for where times come from).
pub fn chunks(tl: &Timeline, words: &[Word], style: &CaptionStyle) -> Vec<Chunk> {
    chunks_from(&timed_words(tl, words), tl, style)
}

pub fn chunks_from(lines: &[LineWords], tl: &Timeline, style: &CaptionStyle) -> Vec<Chunk> {
    let mut out: Vec<Chunk> = Vec::new();
    for line in lines {
        let span = &tl.spans[line.span];
        let end = line.words.last().map(|w| w.end).unwrap_or(span.end);
        let mut cur: Vec<CaptionWord> = Vec::new();
        for (i, w) in line.words.iter().enumerate() {
            cur.push(CaptionWord { text: clean(&w.word, style.uppercase), start: w.start });
            let breaks = w.word.ends_with(['.', '!', '?', ',', ';', ':', '…']);
            if cur.len() >= style.max_words.max(1) || breaks || i + 1 == line.words.len() {
                let start = cur[0].start;
                let chunk_end = line.words.get(i + 1).map(|n| n.start).unwrap_or((end + 0.35).min(span.end.max(end)));
                out.push(Chunk { start, end: chunk_end.max(start + 0.05), words: std::mem::take(&mut cur) });
            }
        }
    }
    // Never overlap: a chunk ends when the next begins.
    for i in 1..out.len() {
        let next_start = out[i].start;
        if out[i - 1].end > next_start {
            out[i - 1].end = next_start;
        }
    }
    out
}

fn clean(word: &str, upper: bool) -> String {
    let w = word.trim_end_matches([',', '.', ';', ':']);
    if upper { w.to_uppercase() } else { w.to_string() }
}

/// The chunk on screen at `t` and the index of the highlighted word.
pub fn caption_at(chunks: &[Chunk], t: f64) -> Option<(&Chunk, usize)> {
    let i = chunks.partition_point(|c| c.start <= t);
    let c = chunks.get(i.checked_sub(1)?)?;
    if t >= c.end {
        return None;
    }
    let w = c.words.partition_point(|w| w.start <= t).saturating_sub(1);
    Some((c, w))
}

fn ass_time(t: f64) -> String {
    let cs = (t.max(0.0) * 100.0).round() as u64;
    format!("{}:{:02}:{:02}.{:02}", cs / 360_000, cs / 6000 % 60, cs / 100 % 60, cs % 100)
}

/// `0xRRGGBB` -> ASS `&HBBGGRR&`.
fn ass_color(rgb: u32) -> String {
    format!("&H{:02X}{:02X}{:02X}&", rgb & 0xFF, (rgb >> 8) & 0xFF, (rgb >> 16) & 0xFF)
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('{', "(").replace('}', ")")
}

/// A complete `.ass` file for `chunks` on a `width`×`height` canvas.
pub fn to_ass(chunks: &[Chunk], style: &CaptionStyle, width: u32, height: u32) -> String {
    let size = (style.size * height as f32).round().max(1.0);
    let outline = (style.outline * size).max(0.0);
    // Style colours carry an alpha byte: `&H00BBGGRR`.
    let primary = format!("&H00{}", &ass_color(style.color)[2..8]);
    let mut s = String::new();
    s += "[Script Info]\nScriptType: v4.00+\n";
    s += &format!("PlayResX: {width}\nPlayResY: {height}\nWrapStyle: 1\nScaledBorderAndShadow: yes\n\n");
    s += "[V4+ Styles]\n";
    s += "Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n";
    s += &format!(
        "Style: Caption,{},{size},{primary},&H000000FF,&H00000000,&H64000000,-1,0,0,0,100,100,0,0,1,{outline:.1},{:.1},5,{m},{m},0,1\n\n",
        style.font,
        outline * 0.4,
        m = (width as f32 * (1.0 - style.width.clamp(0.05, 1.0)) / 2.0).round()
    );
    s += "[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n";
    // libass wraps at PlayResX minus the style margins, wherever \pos puts the text.
    let pos = format!("\\an5\\pos({},{})", (style.x * width as f32).round(), (style.y * height as f32).round());
    for c in chunks {
        for (i, w) in c.words.iter().enumerate() {
            let start = if i == 0 { c.start } else { w.start };
            let end = c.words.get(i + 1).map(|n| n.start).unwrap_or(c.end);
            if end <= start {
                continue;
            }
            let mut text = String::new();
            for (j, word) in c.words.iter().enumerate() {
                if j > 0 {
                    text.push(' ');
                }
                if j == i && style.highlight != style.color {
                    text += &format!("{{\\c{}}}{}{{\\c{}}}", ass_color(style.highlight), escape(&word.text), ass_color(style.color));
                } else {
                    text += &escape(&word.text);
                }
            }
            let mut tags = pos.clone();
            if i == 0 && style.pop > 0.0 {
                let from = 100.0 * (1.0 + style.pop);
                tags += &format!("\\fscx{from:.0}\\fscy{from:.0}\\t(0,90,\\fscx100\\fscy100)");
            }
            s += &format!("Dialogue: 0,{},{},Caption,,0,0,0,,{{{tags}}}{text}\n", ass_time(start), ass_time(end));
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cues::CueSheet;
    use crate::poses::PoseSet;

    fn tl(src: &str) -> Timeline {
        Timeline::build(&CueSheet::parse(src).unwrap(), &PoseSet::default(), Some(10.0)).unwrap()
    }

    fn w(start: f64, word: &str) -> Word {
        Word { start, end: start + 0.3, word: word.into() }
    }

    #[test]
    fn chunks_follow_word_timings_and_punctuation() {
        let tl = tl("[0] Hey everyone, welcome back to it.\n[5] Next line");
        let words = vec![w(0.0, "Hey"), w(0.4, "everyone,"), w(1.0, "welcome"), w(1.5, "back"), w(1.9, "to"), w(2.2, "it.")];
        let style = CaptionStyle::default();
        let c = chunks(&tl, &words, &style);
        let texts: Vec<Vec<&str>> = c.iter().map(|c| c.words.iter().map(|w| w.text.as_str()).collect()).collect();
        assert_eq!(texts, vec![vec!["HEY", "EVERYONE"], vec!["WELCOME", "BACK", "TO"], vec!["IT"], vec!["NEXT", "LINE"]]);
        assert_eq!(c[0].end, 1.0);
        assert_eq!(c[1].words[1].start, 1.5);
        // The last spoken chunk lingers briefly after its word.
        assert!((c[2].end - 2.85).abs() < 1e-9);
        let (chunk, i) = caption_at(&c, 1.6).unwrap();
        assert_eq!((chunk.words[i].text.as_str()), "BACK");
        assert!(caption_at(&c, 4.0).is_none());
    }

    #[test]
    fn retimed_line_words_round_trip() {
        let tl = tl("[0] Hey there friend\n[5] Next");
        let mut words = vec![w(0.0, "Hey"), w(0.5, "there"), w(1.0, "friend"), w(5.0, "Next")];
        let mut line = timed_words(&tl, &words)[0].clone();
        assert!(line.from_transcript);
        line.words[1].start = 0.7;
        set_line_words(&mut words, &tl.spans[0], &line.words);
        assert_eq!(words.len(), 4);
        assert_eq!(words[1].start, 0.7);
        let c = chunks(&tl, &words, &CaptionStyle::default());
        assert_eq!(c[0].words[1].start, 0.7);
        // An edited line (word count changed) gets estimated times, and can be pinned.
        let tl2 = tl_edit();
        let est = timed_words(&tl2, &words);
        assert!(!est[0].from_transcript);
        set_line_words(&mut words, &tl2.spans[0], &est[0].words);
        assert!(timed_words(&tl2, &words)[0].from_transcript);
    }

    fn tl_edit() -> Timeline {
        tl("[0] Hey there my friend\n[5] Next")
    }

    #[test]
    fn ass_output() {
        let tl = tl("[0] Hi there");
        let c = chunks(&tl, &[], &CaptionStyle::default());
        let ass = to_ass(&c, &CaptionStyle::default(), 1080, 1920);
        assert!(ass.contains("PlayResX: 1080"));
        assert!(ass.contains("Dialogue: 0,0:00:00.00,"));
        assert!(ass.contains("{\\c&H00B3FF&}HI{\\c&HFFFFFF&} THERE"));
        assert_eq!(ass_time(3725.5), "1:02:05.50");
        assert!(ass.contains("\\an5\\pos(540,1190)"));
        assert!(ass.contains(",5,65,65,0,1\n"));
        assert!(ass.contains("Style: Caption,Noto Sans,86,&H00FFFFFF,"));
        let moved = CaptionStyle { x: 0.25, y: 0.5, width: 0.5, color: 0x112233, ..Default::default() };
        let ass = to_ass(&c, &moved, 1080, 1920);
        assert!(ass.contains("Style: Caption,Noto Sans,86,&H00332211,"));
        assert!(ass.contains("\\an5\\pos(270,960)"));
        assert!(ass.contains(",5,270,270,0,1\n"));
    }
}
