//! Splitting prose into addressable units.
//!
//! Ported from `sparseread/core/readers/text_units.py`. A unit is the smallest
//! thing the preview can cite and the agent can page to, so its anchor is a
//! line range — `L120-L168` — which is exactly what `fs/read_text_file`'s
//! `line` and `limit` parameters take. That correspondence is what makes the
//! card actionable instead of decorative.

use regex::Regex;
use std::sync::OnceLock;

const LONG_UNIT_CHARS: usize = 1600;
const LONG_UNIT_OVERLAP: usize = 180;

/// One heading and the lines it owns, so the card can offer a call per section
/// instead of only "read the whole file".
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Section {
    pub title: String,
    pub start_line: usize,
    pub end_line: usize,
}

impl Section {
    pub fn lines(&self) -> usize {
        self.end_line.saturating_sub(self.start_line) + 1
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextUnit {
    /// `L120-L168`, or `L120-L168:C0-1600` when a unit had to be split.
    pub anchor: String,
    pub text: String,
    pub heading: String,
    pub start_line: usize,
    pub end_line: usize,
}

fn list_item_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\s*(?:[-*]|\d+[.)])\s+\S").unwrap())
}

fn numeric_only_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[\d\s./:%-]+$").unwrap())
}

fn numbered_heading_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\d+(?:\.\d+)*[.)]?\s+\S").unwrap())
}

fn word_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[A-Za-z][A-Za-z&/+.-]*").unwrap())
}

pub fn looks_like_list_item(line: &str) -> bool {
    list_item_re().is_match(line)
}

/// A heading is a short, mostly-capitalised, low-digit line that does not end
/// in sentence punctuation. The heuristics are upstream's and they are tuned
/// for prose, not for code — which is fine, because code never reaches here.
pub fn looks_like_heading(stripped: &str) -> bool {
    if stripped.is_empty() || stripped.chars().count() > 140 {
        return false;
    }
    // A table row is data, not a heading. `| Key | Action |` is capitalised and
    // short, so the prose heuristic below would happily call it one — and a map
    // whose entries are table rows is a map of the wrong thing.
    if stripped.starts_with('|') || stripped.starts_with("```") || stripped.starts_with("~~~") {
        return false;
    }
    if stripped.contains("http://") || stripped.contains("https://") {
        return false;
    }
    if numeric_only_re().is_match(stripped) {
        return false;
    }
    let words: Vec<&str> = word_re().find_iter(stripped).map(|m| m.as_str()).collect();
    if words.is_empty() || words.len() > 18 {
        return false;
    }
    let digits = stripped.chars().filter(|ch| ch.is_ascii_digit()).count();
    if digits > std::cmp::max(3, stripped.chars().count() / 5) {
        return false;
    }
    if stripped.starts_with('#') {
        return true;
    }
    if numbered_heading_re().is_match(stripped) {
        return true;
    }
    let letters: String = stripped
        .chars()
        .filter(|ch| ch.is_alphabetic())
        .collect::<String>();
    if !letters.is_empty() && letters == letters.to_uppercase() && words.len() >= 2 {
        return true;
    }
    if stripped.ends_with(['.', '?', '!', ';']) {
        return false;
    }
    let capitalised = words
        .iter()
        .filter(|word| word.chars().next().is_some_and(|ch| ch.is_uppercase()))
        .count();
    capitalised >= std::cmp::max(1, words.len() - 1)
}

fn dedupe(items: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for item in items {
        let key = item.trim().to_string();
        if !key.is_empty() && seen.insert(key.clone()) {
            out.push(key);
        }
    }
    out
}

fn split_long_text(text: &str) -> Vec<(usize, usize, String)> {
    let chars: Vec<char> = text.chars().collect();
    let length = chars.len();
    let mut chunks = Vec::new();
    let mut start = 0usize;
    while start < length {
        let hard_end = std::cmp::min(length, start + LONG_UNIT_CHARS);
        let mut end = hard_end;
        if hard_end < length {
            let window: String = chars[start..hard_end].iter().collect();
            let best_sentence = [". ", "? ", "! "]
                .iter()
                .filter_map(|mark| window.rfind(mark))
                .max();
            match best_sentence {
                Some(position) if position >= LONG_UNIT_CHARS / 2 => end = start + position + 1,
                _ => {
                    if let Some(space) = window.rfind(' ') {
                        if space >= LONG_UNIT_CHARS / 2 {
                            end = start + space;
                        }
                    }
                }
            }
        }
        let chunk: String = chars[start..end].iter().collect();
        let chunk = chunk.trim().to_string();
        if !chunk.is_empty() {
            chunks.push((start, end, chunk));
        }
        if end >= length {
            break;
        }
        start = std::cmp::max(end.saturating_sub(LONG_UNIT_OVERLAP), start + 1);
    }
    chunks
}

fn push_units(
    units: &mut Vec<TextUnit>,
    start_line: usize,
    end_line: usize,
    raw: &str,
    heading: &str,
) {
    let anchor = format!("L{start_line}-L{end_line}");
    if raw.chars().count() <= LONG_UNIT_CHARS {
        units.push(TextUnit {
            anchor,
            text: raw.to_string(),
            heading: heading.to_string(),
            start_line,
            end_line,
        });
        return;
    }
    for (char_start, char_end, chunk) in split_long_text(raw) {
        units.push(TextUnit {
            anchor: format!("{anchor}:C{char_start}-{char_end}"),
            text: chunk,
            heading: heading.to_string(),
            start_line,
            end_line,
        });
    }
}

/// Segment prose into units and collect the heading skeleton alongside.
pub fn segment(text: &str) -> (Vec<TextUnit>, Vec<String>) {
    let lines: Vec<&str> = text.lines().collect();
    let mut units: Vec<TextUnit> = Vec::new();
    let mut skeleton: Vec<String> = Vec::new();
    let mut buffer: Vec<&str> = Vec::new();
    let mut start_line = 1usize;
    let mut heading = String::new();

    let flush = |units: &mut Vec<TextUnit>, buffer: &mut Vec<&str>, start: usize, end: usize, heading: &str| {
        let raw = buffer.join("\n");
        let raw = raw.trim();
        if !raw.is_empty() {
            push_units(units, start, end, raw, heading);
        }
        buffer.clear();
    };

    // A fenced block is code, and a line inside one is never a heading however
    // capitalised it looks. Tracking the fence is what keeps a shell snippet's
    // `# comment` from becoming a section of the map.
    let mut in_fence = false;

    for (index, line) in lines.iter().enumerate() {
        let number = index + 1;
        let stripped = line.trim();
        let fence = stripped.starts_with("```") || stripped.starts_with("~~~");
        if fence {
            in_fence = !in_fence;
        }
        if !fence && !in_fence && looks_like_heading(stripped) {
            flush(&mut units, &mut buffer, start_line, number.saturating_sub(1), &heading);
            heading = stripped.to_string();
            skeleton.push(stripped.to_string());
            start_line = number;
            buffer.push(line);
            continue;
        }
        if stripped.is_empty() {
            flush(&mut units, &mut buffer, start_line, number.saturating_sub(1), &heading);
            continue;
        }
        if looks_like_list_item(line) && !buffer.is_empty() && buffer.join("\n").chars().count() > 500
        {
            flush(&mut units, &mut buffer, start_line, number.saturating_sub(1), &heading);
        }
        if buffer.is_empty() {
            start_line = number;
        }
        buffer.push(line);
        if buffer.join("\n").chars().count() > 1200 {
            flush(&mut units, &mut buffer, start_line, number, &heading);
        }
    }
    flush(&mut units, &mut buffer, start_line, lines.len(), &heading);

    (units, dedupe(skeleton))
}

/// The heading skeleton with the line range each heading owns.
///
/// `segment` already tracks the heading a unit belongs to and the lines it
/// covers, so a section is just the run of consecutive units that share one.
/// This is what lets the card offer a call per section: a map with roads to each
/// place, not only to the front door.
pub fn sections(units: &[TextUnit]) -> Vec<Section> {
    let mut out: Vec<Section> = Vec::new();
    for unit in units {
        if unit.heading.is_empty() {
            continue;
        }
        match out.last_mut() {
            Some(last) if last.title == unit.heading => {
                last.end_line = last.end_line.max(unit.end_line);
            }
            _ => out.push(Section {
                title: unit.heading.clone(),
                start_line: unit.start_line,
                end_line: unit.end_line,
            }),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_heading_starts_a_new_unit_and_lands_in_the_skeleton() {
        let text = "# Title\n\nfirst paragraph\n\n## Section\n\nsecond paragraph\n";
        let (units, skeleton) = segment(text);
        assert_eq!(skeleton, vec!["# Title", "## Section"]);
        assert!(units.len() >= 2);
        assert!(units.iter().any(|unit| unit.heading == "## Section"));
    }

    #[test]
    fn an_anchor_is_a_line_range_the_read_tool_accepts() {
        let text = "# Title\n\nbody\n";
        let (units, _) = segment(text);
        let anchor = &units[0].anchor;
        assert!(anchor.starts_with('L'), "anchor was {anchor}");
        assert!(anchor.contains("-L"));
    }

    #[test]
    fn a_sentence_is_not_a_heading() {
        assert!(!looks_like_heading("This is a normal sentence."));
        assert!(!looks_like_heading("Does it end in a question?"));
    }

    #[test]
    fn a_url_is_not_a_heading() {
        assert!(!looks_like_heading("https://example.com/Some/Path"));
    }

    #[test]
    fn a_numeric_line_is_not_a_heading() {
        assert!(!looks_like_heading("12 34.5 67%"));
    }

    #[test]
    fn a_markdown_heading_is_a_heading() {
        assert!(looks_like_heading("## Configuration"));
    }

    #[test]
    fn a_numbered_heading_is_a_heading() {
        assert!(looks_like_heading("3.2 Deployment"));
    }

    #[test]
    fn an_all_caps_line_of_two_words_is_a_heading() {
        assert!(looks_like_heading("RELEASE NOTES"));
    }

    #[test]
    fn a_bullet_is_a_list_item() {
        assert!(looks_like_list_item("- something"));
        assert!(looks_like_list_item("  1. something"));
        assert!(!looks_like_list_item("plain text"));
    }

    #[test]
    fn a_very_long_paragraph_is_split_with_overlap() {
        let paragraph = "word ".repeat(1200);
        let (units, _) = segment(&paragraph);
        assert!(units.len() > 1);
        assert!(units[0].anchor.contains(":C0-"));
    }

    #[test]
    fn the_skeleton_does_not_repeat_a_heading() {
        let text = "# Same\n\nbody\n\n# Same\n\nmore\n";
        let (_, skeleton) = segment(text);
        assert_eq!(skeleton.len(), 1);
    }
}
