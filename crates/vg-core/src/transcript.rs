//! Word-timed transcripts (from the Whisper sidecar) and turning them into cue sheets.

use crate::autotag;
use crate::cues::format_time;
use crate::poses::PoseSet;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Word {
    pub start: f64,
    pub end: f64,
    pub word: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    pub text: String,
    #[serde(default)]
    pub words: Vec<Word>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Transcript {
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub duration: f64,
    pub segments: Vec<Segment>,
}

impl Transcript {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        std::fs::write(path, serde_json::to_string(self)? + "\n").with_context(|| format!("writing {}", path.display()))
    }

    pub fn words(&self) -> Vec<Word> {
        self.segments.iter().flat_map(|s| s.words.iter().cloned()).collect()
    }
}

/// One cue line's worth of words.
#[derive(Clone, Debug, PartialEq)]
pub struct Sentence {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// Split words into short lines: at sentence punctuation, at pauses, and before lines get long.
pub fn sentences(words: &[Word]) -> Vec<Sentence> {
    const PAUSE: f64 = 0.7;
    const SOFT_MAX: usize = 12;
    const HARD_MAX: usize = 20;
    let mut out = Vec::new();
    let mut cur: Vec<&Word> = Vec::new();
    let flush = |cur: &mut Vec<&Word>, out: &mut Vec<Sentence>| {
        if let (Some(first), Some(last)) = (cur.first(), cur.last()) {
            let text = cur.iter().map(|w| w.word.trim()).collect::<Vec<_>>().join(" ");
            out.push(Sentence { start: first.start, end: last.end, text });
        }
        cur.clear();
    };
    for (i, w) in words.iter().enumerate() {
        cur.push(w);
        let t = w.word.trim_end_matches(['"', '\'', ')', '”', '’']);
        let ends_sentence = t.ends_with(['.', '!', '?', '…']);
        let clause = t.ends_with([',', ';', ':', '—']);
        let pause = words.get(i + 1).is_some_and(|n| n.start - w.end > PAUSE);
        if ends_sentence || pause || (clause && cur.len() >= SOFT_MAX) || cur.len() >= HARD_MAX {
            flush(&mut cur, &mut out);
        }
    }
    flush(&mut cur, &mut out);
    out
}

/// A cue sheet for this transcript, one line per sentence, with suggested pose tags.
pub fn cue_sheet(transcript: &Transcript, audio_rel: &str, words_rel: &str, poses: Option<&PoseSet>, source_note: &str) -> String {
    let lines = sentences(&transcript.words());
    let tags = match poses {
        Some(p) => autotag::suggest(&lines, p),
        None => vec![None; lines.len()],
    };
    let mut out = String::new();
    out += &format!("# {source_note}\n");
    out += "# One line per sentence. Tags like [facepalm] switch the pose; edit freely.\n";
    out += &format!("@audio {audio_rel}\n@words {words_rel}\n\n");
    for (s, tag) in lines.iter().zip(tags) {
        out += &format!("[{}] ", format_time(s.start));
        if let Some(tag) = tag {
            out += &format!("[{tag}] ");
        }
        out += &s.text;
        out.push('\n');
    }
    out
}

/// The words that belong to a line spoken in `[start, end)`: those starting inside it.
pub(crate) fn line_window(words: &[Word], start: f64, end: f64) -> std::ops::Range<usize> {
    let first = words.partition_point(|w| w.start < start - 0.05);
    let n = words[first..].iter().take_while(|w| w.start < end - 1e-6).count();
    first..first + n
}

/// Time of the `k`-th word of a line spoken in `[start, end)`, if the transcript has it.
pub fn word_time(words: &[Word], start: f64, end: f64, k: usize) -> Option<f64> {
    words[line_window(words, start, end)].get(k).map(|w| w.start)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(start: f64, end: f64, word: &str) -> Word {
        Word { start, end, word: word.into() }
    }

    #[test]
    fn splits_on_punctuation_and_pauses() {
        let words = vec![w(0.0, 0.2, "Hey"), w(0.2, 0.6, "everyone."), w(0.7, 0.9, "Wait"), w(2.0, 2.3, "what?")];
        let s = sentences(&words);
        assert_eq!(s.len(), 3);
        assert_eq!(s[0].text, "Hey everyone.");
        assert_eq!(s[1], Sentence { start: 0.7, end: 0.9, text: "Wait".into() });
        assert_eq!(s[2].start, 2.0);
    }

    #[test]
    fn long_runs_split() {
        let words: Vec<Word> = (0..25).map(|i| w(i as f64 * 0.2, i as f64 * 0.2 + 0.15, "word")).collect();
        let s = sentences(&words);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].text.split(' ').count(), 20);
    }

    #[test]
    fn word_times_within_a_line() {
        let words = vec![w(0.0, 0.2, "a"), w(0.5, 0.7, "b"), w(1.0, 1.2, "c"), w(3.0, 3.1, "d")];
        assert_eq!(word_time(&words, 0.0, 3.0, 2), Some(1.0));
        assert_eq!(word_time(&words, 0.0, 3.0, 3), None);
        assert_eq!(word_time(&words, 3.0, 9.0, 0), Some(3.0));
    }
}
