//! tokenjuice: lean output compaction for terminal-heavy agent workflows.
//!
//! The engine is a rule set plus a fixed pipeline. A rule says which command it
//! owns and how to shrink that command's output; the pipeline applies the
//! transforms, drops the noise lines, counts the facts worth keeping, and
//! summarises to head and tail. Nothing here calls a model — the whole point is
//! that the reduction is deterministic, so the same command always produces the
//! same text and the agent's prefix cache stays warm.
//!
//! Ported from tokenjuice (`src/core/reduce.ts` and friends). See
//! [`rules`] for the rule set and [`text`] for the primitives.

pub mod formatters;
pub mod pattern;
pub mod rules;
pub mod text;

use std::collections::BTreeMap;

use rules::{CompiledRule, FALLBACK_RULE};

pub use rules::ExecutionInput;

/// Outputs at or below this many characters are never worth rewriting: the
/// marker text alone would cost more than it saves.
const TINY_OUTPUT_MAX_CHARS: usize = 240;

/// A small output is left verbatim unless compaction saves at least this much.
const SMALL_OUTPUT_PASSTHROUGH_MIN_SAVED_CHARS: usize = 120;

/// ...and unless it shrinks it to at most this fraction.
const SMALL_OUTPUT_PASSTHROUGH_MAX_RATIO: f64 = 0.75;

/// Counters whose names read as failures. On a successful fallback run they are
/// mentions, not failures, and the label says so.
const FAILURE_SEVERITY_FACTS: [&str; 2] = ["error", "warning"];

/// Commands whose whole purpose is to show a file's bytes. Their output is
/// passed through untouched: the agent asked for the content, not a summary of
/// it, and a summarised config file is worse than no config file.
const FILE_CONTENT_INSPECTION_COMMANDS: [&str; 9] =
    ["cat", "sed", "head", "tail", "nl", "bat", "batcat", "jq", "yq"];

/// What the engine decided.
#[derive(Debug, Clone)]
pub struct CompactResult {
    /// The text the agent should see.
    pub text: String,
    /// The rule's own summary, before the exit code and facts were added. Kept
    /// because it is the part a human reads in the panel.
    pub preview: String,
    /// The rule that handled it, or `None` when nothing matched.
    pub rule_id: Option<String>,
    pub family: String,
    /// Named counts the rule extracted — `modified file`, `test failed`.
    pub facts: BTreeMap<String, u64>,
    pub raw_chars: usize,
    pub compact_chars: usize,
    /// Whether the text differs from the raw output.
    pub compacted: bool,
}

impl CompactResult {
    pub fn saved_chars(&self) -> usize {
        self.raw_chars.saturating_sub(self.compact_chars)
    }

    /// The one-line summary the panel shows.
    pub fn headline(&self) -> String {
        let rule = self.rule_id.as_deref().unwrap_or("no rule");
        if self.compacted {
            format!(
                "{rule} · {} → {} chars",
                self.raw_chars, self.compact_chars
            )
        } else {
            format!("{rule} · passed through")
        }
    }
}

/// Options for one reduction.
#[derive(Debug, Clone)]
pub struct Options {
    /// The largest text handed over inline. Longer output is summarised.
    pub max_inline_chars: usize,
    /// Rule ids the user switched off.
    pub disabled_rules: Vec<String>,
    /// Extra commands to pass through verbatim.
    pub passthrough_commands: Vec<String>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            max_inline_chars: 1200,
            disabled_rules: Vec::new(),
            passthrough_commands: Vec::new(),
        }
    }
}

/// Compact one command's output.
pub fn compact(input: &ExecutionInput, options: &Options) -> CompactResult {
    let raw_text = input.raw_text();
    let raw_chars = text::count_chars(&text::strip_ansi(&raw_text));

    if raw_text.trim().is_empty() {
        return CompactResult {
            text: "(no output)".to_string(),
            preview: String::new(),
            rule_id: None,
            family: "empty".to_string(),
            facts: BTreeMap::new(),
            raw_chars,
            compact_chars: 13,
            compacted: false,
        };
    }

    // A command whose whole purpose is to print a setting verbatim is never
    // rewritten, whatever a rule says. This is the narrow case: `plutil -p` and
    // `openclaw config get`. Reading a *file* is handled further down, because
    // a rule that owns the command knows better than the file-type heuristic.
    if is_verbatim_setting_command(input, options) {
        return passthrough(raw_text, raw_chars, "verbatim");
    }

    let Some(rule) = rules::resolve(input) else {
        return passthrough(raw_text, raw_chars, "generic");
    };

    if options.disabled_rules.iter().any(|id| id == &rule.rule.id) {
        return CompactResult {
            text: raw_text.clone(),
            preview: String::new(),
            rule_id: Some(rule.rule.id.clone()),
            family: rule.rule.family.clone(),
            facts: BTreeMap::new(),
            raw_chars,
            compact_chars: raw_chars,
            compacted: false,
        };
    }

    // A file-content command that no rule claimed is passed through: the agent
    // asked for the bytes, and a summary of a config file is worse than none.
    if rule.rule.id == FALLBACK_RULE && is_file_content_command(input) {
        return passthrough(raw_text, raw_chars, "file-content");
    }

    let (summary, facts) = apply_rule(rule, input, &raw_text, options.max_inline_chars);
    let compact_text = format_inline(rule, input, &summary, &facts);
    let chosen = select_inline_text(
        rule,
        input,
        &raw_text,
        raw_chars,
        &compact_text,
        options.max_inline_chars,
    );
    let compact_chars = text::count_chars(&chosen);

    CompactResult {
        compacted: chosen != raw_text,
        text: chosen,
        preview: summary,
        rule_id: Some(rule.rule.id.clone()),
        family: rule.rule.family.clone(),
        facts,
        raw_chars,
        compact_chars,
    }
}

fn passthrough(raw_text: String, raw_chars: usize, family: &str) -> CompactResult {
    CompactResult {
        text: raw_text,
        preview: String::new(),
        rule_id: None,
        family: family.to_string(),
        facts: BTreeMap::new(),
        raw_chars,
        compact_chars: raw_chars,
        compacted: false,
    }
}

/// The command name with any directory and `.exe` stripped.
fn command_name(input: &ExecutionInput) -> Option<&str> {
    let argv0 = input.argv.first()?;
    Some(
        argv0
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(argv0)
            .trim_end_matches(".exe"),
    )
}

/// Commands that print a setting's value and nothing else.
fn is_verbatim_setting_command(input: &ExecutionInput, options: &Options) -> bool {
    let Some(name) = command_name(input) else {
        return false;
    };
    if options
        .passthrough_commands
        .iter()
        .any(|command| command == name)
    {
        return true;
    }
    // `plutil -p file` and `plutil -o - file` print a plist as text.
    if name == "plutil" {
        return input.argv.iter().any(|arg| arg == "-p")
            || input
                .argv
                .windows(2)
                .any(|pair| pair[0] == "-o" && pair[1] == "-");
    }
    // `openclaw config get <key>` prints one value.
    name == "openclaw"
        && input.argv.get(1).map(String::as_str) == Some("config")
        && input.argv.get(2).map(String::as_str) == Some("get")
}

/// Commands whose output is a file's contents.
fn is_file_content_command(input: &ExecutionInput) -> bool {
    let Some(name) = command_name(input) else {
        return false;
    };
    if FILE_CONTENT_INSPECTION_COMMANDS.contains(&name) {
        return true;
    }
    // `git show <blob>` prints a file. `git show <commit>` prints a commit,
    // which the `git/show` rule owns — but that rule is only reached when it
    // matched, and this check only runs on the fallback path.
    name == "git" && input.argv.get(1).map(String::as_str) == Some("show")
}

fn pretty_print_json_if_possible(text: &str) -> String {
    let trimmed = text.trim();
    if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
        return text.to_string();
    }
    match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(value) if value.is_object() || value.is_array() => {
            serde_json::to_string_pretty(&value).unwrap_or_else(|_| text.to_string())
        }
        _ => text.to_string(),
    }
}

fn as_usize(value: Option<f64>, fallback: usize) -> usize {
    match value {
        Some(number) if number >= 0.0 => number as usize,
        _ => fallback,
    }
}

/// Apply one rule: transforms, filters, counters, then head/tail.
fn apply_rule(
    rule: &CompiledRule,
    input: &ExecutionInput,
    raw_text: &str,
    max_inline_chars: usize,
) -> (String, BTreeMap<String, u64>) {
    let mut facts: BTreeMap<String, u64> = BTreeMap::new();
    let mut raw = raw_text.to_string();

    if rule.rule.transforms.pretty_print_json {
        raw = pretty_print_json_if_possible(&raw);
    }

    let mut lines = text::normalize_lines(&raw);
    if rule.rule.transforms.strip_ansi {
        lines = text::normalize_lines(&text::strip_ansi(&lines.join("\n")));
    }

    // A rule can replace the whole output with one sentence when the output
    // matches a known shape — "no changes" for an empty diff, and so on.
    let output_match_text = text::trim_empty_edges(&lines).join("\n");
    if input.exit_code.unwrap_or(0) == 0 {
        if let Some((_, message)) = rule
            .output_matches()
            .iter()
            .find(|(pattern, _)| pattern.is_match(&output_match_text))
        {
            return (message.clone(), facts);
        }
    }

    if !rule.skip_patterns().is_empty() {
        lines.retain(|line| !rule.skip_patterns().iter().any(|re| re.is_match(line)));
    }

    let mut counter_lines = lines.clone();

    if !rule.keep_patterns().is_empty() {
        let kept: Vec<String> = lines
            .iter()
            .filter(|line| rule.keep_patterns().iter().any(|re| re.is_match(line)))
            .cloned()
            .collect();
        // An empty keep result means the patterns did not apply to this output;
        // dropping everything would be worse than keeping the filtered lines.
        if !kept.is_empty() {
            lines = kept;
        }
    }

    if rule.rule.transforms.trim_empty_edges {
        counter_lines = text::trim_empty_edges(&counter_lines);
        lines = text::trim_empty_edges(&lines);
    }

    if rule.rule.transforms.dedupe_adjacent {
        counter_lines = text::dedupe_adjacent(&counter_lines);
        lines = text::dedupe_adjacent(&lines);
    }

    // `gh --json` prints JSON; the rule's filters cannot make that readable, so
    // the formatter rewrites it into one line per record first.
    if rule.rule.id == "cloud/gh" {
        if let Some(rewritten) = formatters::rewrite_gh_lines(&lines) {
            counter_lines = rewritten.clone();
            lines = rewritten;
        }
    }

    let pre_keep = rule.rule.counter_source.as_deref() == Some("preKeep");
    for (name, pattern) in rule.counters() {
        let source = if pre_keep { &counter_lines } else { &lines };
        let count = source.iter().filter(|line| pattern.is_match(line)).count();
        facts.insert(name.clone(), count as u64);
    }

    if lines.is_empty() {
        if let Some(on_empty) = &rule.rule.on_empty {
            return (on_empty.clone(), facts);
        }
    }

    let failed = input.exit_code.is_some_and(|code| code != 0);
    let (head, tail) = if failed && rule.rule.failure.preserve_on_failure {
        (
            as_usize(rule.rule.failure.head, 6),
            as_usize(rule.rule.failure.tail, 12),
        )
    } else {
        (
            as_usize(rule.rule.summarize.head, 6),
            as_usize(rule.rule.summarize.tail, 6),
        )
    };

    // Search results are the one family where the middle matters: a match list
    // is the answer, so it is only summarised when it genuinely does not fit.
    let joined = lines.join("\n");
    let search_fits = rule.rule.family == "search" && text::count_chars(&joined) <= max_inline_chars;

    let compacted = if search_fits {
        lines
    } else {
        text::head_tail(&lines, head, tail)
    };

    (compacted.join("\n").trim().to_string(), facts)
}

/// Assemble the text the agent sees: exit code, the facts worth keeping, then
/// the summary.
fn format_inline(
    rule: &CompiledRule,
    input: &ExecutionInput,
    summary: &str,
    facts: &BTreeMap<String, u64>,
) -> String {
    let failed = input.exit_code.is_some_and(|code| code != 0);
    let neutralize = rule.rule.id == FALLBACK_RULE && !failed;

    let fact_parts: Vec<String> = facts
        .iter()
        .filter(|(_, count)| **count > 0)
        .map(|(name, count)| {
            let label = if neutralize && FAILURE_SEVERITY_FACTS.contains(&name.as_str()) {
                format!("{name} mention")
            } else {
                name.clone()
            };
            text::pluralize(*count as usize, &label)
        })
        .collect();

    let mut lines: Vec<String> = Vec::new();
    if let Some(code) = input.exit_code.filter(|code| *code != 0) {
        lines.push(format!("exit {code}"));
    }

    let family = rule.rule.family.as_str();
    let include_facts = family == "search"
        || (family != "git-status" && family != "help" && summary.contains("omitted"))
        || (family == "test-results" && failed);

    if include_facts && !fact_parts.is_empty() {
        lines.push(fact_parts.join(", "));
    }
    lines.push(summary.to_string());
    lines.join("\n").trim().to_string()
}

fn build_passthrough_text(input: &ExecutionInput, raw_text: &str) -> String {
    let normalized = text::trim_empty_edges(&text::normalize_lines(&text::strip_ansi(raw_text)))
        .join("\n")
        .trim()
        .to_string();
    if normalized.is_empty() {
        return "(no output)".to_string();
    }
    match input.exit_code.filter(|code| *code != 0) {
        Some(code) => format!("exit {code}\n{normalized}"),
        None => normalized,
    }
}

fn build_literal_passthrough_text(input: &ExecutionInput, raw_text: &str) -> String {
    let normalized = text::strip_ansi(raw_text).trim_end().to_string();
    if normalized.is_empty() {
        return build_passthrough_text(input, raw_text);
    }
    match input.exit_code.filter(|code| *code != 0) {
        Some(code) => format!("exit {code}\n{normalized}"),
        None => normalized,
    }
}

/// A terse discovery command — `git status --short`, `git remote -v` — already
/// prints almost nothing. Rewriting it costs more than it saves.
fn is_terse_discovery_command(rule: &CompiledRule, input: &ExecutionInput) -> bool {
    let has = |needle: &str| input.argv.iter().any(|arg| arg == needle);
    match rule.rule.family.as_str() {
        "git-status" => {
            has("--short") || has("-s") || input.argv.iter().any(|a| a.starts_with("--porcelain"))
        }
        "git-remote" => has("-v") || has("--verbose"),
        "git-worktree" => has("--porcelain"),
        _ => false,
    }
}

fn should_keep_small_output(
    rule: &CompiledRule,
    input: &ExecutionInput,
    raw_chars: usize,
    compact_chars: usize,
    max_inline_chars: usize,
) -> bool {
    if raw_chars == 0 || raw_chars > max_inline_chars || input.exit_code.unwrap_or(0) != 0 {
        return false;
    }
    if !is_terse_discovery_command(rule, input) {
        return false;
    }
    let saved = raw_chars.saturating_sub(compact_chars);
    let ratio = if raw_chars == 0 {
        1.0
    } else {
        compact_chars as f64 / raw_chars as f64
    };
    saved < SMALL_OUTPUT_PASSTHROUGH_MIN_SAVED_CHARS || ratio > SMALL_OUTPUT_PASSTHROUGH_MAX_RATIO
}

/// Choose between the compacted text and the raw text.
///
/// The rule is "never make it worse". A rewrite that saves nothing, or that
/// costs more than the original, is discarded in favour of what the command
/// actually printed.
fn select_inline_text(
    rule: &CompiledRule,
    input: &ExecutionInput,
    raw_text: &str,
    raw_chars: usize,
    compact_text: &str,
    max_inline_chars: usize,
) -> String {
    let compact_chars = text::count_chars(compact_text);

    // The budget is a budget on what the agent reads, so it applies to the
    // compacted text as well. A rule that folds a 40 KB build log into 8 KB has
    // still handed the agent 8 KB when the limit is 1200 characters, and the
    // panel says the excess is cut to its head and tail. It was not: the limit
    // was only ever compared against the raw output, so a rule that compacted
    // well but not enough returned the whole thing.
    let compact = text::clamp_text_middle(compact_text, max_inline_chars);

    if should_keep_small_output(rule, input, raw_chars, compact_chars, max_inline_chars) {
        return build_literal_passthrough_text(input, raw_text);
    }

    // git status is always rewritten: its verbose form is the single most
    // expensive thing an agent reads repeatedly.
    if rule.rule.family == "git-status" {
        return compact;
    }

    if raw_chars <= max_inline_chars && compact_chars >= raw_chars {
        return build_passthrough_text(input, raw_text);
    }

    let passthrough = build_passthrough_text(input, raw_text);
    let passthrough_chars = text::count_chars(&passthrough);
    let passthrough_limit = if rule.rule.family == "help" {
        max_inline_chars
    } else {
        TINY_OUTPUT_MAX_CHARS
    };

    if passthrough_chars > passthrough_limit {
        return compact;
    }
    if passthrough_chars <= compact_chars {
        return passthrough;
    }
    compact
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The terminal boundary is the agent's `exec` tool, so every rule that
    /// filters on `toolNames` expects that name.
    fn run(argv: &[&str], stdout: &str, exit_code: i32) -> CompactResult {
        let input = ExecutionInput {
            command: Some(argv.join(" ")),
            argv: argv.iter().map(|s| s.to_string()).collect(),
            exit_code: Some(exit_code),
            stdout: stdout.to_string(),
            tool_name: Some("exec".into()),
            ..Default::default()
        };
        compact(&input, &Options::default())
    }

    #[test]
    fn empty_output_says_so_rather_than_returning_nothing() {
        let result = run(&["true"], "", 0);
        assert_eq!(result.text, "(no output)");
        assert!(!result.compacted);
    }

    #[test]
    fn a_file_inspection_command_is_never_rewritten() {
        // 400 lines of a config file: exactly what a summariser would ruin.
        let body: String = (0..400).map(|i| format!("key{i} = value{i}\n")).collect();
        let result = run(&["cat", "config.toml"], &body, 0);
        assert_eq!(result.family, "file-content");
        assert_eq!(result.text, body);
        assert!(!result.compacted);
    }

    /// `git show` is owned by a rule, so it is compacted — the file-content
    /// passthrough only applies when nothing claimed the command.
    #[test]
    fn git_show_is_owned_by_its_rule_not_the_passthrough() {
        let body: String = (0..400).map(|i| format!("line {i}\n")).collect();
        let result = run(&["git", "show", "HEAD"], &body, 0);
        assert_eq!(result.rule_id.as_deref(), Some("git/show"));
    }

    #[test]
    fn a_long_build_log_is_summarised_to_head_and_tail() {
        let body: String = (0..500)
            .map(|i| format!("compiling crate number {i}\n"))
            .collect();
        let result = run(&["cargo", "test"], &body, 0);
        assert!(result.compacted, "a 500-line log must be compacted");
        assert!(result.compact_chars < result.raw_chars);
        assert!(result.text.contains("lines omitted"));
        assert!(result.text.contains("compiling crate number 0"));
        assert!(result.text.contains("compiling crate number 499"));
    }

    /// The budget is a budget on what the agent reads, so it applies to the
    /// compacted text as well as the raw output.
    ///
    /// It did not. The limit was only ever compared against the raw output, so a
    /// rule that compacted a 40 KB log into 8 KB returned all 8 KB against a
    /// 1200-character budget — and the panel said the excess was cut to its head
    /// and tail. `clamp_text_middle` existed for exactly this and nothing called
    /// it, which is how the gap survived: the function was tested, the wiring was
    /// not.
    #[test]
    fn the_budget_applies_to_the_compacted_text_too() {
        let body: String = (0..500)
            .map(|i| format!("compiling crate number {i}\n"))
            .collect();
        let input = ExecutionInput {
            command: Some("cargo test".into()),
            argv: vec!["cargo".into(), "test".into()],
            exit_code: Some(0),
            stdout: body,
            tool_name: Some("exec".into()),
            ..Default::default()
        };
        let result = compact(
            &input,
            &Options {
                max_inline_chars: 200,
                ..Default::default()
            },
        );
        assert!(result.compacted);
        let read = text::count_chars(&result.text);
        assert!(
            read <= 200,
            "the agent read {read} characters against a 200-character budget"
        );
        // Head and tail, not a head-only cut: the last lines say how it ended.
        assert!(result.text.contains("compiling crate number 499"));
        assert!(result.text.contains("compiling crate number 0"));
    }

    #[test]
    fn a_failing_command_keeps_more_of_its_tail() {
        let body: String = (0..500).map(|i| format!("line {i}\n")).collect();
        let ok = run(&["cargo", "test"], &body, 0);
        let failed = run(&["cargo", "test"], &body, 1);
        assert!(failed.text.starts_with("exit 1"));
        // The failure rule keeps 12 tail lines against the success rule's 6.
        assert!(
            failed.compact_chars > ok.compact_chars,
            "a failure must keep more context than a success"
        );
    }

    #[test]
    fn a_disabled_rule_passes_the_output_through() {
        let body: String = (0..500).map(|i| format!("line {i}\n")).collect();
        let input = ExecutionInput {
            command: Some("cargo test".into()),
            argv: vec!["cargo".into(), "test".into()],
            exit_code: Some(0),
            stdout: body.clone(),
            tool_name: Some("exec".into()),
            ..Default::default()
        };
        let options = Options {
            disabled_rules: vec!["tests/cargo-test".into()],
            ..Default::default()
        };
        let result = compact(&input, &options);
        assert_eq!(result.text, body);
        assert!(!result.compacted);
    }

    /// A two-line listing is not worth rewriting. The passthrough normalises
    /// trailing whitespace, so the text is compared by content rather than by
    /// identity — the point is that no summary replaced it.
    #[test]
    fn a_tiny_output_is_left_alone() {
        let result = run(&["ls"], "a.txt\nb.txt\n", 0);
        assert_eq!(result.text, "a.txt\nb.txt");
        assert!(!result.text.contains("items"));
    }

    #[test]
    fn the_headline_names_the_rule_and_the_saving() {
        let body: String = (0..500).map(|i| format!("line {i}\n")).collect();
        let result = run(&["cargo", "test"], &body, 0);
        let headline = result.headline();
        assert!(headline.contains("tests/cargo-test"), "{headline}");
        assert!(headline.contains("→"), "{headline}");
    }

    #[test]
    fn a_passthrough_command_from_settings_is_honoured() {
        let body: String = (0..500).map(|i| format!("line {i}\n")).collect();
        let input = ExecutionInput {
            command: Some("my-tool dump".into()),
            argv: vec!["my-tool".into(), "dump".into()],
            exit_code: Some(0),
            stdout: body.clone(),
            ..Default::default()
        };
        let options = Options {
            passthrough_commands: vec!["my-tool".into()],
            ..Default::default()
        };
        let result = compact(&input, &options);
        assert_eq!(result.text, body);
    }

    #[test]
    fn stderr_is_compacted_with_stdout_rather_than_dropped() {
        let input = ExecutionInput {
            command: Some("cargo test".into()),
            argv: vec!["cargo".into(), "test".into()],
            exit_code: Some(1),
            stdout: "running 1 test\n".into(),
            stderr: "error[E0308]: mismatched types\n".into(),
            ..Default::default()
        };
        let result = compact(&input, &Options::default());
        assert!(result.text.contains("mismatched types"), "{}", result.text);
    }

    #[test]
    fn json_output_is_pretty_printed_when_the_rule_asks() {
        let pretty = pretty_print_json_if_possible(r#"{"a":1,"b":[2,3]}"#);
        assert!(pretty.contains("\n  \"a\": 1"), "{pretty}");
        assert_eq!(pretty_print_json_if_possible("not json"), "not json");
    }

    /* ---------------------------------------------------------------- *
     * Conformance against upstream's own fixtures
     * ---------------------------------------------------------------- */

    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Fixture {
        id: String,
        input: FixtureInput,
        expect: FixtureExpect,
    }

    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct FixtureInput {
        #[serde(default)]
        tool_name: Option<String>,
        #[serde(default)]
        argv: Vec<String>,
        #[serde(default)]
        command: Option<String>,
        #[serde(default)]
        combined_text: Option<String>,
        #[serde(default)]
        stdout: Option<String>,
        #[serde(default)]
        stderr: Option<String>,
        #[serde(default)]
        exit_code: Option<i32>,
    }

    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct FixtureExpect {
        matched_reducer: String,
        family: String,
        #[serde(default)]
        contains: Vec<String>,
        #[serde(default)]
        excludes: Vec<String>,
    }

    fn fixtures() -> Vec<Fixture> {
        serde_json::from_str(include_str!("fixtures.json"))
            .expect("the embedded tokenjuice fixtures are valid JSON")
    }

    /// The port is only worth having if it agrees with the original. These are
    /// tokenjuice's own fixtures, copied verbatim from `src/rules/fixtures/`,
    /// and the comparison mirrors `verifyFixtures` in `src/core/fixtures.ts`:
    /// the searchable text is the inline text, the rule's preview, and the
    /// facts rendered as `N name`, joined by newlines.
    #[test]
    fn every_upstream_fixture_agrees_with_the_port() {
        let mut failures: Vec<String> = Vec::new();
        let all = fixtures();

        for fixture in &all {
            let input = ExecutionInput {
                command: fixture.input.command.clone(),
                argv: fixture.input.argv.clone(),
                exit_code: fixture.input.exit_code,
                stdout: fixture
                    .input
                    .stdout
                    .clone()
                    .or_else(|| fixture.input.combined_text.clone())
                    .unwrap_or_default(),
                stderr: fixture.input.stderr.clone().unwrap_or_default(),
                tool_name: fixture.input.tool_name.clone(),
            };
            // Upstream verifies at a 5000-character budget.
            let options = Options {
                max_inline_chars: 5000,
                ..Default::default()
            };
            let result = compact(&input, &options);

            if result.rule_id.as_deref() != Some(fixture.expect.matched_reducer.as_str()) {
                failures.push(format!(
                    "{}: expected rule {}, got {:?}",
                    fixture.id, fixture.expect.matched_reducer, result.rule_id
                ));
                continue;
            }
            if result.family != fixture.expect.family {
                failures.push(format!(
                    "{}: expected family {}, got {}",
                    fixture.id, fixture.expect.family, result.family
                ));
            }

            let fact_text = result
                .facts
                .iter()
                .filter(|(_, count)| **count > 0)
                .map(|(name, count)| {
                    format!("{count} {name}{}", if *count == 1 { "" } else { "s" })
                })
                .collect::<Vec<_>>()
                .join("\n");
            let searchable = [result.text.clone(), result.preview.clone(), fact_text]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join("\n");

            for needle in &fixture.expect.contains {
                if !searchable.contains(needle.as_str()) {
                    failures.push(format!("{}: output is missing {needle:?}", fixture.id));
                }
            }
            for needle in &fixture.expect.excludes {
                if searchable.contains(needle.as_str()) {
                    failures.push(format!("{}: output still contains {needle:?}", fixture.id));
                }
            }
        }

        assert!(
            failures.is_empty(),
            "{} divergences across {} fixtures:\n{}",
            failures.len(),
            all.len(),
            failures.join("\n")
        );
    }

    #[test]
    fn the_fixture_set_is_not_empty() {
        assert!(fixtures().len() > 100, "fixtures did not load");
    }
}
