//! Lenient JSON: find and repair the JSON in what an LLM actually wrote.

use serde_json::Value;

/// Parse the JSON value in `text`, forgiving the usual mistakes: a thinking
/// block, code fences, prose around it, trailing commas, smart quotes, Python
/// literals, or an answer cut off mid-way (unclosed brackets). The error says
/// what was wrong, for a repair request.
pub fn parse_lenient(text: &str) -> Result<Value, String> {
    let mut t = text.to_string();
    // Reasoning models: drop <think>…</think>.
    while let Some(a) = t.find("<think>") {
        match t[a..].find("</think>") {
            Some(b) => t.replace_range(a..a + b + "</think>".len(), ""),
            None => t.truncate(a),
        }
    }
    let t = t.replace("```json", "").replace("```JSON", "").replace("```", "");
    if let Ok(v) = serde_json::from_str::<Value>(t.trim()) {
        return Ok(v);
    }
    let start = t.find(['{', '[']).ok_or_else(|| "there is no JSON object in the answer".to_string())?;
    let body = &t[start..];
    let first_err = match serde_json::from_str::<Value>(balanced(body).unwrap_or(body)) {
        Ok(v) => return Ok(v),
        Err(e) => e.to_string(),
    };
    let fixed = repair(balanced(body).unwrap_or(body));
    if let Ok(v) = serde_json::from_str::<Value>(&fixed) {
        return Ok(v);
    }
    // Cut off mid-way: close what's open.
    let closed = close_open(&repair(body));
    serde_json::from_str::<Value>(&closed).map_err(|_| format!("the JSON is invalid ({first_err})"))
}

/// Tracks whether a character-by-character scan is inside a JSON string.
#[derive(Default)]
struct Strings {
    in_str: bool,
    esc: bool,
}

impl Strings {
    /// Feed the next character; `true` if it's part of a string (quotes included).
    fn inside(&mut self, c: char) -> bool {
        if self.in_str {
            match (self.esc, c) {
                (true, _) => self.esc = false,
                (false, '\\') => self.esc = true,
                (false, '"') => self.in_str = false,
                _ => {}
            }
            return true;
        }
        self.in_str = c == '"';
        self.in_str
    }
}

/// The prefix of `s` up to the bracket that closes the first one.
fn balanced(s: &str) -> Option<&str> {
    let mut depth = 0i32;
    let mut strings = Strings::default();
    for (i, c) in s.char_indices() {
        if strings.inside(c) {
            continue;
        }
        match c {
            '{' | '[' => depth += 1,
            '}' | ']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Fix common slips outside strings: smart quotes, trailing commas, Python literals.
fn repair(s: &str) -> String {
    let s = s.replace(['\u{201c}', '\u{201d}'], "\"").replace(['\u{2018}', '\u{2019}'], "'");
    let mut out = String::with_capacity(s.len());
    let mut strings = Strings::default();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if strings.inside(c) {
            out.push(c);
            i += 1;
            continue;
        }
        // A comma followed only by whitespace and a closing bracket.
        if c == ',' {
            let next = chars[i + 1..].iter().find(|c| !c.is_whitespace());
            if matches!(next, Some('}') | Some(']')) {
                i += 1;
                continue;
            }
        }
        let rest: String = chars[i..chars.len().min(i + 5)].iter().collect();
        let word = |w: &str| rest.starts_with(w) && !chars.get(i + w.len()).is_some_and(|c| c.is_alphanumeric());
        if word("True") {
            out += "true";
            i += 4;
            continue;
        }
        if word("False") {
            out += "false";
            i += 5;
            continue;
        }
        if word("None") {
            out += "null";
            i += 4;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Close an answer that stopped early: drop a dangling partial entry, then
/// close the open string and brackets.
fn close_open(s: &str) -> String {
    let mut stack = vec![];
    let mut strings = Strings::default();
    // Where the last closing bracket ended, and what was still open there.
    let (mut last_good, mut open_there) = (0, vec![]);
    for (i, c) in s.char_indices() {
        if strings.inside(c) {
            continue;
        }
        match c {
            '{' | '[' => stack.push(c),
            '}' | ']' => {
                stack.pop();
                last_good = i + 1;
                open_there = stack.clone();
            }
            _ => {}
        }
    }
    if stack.is_empty() {
        return s.to_string();
    }
    // Keep up to the last closed entry, then close everything still open there.
    let mut out = s[..last_good].to_string();
    while let Some(open) = open_there.pop() {
        out.push(if open == '{' { '}' } else { ']' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forgives_the_usual_mistakes() {
        let ok = |t: &str| parse_lenient(t).unwrap();
        assert_eq!(ok(r#"{"a": 1}"#)["a"], 1);
        assert_eq!(ok("<think>hmm {not json}</think>\n```json\n{\"a\": [1, 2,],}\n```")["a"][1], 2);
        assert_eq!(ok("Sure! Here you go:\n{\"a\": True, \"b\": None} Hope that helps.")["a"], true);
        assert_eq!(ok("{\u{201c}a\u{201d}: \"it's, fine\"}")["a"], "it's, fine");
        // Cut off in the middle of the third entry.
        let v = ok(r#"{"suggestions": [{"line": 1, "pose": "wave"}, {"line": 2, "pose": "shrug"}, {"line": 3, "po"#);
        assert_eq!(v["suggestions"].as_array().unwrap().len(), 2);
        // Brackets and escaped quotes inside strings don't count.
        let v = ok(r#"{"a": [{"b": "x]}\""}, {"c": "y"#);
        assert_eq!(v["a"].as_array().unwrap().len(), 1);
        assert_eq!(v["a"][0]["b"], "x]}\"");
        assert!(parse_lenient("no json here").is_err());
    }
}
