//! Text primitives the rule engine is built from.
//!
//! Ported from tokenjuice's `src/core/text.ts`. The behaviour that matters is
//! preserved exactly: which lines survive, what the omission marker says, and
//! how a count is pluralised. The one deliberate difference is character
//! counting — the original segments grapheme clusters with `Intl.Segmenter`,
//! which Rust's standard library does not have. Counting `char`s is within a
//! few percent for terminal output and, unlike a grapheme table, it cannot
//! drift between builds.

use std::sync::OnceLock;

use regex::Regex;

/// The marker a middle truncation leaves behind.
pub const MIDDLE_TRUNCATION_MARKER: &str = "\n... omitted ...\n";

fn ansi_osc() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)").unwrap())
}

fn ansi_csi() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\x1b\[[0-?]*[ -/]*[@-~]").unwrap())
}

fn ansi_osc_incomplete() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\x1b\][^\x07\x1b]*$").unwrap())
}

fn ansi_csi_incomplete() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\x1b\[[0-?]*[ -/]*$").unwrap())
}

fn ansi_single() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\x1b[@-_]").unwrap())
}

/// Remove every escape sequence, including the half-written one a killed
/// process leaves at the end of its output.
pub fn strip_ansi(text: &str) -> String {
    let out = ansi_osc().replace_all(text, "");
    let out = ansi_csi().replace_all(&out, "");
    let out = ansi_osc_incomplete().replace_all(&out, "");
    let out = ansi_csi_incomplete().replace_all(&out, "");
    let out = ansi_single().replace_all(&out, "");
    out.replace('\x1b', "")
}

/// Split into lines with CRLF normalised and trailing whitespace removed.
///
/// Trailing whitespace goes because a terminal pads, and the padding is pure
/// cost once the text is in a prompt.
pub fn normalize_lines(text: &str) -> Vec<String> {
    text.replace("\r\n", "\n")
        .split('\n')
        .map(|line| line.trim_end().to_string())
        .collect()
}

pub fn trim_empty_edges(lines: &[String]) -> Vec<String> {
    let mut start = 0;
    let mut end = lines.len();
    while start < end && lines[start].trim().is_empty() {
        start += 1;
    }
    while end > start && lines[end - 1].trim().is_empty() {
        end -= 1;
    }
    lines[start..end].to_vec()
}

pub fn dedupe_adjacent(lines: &[String]) -> Vec<String> {
    let mut next: Vec<String> = Vec::with_capacity(lines.len());
    for line in lines {
        if next.last() != Some(line) {
            next.push(line.clone());
        }
    }
    next
}

/// How many characters a string is, for the size comparisons the engine makes.
pub fn count_chars(text: &str) -> usize {
    text.chars().count()
}

/// Keep the head and the tail, and say how much was dropped.
///
/// The count in the marker is the whole point: the agent can tell a 12-line
/// omission from a 12,000-line one and decide whether to ask for more.
pub fn head_tail(lines: &[String], head: usize, tail: usize) -> Vec<String> {
    if head == 0 && tail == 0 {
        return lines.to_vec();
    }
    if lines.len() <= head + tail {
        return lines.to_vec();
    }
    let mut out: Vec<String> = Vec::with_capacity(head + tail + 1);
    out.extend_from_slice(&lines[..head]);
    out.push(format!(
        "... {} lines omitted ...",
        lines.len() - head - tail
    ));
    out.extend_from_slice(&lines[lines.len() - tail..]);
    out
}

/// `1 file`, `2 files`, `3 matches`, `2 passed`.
///
/// The `passed`/`failed`/`skipped` carve-out exists because those are already
/// past tense — "2 passeds" is the kind of thing that makes output look broken.
pub fn pluralize(count: usize, noun: &str) -> String {
    if noun.ends_with("passed") || noun.ends_with("failed") || noun.ends_with("skipped") {
        return format!("{count} {noun}");
    }
    if count == 1 {
        return format!("{count} {noun}");
    }
    let last = noun.chars().last().unwrap_or(' ');
    if matches!(last, 's' | 'x' | 'z') || noun.ends_with("sh") || noun.ends_with("ch") {
        return format!("{count} {noun}es");
    }
    let chars: Vec<char> = noun.chars().collect();
    if chars.len() >= 2 && chars[chars.len() - 1] == 'y' {
        let before = chars[chars.len() - 2];
        if !matches!(before, 'a' | 'e' | 'i' | 'o' | 'u') {
            let stem: String = chars[..chars.len() - 1].iter().collect();
            return format!("{count} {stem}ies");
        }
    }
    format!("{count} {noun}s")
}

/// Clip the middle, keeping 70% of the budget at the head.
///
/// The head is favoured because a command's first lines say what it is doing
/// and its last lines say how it ended; the middle is the repetition.
pub fn clamp_text_middle(text: &str, max_chars: usize) -> String {
    if count_chars(text) <= max_chars {
        return text.to_string();
    }
    let marker_chars = count_chars(MIDDLE_TRUNCATION_MARKER);
    let body_chars = max_chars.saturating_sub(marker_chars);
    let head_chars = (body_chars * 7).div_ceil(10);
    let tail_chars = body_chars.saturating_sub(head_chars);

    let head: String = text.chars().take(head_chars).collect();
    let head = trim_head_to_line_boundary(&head);
    let tail: String = text
        .chars()
        .rev()
        .take(tail_chars)
        .collect::<Vec<char>>()
        .into_iter()
        .rev()
        .collect();
    let tail = trim_tail_to_line_boundary(&tail);
    format!("{head}{MIDDLE_TRUNCATION_MARKER}{tail}")
}

fn trim_head_to_line_boundary(text: &str) -> String {
    match text.rfind('\n') {
        Some(index) if index >= text.len() / 2 => text[..index].to_string(),
        _ => text.to_string(),
    }
}

fn trim_tail_to_line_boundary(text: &str) -> String {
    match text.find('\n') {
        Some(index) if index <= text.len().div_ceil(2) => text[index + 1..].to_string(),
        _ => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ansi_is_stripped_including_a_half_written_sequence() {
        assert_eq!(strip_ansi("\x1b[31mred\x1b[0m"), "red");
        assert_eq!(strip_ansi("a\x1b]0;title\x07b"), "ab");
        // A process killed mid-escape leaves the introducer behind.
        assert_eq!(strip_ansi("done\x1b[3"), "done");
        assert_eq!(strip_ansi("done\x1b"), "done");
    }

    #[test]
    fn lines_are_normalised_and_right_trimmed() {
        // CRLF becomes LF, trailing whitespace goes, and the split keeps the
        // empty tail a trailing newline produces — the same shape the original
        // `split("\n")` gives, which the edge trimmer then removes.
        let lines = normalize_lines("a  \r\nb\t\r\n\r\n");
        assert_eq!(lines, vec!["a", "b", "", ""]);
        assert_eq!(trim_empty_edges(&lines), vec!["a", "b"]);
    }

    #[test]
    fn empty_edges_are_trimmed_but_interior_blanks_survive() {
        let lines: Vec<String> = ["", "  ", "a", "", "b", "", ""]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(trim_empty_edges(&lines), vec!["a", "", "b"]);
    }

    #[test]
    fn adjacent_duplicates_collapse_but_separated_ones_do_not() {
        let lines: Vec<String> = ["a", "a", "b", "a"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(dedupe_adjacent(&lines), vec!["a", "b", "a"]);
    }

    #[test]
    fn head_tail_says_how_many_lines_it_dropped() {
        let lines: Vec<String> = (0..100).map(|i| i.to_string()).collect();
        let out = head_tail(&lines, 3, 2);
        assert_eq!(out.len(), 6);
        assert_eq!(out[0], "0");
        assert_eq!(out[3], "... 95 lines omitted ...");
        assert_eq!(out[5], "99");
    }

    #[test]
    fn head_tail_leaves_a_short_list_alone() {
        let lines: Vec<String> = ["a", "b"].iter().map(|s| s.to_string()).collect();
        assert_eq!(head_tail(&lines, 6, 6), lines);
    }

    #[test]
    fn pluralize_matches_the_original_rules() {
        assert_eq!(pluralize(1, "file"), "1 file");
        assert_eq!(pluralize(2, "file"), "2 files");
        assert_eq!(pluralize(2, "match"), "2 matches");
        assert_eq!(pluralize(3, "box"), "3 boxes");
        assert_eq!(pluralize(2, "directory"), "2 directories");
        assert_eq!(pluralize(2, "day"), "2 days");
        // Already past tense: no suffix.
        assert_eq!(pluralize(2, "passed"), "2 passed");
        assert_eq!(pluralize(2, "failed"), "2 failed");
    }

    #[test]
    fn middle_clamping_keeps_both_ends() {
        let text: String = (0..200).map(|i| format!("line{i}\n")).collect();
        let out = clamp_text_middle(&text, 200);
        assert!(out.starts_with("line0"));
        assert!(out.contains("... omitted ..."));
        assert!(out.trim_end().ends_with("line199"));
    }

    #[test]
    fn a_string_under_the_budget_is_returned_untouched() {
        assert_eq!(clamp_text_middle("short", 100), "short");
    }
}
