//! Rule-based pose suggestions for transcribed lines.
//!
//! Each rule matches words or phrases (or the line's closing punctuation) and
//! names poses in order of preference; the first pose that exists in the
//! library wins. The result is only a starting point for editing.

use crate::poses::PoseSet;
use crate::transcript::Sentence;

struct Rule {
    /// Single words (matched against the line's lowercased words) or phrases (contain a space).
    triggers: &'static [&'static str],
    /// Also matches when the line ends with this character.
    ends_with: Option<char>,
    /// Only look at the first few words (greetings).
    leading_only: bool,
    poses: &'static [&'static str],
}

const RULES: &[Rule] = &[
    Rule { triggers: &["hey", "hi", "hello", "welcome", "yo"], ends_with: None, leading_only: true, poses: &["wave", "salute"] },
    Rule { triggers: &["thanks", "thank"], ends_with: None, leading_only: false, poses: &["thank_you", "thumbs_up"] },
    Rule { triggers: &["subscribe", "like and", "comment"], ends_with: None, leading_only: false, poses: &["thumbs_up", "salute"] },
    Rule {
        triggers: &["bug", "bugs", "error", "errors", "crash", "crashed", "crashes", "broke", "broken", "exploded", "oh no", "segfault", "panic"],
        ends_with: None,
        leading_only: false,
        poses: &["debugging_panic", "panic", "surprised"],
    },
    Rule { triggers: &["ugh", "stupid", "dumb", "of course", "obviously", "why would"], ends_with: None, leading_only: false, poses: &["facepalm"] },
    Rule { triggers: &["phew", "relief", "fixed", "saved"], ends_with: None, leading_only: false, poses: &["relief", "thumbs_up"] },
    Rule {
        triggers: &["finally", "works", "worked", "working", "success", "won", "beat", "perfect", "nailed"],
        ends_with: None,
        leading_only: false,
        poses: &["victory", "excited_discovery", "thumbs_up"],
    },
    Rule { triggers: &["haha", "lol", "funny", "joke", "hilarious"], ends_with: None, leading_only: false, poses: &["laughing"] },
    Rule { triggers: &["wait", "what", "whoa", "huh", "somehow"], ends_with: None, leading_only: false, poses: &["surprised", "skeptical"] },
    Rule { triggers: &["amazing", "awesome", "incredible", "insane", "wow", "cool"], ends_with: Some('!'), leading_only: false, poses: &["excited_discovery", "excited", "victory"] },
    Rule { triggers: &["apparently", "supposedly", "or did it", "sure", "suspicious"], ends_with: None, leading_only: false, poses: &["skeptical"] },
    Rule { triggers: &["hmm", "think", "thinking", "wonder", "maybe", "figure"], ends_with: None, leading_only: false, poses: &["thinking", "deep_thinking"] },
    Rule { triggers: &["versus", "vs", "compared", "instead", "better", "worse"], ends_with: None, leading_only: false, poses: &["comparison"] },
    Rule { triggers: &["first", "second", "third", "three", "steps", "reasons"], ends_with: None, leading_only: false, poses: &["three_points"] },
    Rule { triggers: &["listen", "hear", "sounds"], ends_with: None, leading_only: false, poses: &["listening"] },
    Rule { triggers: &["so basically", "basically", "the idea", "idea", "because", "how it"], ends_with: None, leading_only: false, poses: &["explaining_idea", "presenting"] },
    Rule { triggers: &["here", "this is", "look", "presenting", "introducing"], ends_with: None, leading_only: false, poses: &["presenting", "explaining_idea"] },
    Rule { triggers: &[], ends_with: Some('?'), leading_only: false, poses: &["confused_shrug", "shrug", "skeptical"] },
];

/// Neutral poses cycled through when nothing has matched for a while, for variety.
const FILLERS: &[&str] = &["explaining_idea", "presenting", "listening", "thinking"];
/// Lines without a tag before a filler pose is added.
const FILLER_AFTER: usize = 3;

fn words_of(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'').to_lowercase())
        .filter(|w| !w.is_empty())
        .collect()
}

fn matches(rule: &Rule, words: &[String], text: &str) -> bool {
    let joined = format!(" {} ", words.join(" "));
    let scope = if rule.leading_only { &words[..words.len().min(3)] } else { words };
    let hit = rule.triggers.iter().any(|t| if t.contains(' ') { joined.contains(&format!(" {t} ")) } else { scope.iter().any(|w| w == t) });
    hit || rule.ends_with.is_some_and(|c| text.trim_end().ends_with(c))
}

/// One suggested tag (the pose's shortest name) per line, or `None`.
pub fn suggest(lines: &[Sentence], poses: &PoseSet) -> Vec<Option<String>> {
    let mut out = Vec::with_capacity(lines.len());
    let mut current: Option<usize> = None;
    let mut since_tag = 0usize;
    let mut filler = 0usize;
    for (i, line) in lines.iter().enumerate() {
        let words = words_of(&line.text);
        // The first matching rule whose poses exist decides; if it wants the pose
        // that's already showing, the line keeps it (no tag).
        let matched = RULES
            .iter()
            .filter(|r| !(r.leading_only && i > 1))
            .filter(|r| matches(r, &words, &line.text))
            .find_map(|r| r.poses.iter().find_map(|n| poses.find(n)));
        if matched.is_some() && matched == current {
            since_tag = 0;
            out.push(None);
            continue;
        }
        let mut pick = matched;
        if pick.is_none() && since_tag >= FILLER_AFTER {
            for k in 0..FILLERS.len() {
                if let Some(p) = poses.find(FILLERS[(filler + k) % FILLERS.len()]).filter(|&p| Some(p) != current) {
                    filler += k + 1;
                    pick = Some(p);
                    break;
                }
            }
        }
        if i == 0 && pick.is_none() {
            pick = poses.find("wave").or(if poses.poses.is_empty() { None } else { Some(0) });
        }
        match pick {
            Some(p) => {
                current = Some(p);
                since_tag = 0;
                out.push(Some(poses.poses[p].short_name().to_string()));
            }
            None => {
                since_tag += 1;
                out.push(None);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::poses::Pose;
    use std::path::PathBuf;

    fn poses() -> PoseSet {
        let mk = |t: &str, a: &[&str]| Pose {
            tag: t.into(),
            aliases: a.iter().map(|s| s.to_string()).collect(),
            file: PathBuf::from("x.png"),
            size: (1, 1),
            anchor: (0.5, 1.0),
            scale: 1.0,
            ..Default::default()
        };
        PoseSet {
            poses: vec![
                mk("friendly_wave", &["wave"]),
                mk("explaining_idea", &["idea"]),
                mk("debugging_panic", &["panic"]),
                mk("victory", &[]),
                mk("surprised", &[]),
                mk("thinking", &[]),
                mk("thank_you", &["thank"]),
                mk("confused_shrug", &["shrug"]),
            ],
            base: PathBuf::new(),
        }
    }

    fn line(text: &str) -> Sentence {
        Sentence { start: 0.0, end: 1.0, text: text.into() }
    }

    #[test]
    fn suggestions() {
        let lines: Vec<Sentence> = [
            "Hey everyone, welcome back.",
            "Wait.",
            "Oh no.",
            "There is a bug in the engine.",
            "Hmm.",
            "Anyway it finally works.",
            "Is that right?",
            "Thanks for watching.",
        ]
        .into_iter()
        .map(line)
        .collect();
        let got = suggest(&lines, &poses());
        let got: Vec<Option<&str>> = got.iter().map(|s| s.as_deref()).collect();
        // No back-to-back repeats: the second panic line gets nothing.
        assert_eq!(got, vec![Some("wave"), Some("surprised"), Some("panic"), None, Some("thinking"), Some("victory"), Some("shrug"), Some("thank")]);
    }

    #[test]
    fn fillers_after_quiet_stretch() {
        let lines: Vec<Sentence> = ["Hey.", "One.", "Two.", "Three.", "Four."].into_iter().map(line).collect();
        let got = suggest(&lines, &poses());
        assert_eq!(got[0].as_deref(), Some("wave"));
        assert_eq!(&got[1..4], &[None, None, None]);
        assert_eq!(got[4].as_deref(), Some("idea"));
    }
}
