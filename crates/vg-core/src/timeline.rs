//! Resolving a cue sheet against a pose set into timed events.

use crate::cues::{CueError, CueSheet, Item, TagTime};
use crate::poses::PoseSet;
use crate::transcript::{Word, word_time};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    Pose(usize),
    Hide,
    Show,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub t: f64,
    pub action: Action,
    /// Source line, so the GUI can jump from a timeline marker to the script.
    pub line_no: usize,
    /// Byte range of the tag within its source line (see [`crate::cues::Tag::range`]).
    pub range: (usize, usize),
    /// Whether the tag carries an explicit `@time` (vs. being placed by its position in the text).
    pub timed: bool,
}

/// A span of narration text (one cue line), used for captions and the GUI's script lane.
#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    pub start: f64,
    pub end: f64,
    pub text: String,
    pub line_no: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Timeline {
    pub duration: f64,
    pub initial_pose: usize,
    /// Sorted by time.
    pub events: Vec<Event>,
    pub spans: Vec<Span>,
}

impl Timeline {
    /// `duration` is used when the sheet has no `@duration`; pass the audio length
    /// when there is audio, else `None` to end a few seconds after the last cue.
    pub fn build(sheet: &CueSheet, poses: &PoseSet, duration: Option<f64>) -> Result<Self, Vec<CueError>> {
        Self::build_with_words(sheet, poses, duration, &[])
    }

    /// Like `build`; untimed mid-line tags land on the start of the next spoken word
    /// when `words` (from the transcript) covers the line, else by text position.
    pub fn build_with_words(sheet: &CueSheet, poses: &PoseSet, duration: Option<f64>, words: &[Word]) -> Result<Self, Vec<CueError>> {
        let mut errors = Vec::new();
        let line_times: Vec<Option<f64>> = sheet.lines.iter().map(|l| l.time).collect();
        let last_explicit = sheet
            .lines
            .iter()
            .flat_map(|l| l.time.into_iter().chain(l.items.iter().filter_map(|i| match i {
                Item::Tag(t) => match t.at {
                    Some(TagTime::Abs(a)) => Some(a),
                    _ => None,
                },
                _ => None,
            })))
            .fold(0.0_f64, f64::max);
        let duration = sheet
            .duration
            .or(duration)
            .unwrap_or_else(|| if sheet.lines.is_empty() { 5.0 } else { (last_explicit + 4.0).max(2.0 * sheet.lines.len() as f64) });
        let starts = fill_line_times(&line_times, duration);

        let initial_pose = match &sheet.start_pose {
            Some(name) => poses.resolve(name).unwrap_or_else(|e| {
                errors.push(CueError { line_no: 0, message: format!("@start: {e}") });
                0
            }),
            None => 0,
        };

        let mut events = Vec::new();
        let mut spans = Vec::new();
        for (i, line) in sheet.lines.iter().enumerate() {
            let start = starts[i];
            let end = starts.get(i + 1).copied().unwrap_or(duration).max(start);
            let total_chars: usize = line.items.iter().map(|it| if let Item::Text(t) = it { t.chars().count() } else { 0 }).sum();
            let mut chars_before = 0usize;
            let mut words_before = 0usize;
            for item in &line.items {
                match item {
                    Item::Text(t) => {
                        chars_before += t.chars().count();
                        words_before += t.split_whitespace().count();
                    }
                    Item::Tag(tag) => {
                        let t = match tag.at {
                            Some(TagTime::Abs(a)) => a,
                            Some(TagTime::Rel(r)) => start + r,
                            None if total_chars == 0 || words_before == 0 => start,
                            None => word_time(words, start, end, words_before)
                                .unwrap_or_else(|| start + (end - start) * chars_before as f64 / total_chars as f64),
                        };
                        let action = match tag.name.as_str() {
                            "hide" => Action::Hide,
                            "show" => Action::Show,
                            name => match poses.resolve(name) {
                                Ok(p) => Action::Pose(p),
                                Err(e) => {
                                    errors.push(CueError { line_no: line.line_no, message: e.to_string() });
                                    continue;
                                }
                            },
                        };
                        events.push(Event { t: t.max(0.0), action, line_no: line.line_no, range: tag.range, timed: tag.at.is_some() });
                    }
                }
            }
            let text = line.text();
            if !text.is_empty() {
                spans.push(Span { start, end, text, line_no: line.line_no });
            }
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        events.sort_by(|a, b| a.t.total_cmp(&b.t));
        // Without @start, the avatar holds the first tagged pose until that tag.
        let initial_pose = if sheet.start_pose.is_none() {
            events.iter().find_map(|e| if let Action::Pose(p) = e.action { Some(p) } else { None }).unwrap_or(initial_pose)
        } else {
            initial_pose
        };
        Ok(Timeline { duration, initial_pose, events, spans })
    }
}

/// Untimed lines are spread evenly between their timed neighbours (or 0 / `duration`).
fn fill_line_times(times: &[Option<f64>], duration: f64) -> Vec<f64> {
    let n = times.len();
    let mut out = vec![0.0; n];
    let mut i = 0;
    while i < n {
        if let Some(t) = times[i] {
            out[i] = t;
            i += 1;
            continue;
        }
        let prev = if i == 0 { None } else { Some(out[i - 1]) };
        let j = (i..n).find(|&j| times[j].is_some()).unwrap_or(n);
        let next = if j < n { times[j].unwrap() } else { duration };
        // A gap starting at the first line begins at 0; after a timed line, the
        // untimed ones share the interval with it.
        let (from, slots) = match prev {
            None => (0.0, j - i),
            Some(p) => (p, j - i + 1),
        };
        let step = (next - from).max(0.0) / slots as f64;
        for k in i..j {
            let idx = if prev.is_none() { k - i } else { k - i + 1 };
            out[k] = from + step * idx as f64;
        }
        i = j;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::poses::Pose;
    use std::path::PathBuf;

    fn poses() -> PoseSet {
        let mk = |t: &str| Pose { tag: t.into(), file: PathBuf::from("x.png"), size: (1, 1), anchor: (0.5, 1.0), scale: 1.0, ..Default::default() };
        PoseSet { poses: vec![mk("wave"), mk("excited"), mk("facepalm")], base: PathBuf::new() }
    }

    fn build(src: &str, dur: Option<f64>) -> Timeline {
        Timeline::build(&CueSheet::parse(src).unwrap(), &poses(), dur).unwrap()
    }

    #[test]
    fn fills_untimed_lines() {
        assert_eq!(fill_line_times(&[None, None], 10.0), vec![0.0, 5.0]);
        assert_eq!(fill_line_times(&[Some(2.0), None, Some(8.0)], 10.0), vec![2.0, 5.0, 8.0]);
        assert_eq!(fill_line_times(&[Some(4.0), None], 10.0), vec![4.0, 7.0]);
    }

    #[test]
    fn tag_times() {
        let tl = build("[0] [wave]aaaaa[excited]aaaaa\n[10] Next [facepalm@+1.5]\n[hide@12]", Some(20.0));
        let times: Vec<(f64, Action)> = tl.events.iter().map(|e| (e.t, e.action)).collect();
        // `[excited]` sits after 5 of 10 text chars in a 0..10 s line.
        assert_eq!(times, vec![(0.0, Action::Pose(0)), (5.0, Action::Pose(1)), (11.5, Action::Pose(2)), (12.0, Action::Hide)]);
        assert_eq!(tl.duration, 20.0);
        assert_eq!(tl.initial_pose, 0);
        assert_eq!(tl.spans.len(), 2);
        assert_eq!(tl.spans[0].end, 10.0);
    }

    #[test]
    fn mid_line_tags_snap_to_words() {
        let words: Vec<Word> = [(0.0, "so"), (0.4, "it"), (2.5, "exploded")]
            .iter()
            .map(|&(s, w)| Word { start: s, end: s + 0.3, word: w.into() })
            .collect();
        let sheet = CueSheet::parse("[0] so it [facepalm] exploded\n[5] next").unwrap();
        let tl = Timeline::build_with_words(&sheet, &poses(), Some(8.0), &words).unwrap();
        assert_eq!(tl.events[0].t, 2.5);
    }

    #[test]
    fn start_directive_and_first_tag() {
        assert_eq!(build("[2] hi [excited]", Some(5.0)).initial_pose, 1);
        assert_eq!(build("@start facepalm\n[2] hi [excited]", Some(5.0)).initial_pose, 2);
    }

    #[test]
    fn unknown_poses_are_all_reported() {
        let sheet = CueSheet::parse("[wavee]\n[facepalm] x [nope]").unwrap();
        let errs = Timeline::build(&sheet, &poses(), None).unwrap_err();
        assert_eq!(errs.len(), 2);
        assert_eq!(errs[0].line_no, 1);
        assert!(errs[0].message.contains("did you mean `wave`"));
        assert_eq!(errs[1].line_no, 2);
    }
}
