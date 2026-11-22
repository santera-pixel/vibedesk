//! Deterministic observation hygiene, run before evidence selection.
//!
//! Ported from `sparseread/core/denoise.py`. The rule the upstream file states
//! and this port keeps: remove *carrier* noise — control bytes, HTML chrome,
//! repeated log lines, progress bars, volatile timestamp prefixes — and never
//! rank, summarise or delete unique semantic content. Everything here is
//! reversible in the sense that matters: a line that appears once survives
//! verbatim.
//!
//! Two upstream rules are not ported, and the plan doc records why: YAML flow
//! flattening needs a YAML parser, and XLSX previews need a zip reader. Both
//! are reported as gaps rather than approximated.

use regex::Regex;
use std::sync::OnceLock;

const MAX_BLOCK_LINES: usize = 32;
const MIN_BLOCK_LINES: usize = 2;

/// Below this the fixed cost of denoising exceeds anything it can save.
pub const MIN_CHARS: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DenoiseResult {
    pub text: String,
    pub input_chars: usize,
    pub output_chars: usize,
    pub rules: Vec<&'static str>,
}

impl DenoiseResult {
    pub fn changed(&self) -> bool {
        self.input_chars != self.output_chars || !self.rules.is_empty()
    }

    pub fn saved_chars(&self) -> usize {
        self.input_chars.saturating_sub(self.output_chars)
    }
}

fn ansi_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\x1b(?:\[[0-?]*[ -/]*[@-~]|\][^\x07]*(?:\x07|\x1b\\))").unwrap()
    })
}

fn progress_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?:\b\d{1,3}%|\b\d+\s*/\s*\d+\b|\[[#=>.\s-]{4,}\])").unwrap())
}

fn volatile_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?:\b\d{1,4}[-/:]\d{1,4}(?:[-/:]\d{1,4})?(?:[T ]\d{1,2}:\d{2}(?::\d{2}(?:\.\d+)?)?Z?)?\b|\b0x[0-9a-f]+\b|\b\d+(?:\.\d+)?\b)",
        )
        .unwrap()
    })
}

fn volatile_prefix_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"^(?:\[)?(?:\d{4}-\d{2}-\d{2}[T ]\d{1,2}:\d{2}(?::\d{2}(?:\.\d+)?)?(?:Z|[+-]\d{2}:?(?:\d{2})?)?|\d{4}-\d{2}-\d{2}|\d{1,2}:\d{2}(?::\d{2}(?:\.\d+)?)?)(?:\])?(?:\s+|$)",
        )
        .unwrap()
    })
}

fn whitespace_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\s+").unwrap())
}

fn blank_run_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\n{3,}").unwrap())
}

fn collapse_ws(text: &str) -> String {
    whitespace_re().replace_all(text, " ").into_owned()
}

/// Strip ANSI escapes, normalise line endings and drop control bytes.
fn clean_controls(text: &str) -> (String, bool) {
    let stripped = ansi_re().replace_all(text, "");
    let normalised = stripped.replace("\r\n", "\n").replace('\r', "\n");
    let cleaned: String = normalised
        .chars()
        .filter(|ch| {
            matches!(ch, '\n' | '\t')
                || (*ch as u32) >= 32
                    && !matches!(*ch, '\u{200b}' | '\u{200c}' | '\u{200d}' | '\u{feff}')
        })
        .collect();
    let changed = cleaned != text;
    (cleaned, changed)
}

/// Lines inside fenced code blocks or indented blocks are never folded: their
/// repetition is usually the point.
fn protected_lines(lines: &[&str]) -> Vec<bool> {
    let mut protected = Vec::with_capacity(lines.len());
    let mut fenced = false;
    for line in lines {
        let stripped = line.trim_start();
        if stripped.starts_with("```") || stripped.starts_with("~~~") {
            protected.push(true);
            fenced = !fenced;
            continue;
        }
        protected.push(fenced || line.starts_with("    ") || line.starts_with('\t'));
    }
    protected
}

fn progress_signature(line: &str) -> String {
    if !progress_re().is_match(line) {
        return String::new();
    }
    collapse_ws(&volatile_re().replace_all(line.trim().to_lowercase().as_str(), "#"))
}

fn join_compact(output: &[String]) -> String {
    blank_run_re()
        .replace_all(&output.join("\n"), "\n\n")
        .trim()
        .to_string()
}

/// Fold consecutive lines whose body is identical after a leading timestamp.
fn collapse_volatile_prefix(text: &str) -> (String, bool) {
    let lines: Vec<&str> = text.lines().collect();
    let protected = protected_lines(&lines);
    let mut output: Vec<String> = Vec::with_capacity(lines.len());
    let mut index = 0;
    let mut changed = false;

    while index < lines.len() {
        let line = lines[index];
        if protected[index] || line.trim().is_empty() {
            output.push(line.trim_end().to_string());
            index += 1;
            continue;
        }
        let Some(captures) = volatile_prefix_re().captures(line) else {
            output.push(line.trim_end().to_string());
            index += 1;
            continue;
        };
        let body = collapse_ws(line[captures.get(0).unwrap().end()..].trim());
        if body.is_empty() {
            output.push(line.trim_end().to_string());
            index += 1;
            continue;
        }

        let mut end = index + 1;
        while end < lines.len() && !protected[end] {
            let Some(next) = volatile_prefix_re().captures(lines[end]) else {
                break;
            };
            let next_body = collapse_ws(lines[end][next.get(0).unwrap().end()..].trim());
            if next_body != body {
                break;
            }
            end += 1;
        }
        if end - index >= 3 {
            output.push(line.trim_end().to_string());
            output.push(format!(
                "[repeated line with volatile prefix x{}]",
                end - index
            ));
            output.push(lines[end - 1].trim_end().to_string());
            changed = true;
            index = end;
            continue;
        }
        output.push(line.trim_end().to_string());
        index += 1;
    }

    let compact = join_compact(&output);
    let changed = changed || compact != text.trim();
    (compact, changed)
}

/// Fold consecutive identical multi-line blocks into one occurrence plus a count.
fn collapse_repeated_blocks(text: &str) -> (String, bool) {
    let lines: Vec<&str> = text.lines().collect();
    let protected = protected_lines(&lines);
    let mut output: Vec<String> = Vec::with_capacity(lines.len());
    let mut index = 0;
    let mut changed = false;

    while index < lines.len() {
        if protected[index] || lines[index].trim().is_empty() {
            output.push(lines[index].trim_end().to_string());
            index += 1;
            continue;
        }

        let mut best_k = 0;
        let mut best_count = 0;
        let max_k = std::cmp::min(MAX_BLOCK_LINES, (lines.len() - index) / 2);
        if max_k >= MIN_BLOCK_LINES {
            for k in (MIN_BLOCK_LINES..=max_k).rev() {
                if lines[index] != lines[index + k] {
                    continue;
                }
                if (0..k).any(|offset| protected[index + offset]) {
                    continue;
                }
                let block = &lines[index..index + k];
                let mut count = 1;
                let mut pos = index + k;
                while pos + k <= lines.len() && lines[pos..pos + k] == *block {
                    count += 1;
                    pos += k;
                }
                if count >= 2 && (k >= MIN_BLOCK_LINES + 1 || count >= 3) {
                    best_k = k;
                    best_count = count;
                    break;
                }
            }
        }

        if best_k > 0 {
            output.extend(
                lines[index..index + best_k]
                    .iter()
                    .map(|line| line.trim_end().to_string()),
            );
            output.push(format!("[repeated block x{best_count}]"));
            changed = true;
            index += best_k * best_count;
            continue;
        }
        output.push(lines[index].trim_end().to_string());
        index += 1;
    }

    let compact = join_compact(&output);
    let changed = changed || compact != text.trim();
    (compact, changed)
}

/// Fold identical lines and progress-bar runs.
fn collapse_repetition(text: &str) -> (String, bool) {
    let lines: Vec<&str> = text.lines().collect();
    let protected = protected_lines(&lines);
    let mut output: Vec<String> = Vec::with_capacity(lines.len());
    let mut index = 0;
    let mut changed = false;

    while index < lines.len() {
        let line = lines[index];
        if protected[index] || line.trim().is_empty() {
            output.push(line.trim_end().to_string());
            index += 1;
            continue;
        }

        let normalised = collapse_ws(line.trim());
        let mut end = index + 1;
        while end < lines.len() && !protected[end] && collapse_ws(lines[end].trim()) == normalised {
            end += 1;
        }
        if end - index >= 3 {
            output.push(line.trim_end().to_string());
            output.push(format!("[repeated identical line x{}]", end - index));
            changed = true;
            index = end;
            continue;
        }

        let signature = progress_signature(line);
        let mut end = index + 1;
        while end < lines.len()
            && !protected[end]
            && !signature.is_empty()
            && progress_signature(lines[end]) == signature
        {
            end += 1;
        }
        if end - index >= 4 {
            output.push(line.trim_end().to_string());
            output.push(format!(
                "[collapsed {} intermediate progress updates]",
                end - index - 2
            ));
            output.push(lines[end - 1].trim_end().to_string());
            changed = true;
            index = end;
            continue;
        }

        output.push(line.trim_end().to_string());
        index += 1;
    }

    let compact = join_compact(&output);
    let changed = changed || compact != text.trim();
    (compact, changed)
}

/// Strip whitespace outside JSON strings; byte-faithful for every other token.
fn minify_json(text: &str) -> (String, bool) {
    let mut output = String::with_capacity(text.len());
    let mut in_string = false;
    let mut escaped = false;
    for ch in text.chars() {
        if in_string {
            output.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
        } else if ch == '"' {
            in_string = true;
            output.push(ch);
        } else if matches!(ch, ' ' | '\t' | '\r' | '\n') {
            continue;
        } else {
            output.push(ch);
        }
    }
    let changed = output != text;
    (output, changed)
}

const DROP_TAGS: &[&str] = &["script", "style", "svg", "template", "noscript"];
const BLOCK_TAGS: &[&str] = &[
    "address", "article", "aside", "blockquote", "br", "div", "dl", "dt", "dd", "figcaption",
    "figure", "footer", "form", "h1", "h2", "h3", "h4", "h5", "h6", "header", "hr", "li", "main",
    "nav", "ol", "p", "pre", "section", "table", "td", "th", "tr", "ul",
];

/// Conservative HTML-to-text extraction with a main-content fast path.
///
/// This is a tag scanner rather than an HTML5 parser. It is deliberately
/// conservative: anything it does not understand is emitted as text, so the
/// worst case is a noisier preview, never a lost paragraph.
fn html_to_text(html: &str) -> (String, bool) {
    let mut all = String::with_capacity(html.len());
    let mut primary = String::new();
    let mut title = String::new();
    let mut drop_depth = 0usize;
    let mut primary_depth = 0usize;
    let mut title_depth = 0usize;
    let mut rest = html;

    while let Some(open) = rest.find('<') {
        let text = &rest[..open];
        if drop_depth == 0 && !text.trim().is_empty() {
            let value = collapse_ws(text);
            if !value.is_empty() {
                all.push(' ');
                all.push_str(&value);
                all.push(' ');
                if primary_depth > 0 {
                    primary.push(' ');
                    primary.push_str(&value);
                    primary.push(' ');
                }
                if title_depth > 0 {
                    title.push(' ');
                    title.push_str(&value);
                    title.push(' ');
                }
            }
        }
        let Some(close) = rest[open..].find('>') else {
            break;
        };
        let tag_body = &rest[open + 1..open + close];
        rest = &rest[open + close + 1..];

        let closing = tag_body.starts_with('/');
        let name: String = tag_body
            .trim_start_matches('/')
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        if name.is_empty() {
            continue;
        }
        if DROP_TAGS.contains(&name.as_str()) {
            if closing {
                drop_depth = drop_depth.saturating_sub(1);
            } else if !tag_body.ends_with('/') {
                drop_depth += 1;
            }
            continue;
        }
        if drop_depth > 0 {
            continue;
        }
        if BLOCK_TAGS.contains(&name.as_str()) {
            all.push('\n');
            if primary_depth > 0 {
                primary.push('\n');
            }
        }
        if name == "main" || name == "article" {
            if closing {
                primary_depth = primary_depth.saturating_sub(1);
            } else {
                primary_depth += 1;
            }
        }
        if name == "title" {
            if closing {
                title_depth = title_depth.saturating_sub(1);
            } else {
                title_depth += 1;
            }
        }
    }

    let render = |raw: &str| -> String {
        raw.lines()
            .map(|line| collapse_ws(line).trim().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    };

    let all_text = render(&all);
    let primary_text = render(&primary);
    let primary_chars = primary_text.trim().chars().count();
    let all_chars = std::cmp::max(1, all_text.trim().chars().count());
    if primary_chars >= 500 && (primary_chars as f32 / all_chars as f32) >= 0.45 {
        let heading = collapse_ws(&title);
        let heading = heading.trim();
        let body = if heading.is_empty() {
            primary_text
        } else {
            format!("# {heading}\n\n{primary_text}")
        };
        return (body, true);
    }
    (all_text, false)
}

/// Remove carrier noise without ranking or deleting unique semantic content.
pub fn denoise(text: &str, source_kind: &str) -> DenoiseResult {
    let original_chars = text.chars().count();
    if original_chars < MIN_CHARS {
        return DenoiseResult {
            text: text.to_string(),
            input_chars: original_chars,
            output_chars: original_chars,
            rules: Vec::new(),
        };
    }

    let mut rules: Vec<&'static str> = Vec::new();
    let (mut current, controls_changed) = clean_controls(text);
    if controls_changed {
        rules.push("controls");
    }

    let kind = source_kind.trim().to_lowercase();
    if kind == "html" || kind == "htm" {
        let (html_text, used_primary) = html_to_text(&current);
        if !html_text.trim().is_empty() {
            current = html_text;
            rules.push(if used_primary { "html_main" } else { "html_text" });
        }
    } else if kind == "json" {
        let (minified, changed) = minify_json(&current);
        if changed {
            current = minified;
            rules.push("structured_json");
        }
    }

    let (next, changed) = collapse_repetition(&current);
    if changed {
        current = next;
        rules.push("repetition");
    }
    let (next, changed) = collapse_volatile_prefix(&current);
    if changed {
        current = next;
        rules.push("volatile_prefix");
    }
    let (next, changed) = collapse_repeated_blocks(&current);
    if changed {
        current = next;
        rules.push("repeated_block");
    }

    DenoiseResult {
        output_chars: current.chars().count(),
        text: current,
        input_chars: original_chars,
        rules,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Denoise is a no-op below MIN_CHARS, so pad past the floor. The filler is
    /// unique per line so it never folds and never masks the rule under test.
    fn big(body: &str) -> String {
        let filler: String = (0..500).map(|index| format!("filler {index}\n")).collect();
        format!("{body}\n{filler}")
    }

    #[test]
    fn a_short_text_is_returned_untouched() {
        let result = denoise("hello\n", "text");
        assert_eq!(result.text, "hello\n");
        assert!(result.rules.is_empty());
        assert!(!result.changed());
    }

    #[test]
    fn ansi_escapes_are_removed() {
        let input = big("\x1b[31mred\x1b[0m");
        let result = denoise(&input, "text");
        assert!(!result.text.contains('\x1b'));
        assert!(result.rules.contains(&"controls"));
    }

    #[test]
    fn carriage_returns_become_newlines() {
        let result = denoise(&big("a\r\nb"), "text");
        assert!(!result.text.contains('\r'));
    }

    #[test]
    fn three_identical_lines_fold_to_one_plus_a_count() {
        let input = big("boom\nboom\nboom");
        let result = denoise(&input, "text");
        assert!(result.text.contains("[repeated identical line x3]"));
        assert_eq!(result.text.matches("boom").count(), 1);
    }

    #[test]
    fn two_identical_lines_are_left_alone() {
        let input = big("boom\nboom");
        let result = denoise(&input, "text");
        assert!(!result.text.contains("[repeated identical line"));
    }

    #[test]
    fn a_fenced_code_block_is_never_folded() {
        let input = big("```\nboom\nboom\nboom\n```");
        let result = denoise(&input, "text");
        assert_eq!(result.text.matches("boom").count(), 3);
    }

    #[test]
    fn an_indented_block_is_never_folded() {
        let input = big("    boom\n    boom\n    boom");
        let result = denoise(&input, "text");
        assert_eq!(result.text.matches("boom").count(), 3);
    }

    #[test]
    fn a_progress_bar_run_collapses_to_its_endpoints() {
        let mut body = String::new();
        for percent in 0..20 {
            body.push_str(&format!("downloading {percent}%\n"));
        }
        let result = denoise(&big(&body), "text");
        assert!(result.text.contains("intermediate progress updates"));
    }

    #[test]
    fn lines_differing_only_by_timestamp_fold() {
        let mut body = String::new();
        for second in 0..5 {
            body.push_str(&format!("2024-01-01T00:00:0{second}Z heartbeat ok\n"));
        }
        let result = denoise(&big(&body), "text");
        assert!(result.text.contains("repeated line with volatile prefix x5"));
        // The fold keeps the first and last occurrence, so the body appears twice.
        assert_eq!(result.text.matches("heartbeat ok").count(), 2);
    }

    #[test]
    fn a_repeated_multi_line_block_folds_to_one_copy() {
        let block = "alpha\nbeta\ngamma\n";
        let result = denoise(&big(&block.repeat(4)), "text");
        // The fold takes the widest repeating window first, so four copies of a
        // three-line block read as two copies of a six-line block. That is the
        // upstream behaviour and it is the smaller output of the two.
        assert!(result.text.contains("[repeated block x2]"));
        assert_eq!(result.text.matches("gamma").count(), 2);
    }

    #[test]
    fn json_whitespace_outside_strings_is_removed_but_strings_are_preserved() {
        let input = format!(
            "{{\n  \"a\": 1,\n  \"b\": \"keep  these   spaces\"\n}}\n{}",
            "x\n".repeat(3000)
        );
        let result = denoise(&input, "json");
        assert!(result.text.starts_with("{\"a\":1,\"b\":\"keep  these   spaces\"}"));
        assert!(result.rules.contains(&"structured_json"));
    }

    #[test]
    fn html_scripts_and_styles_are_dropped() {
        let html = format!(
            "<html><head><style>body{{color:red}}</style></head><body><p>visible</p><script>alert(1)</script></body></html>{}",
            "<!-- pad -->".repeat(500)
        );
        let result = denoise(&html, "html");
        assert!(result.text.contains("visible"));
        assert!(!result.text.contains("alert"));
        assert!(!result.text.contains("color:red"));
    }

    #[test]
    fn html_main_content_wins_when_it_dominates() {
        let body = "paragraph text here. ".repeat(200);
        let html = format!(
            "<html><head><title>Doc</title></head><body><nav>menu</nav><main><p>{body}</p></main></body></html>"
        );
        let result = denoise(&html, "html");
        assert!(result.rules.contains(&"html_main"));
        assert!(result.text.starts_with("# Doc"));
        assert!(!result.text.contains("menu"));
    }

    #[test]
    fn denoise_never_grows_the_text_it_accepts() {
        let input = big("plain\nlines\nhere");
        let result = denoise(&input, "text");
        assert!(result.output_chars <= result.input_chars);
    }
}
