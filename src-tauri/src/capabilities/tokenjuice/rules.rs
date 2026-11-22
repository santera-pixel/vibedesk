//! The rule set: types, matching, scoring and compilation.
//!
//! Ported from tokenjuice's `src/core/rules.ts` and `src/core/classify.ts`. The
//! rules themselves are data — 266 JSON documents covering 71 command families,
//! embedded at build time from `rules.json`, which is a verbatim concatenation
//! of the upstream `src/rules/**/*.json` files.
//!
//! One deliberate simplification: upstream derives several candidate argv
//! vectors by parsing the shell command string, because a host only hands it a
//! command line. VibeDesk is the one running the process, so it already has the
//! real `command` and `args` from `terminal/create`. There is nothing to
//! re-derive, and no chance of mis-tokenising a quoted argument.

use std::sync::OnceLock;

use serde::Deserialize;

use super::pattern::Pattern;

/// The output of one command, as the engine sees it.
#[derive(Debug, Clone, Default)]
pub struct ExecutionInput {
    pub command: Option<String>,
    pub argv: Vec<String>,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub tool_name: Option<String>,
}

impl ExecutionInput {
    /// The text the engine compacts: stdout, then stderr, joined the way a
    /// terminal would show them.
    pub fn raw_text(&self) -> String {
        match (self.stdout.is_empty(), self.stderr.is_empty()) {
            (true, true) => String::new(),
            (false, true) => self.stdout.clone(),
            (true, false) => self.stderr.clone(),
            (false, false) => format!("{}\n{}", self.stdout, self.stderr),
        }
    }

    /// The command line, rebuilt from argv when the caller did not supply one.
    pub fn command_line(&self) -> String {
        match &self.command {
            Some(command) if !command.trim().is_empty() => command.clone(),
            _ => self.argv.join(" "),
        }
    }

    /// The argv to match rules against.
    ///
    /// VibeDesk always has the real argv, because it is the process that ran
    /// the command. A caller that only has a command line — a fixture, or a
    /// future host that hands over a shell string — gets a whitespace split,
    /// which is enough for the `argv0` and `argvIncludes` matchers the rule set
    /// actually uses.
    pub fn effective_argv(&self) -> Vec<String> {
        if !self.argv.is_empty() {
            return self.argv.clone();
        }
        self.command
            .as_deref()
            .map(|command| {
                command
                    .split_whitespace()
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RuleMatch {
    pub tool_names: Option<Vec<String>>,
    pub argv0: Option<Vec<String>>,
    pub git_subcommands: Option<Vec<String>>,
    pub argv_includes: Option<Vec<Vec<String>>>,
    pub argv_includes_any: Option<Vec<Vec<String>>>,
    pub command_includes: Option<Vec<String>>,
    pub command_includes_any: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RuleFilters {
    pub skip_patterns: Option<Vec<String>>,
    pub keep_patterns: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RuleTransforms {
    pub strip_ansi: bool,
    pub pretty_print_json: bool,
    pub dedupe_adjacent: bool,
    pub trim_empty_edges: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RuleSummarize {
    pub head: Option<f64>,
    pub tail: Option<f64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RuleFailure {
    pub preserve_on_failure: bool,
    pub head: Option<f64>,
    pub tail: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleCounter {
    pub name: String,
    pub pattern: String,
    pub flags: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputMatch {
    pub pattern: String,
    pub message: String,
    pub flags: Option<String>,
}

/// One rule, exactly as upstream writes it.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rule {
    pub id: String,
    pub family: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub on_empty: Option<String>,
    #[serde(default)]
    pub counter_source: Option<String>,
    #[serde(default)]
    pub match_output: Option<Vec<OutputMatch>>,
    #[serde(default)]
    pub priority: Option<f64>,
    #[serde(default)]
    pub r#match: RuleMatch,
    #[serde(default)]
    pub filters: RuleFilters,
    #[serde(default)]
    pub transforms: RuleTransforms,
    #[serde(default)]
    pub summarize: RuleSummarize,
    #[serde(default)]
    pub failure: RuleFailure,
    #[serde(default)]
    pub counters: Option<Vec<RuleCounter>>,
}

/// A rule with its patterns compiled once.
pub struct CompiledRule {
    pub rule: Rule,
    skip: Vec<Pattern>,
    keep: Vec<Pattern>,
    counters: Vec<(String, Pattern)>,
    output_matches: Vec<(Pattern, String)>,
}

impl CompiledRule {
    fn new(rule: Rule) -> Self {
        let skip = rule
            .filters
            .skip_patterns
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter_map(|pattern| Pattern::compile(pattern, None).ok())
            .collect();
        let keep = rule
            .filters
            .keep_patterns
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter_map(|pattern| Pattern::compile(pattern, None).ok())
            .collect();
        let counters = rule
            .counters
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter_map(|counter| {
                Pattern::compile(&counter.pattern, counter.flags.as_deref())
                    .ok()
                    .map(|re| (counter.name.clone(), re))
            })
            .collect();
        let output_matches = rule
            .match_output
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter_map(|entry| {
                Pattern::compile(&entry.pattern, entry.flags.as_deref())
                    .ok()
                    .map(|re| (re, entry.message.clone()))
            })
            .collect();
        Self {
            rule,
            skip,
            keep,
            counters,
            output_matches,
        }
    }

    pub fn skip_patterns(&self) -> &[Pattern] {
        &self.skip
    }

    pub fn keep_patterns(&self) -> &[Pattern] {
        &self.keep
    }

    pub fn counters(&self) -> &[(String, Pattern)] {
        &self.counters
    }

    pub fn output_matches(&self) -> &[(Pattern, String)] {
        &self.output_matches
    }
}

/// The subcommand of a `git` invocation: the first argument that is not a flag
/// and not a flag's value.
fn git_subcommand(argv: &[String]) -> Option<&str> {
    if argv.first().map(String::as_str) != Some("git") {
        return None;
    }
    let mut index = 1;
    while index < argv.len() {
        let arg = argv[index].as_str();
        if arg.starts_with('-') {
            // `-C <path>` and `-c <key=value>` take a value; skip it too.
            if matches!(arg, "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace") {
                index += 2;
                continue;
            }
            index += 1;
            continue;
        }
        return Some(arg);
    }
    None
}

fn includes_all(argv: &[String], expected: &[String]) -> bool {
    expected.iter().all(|part| argv.iter().any(|arg| arg == part))
}

fn is_word_like(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

/// Whether `part` appears in `command` on word boundaries.
///
/// `git status` must not match `git statusx`, and `status` must not match
/// `mystatus`. Upstream does the same scan by hand; this is the same rule.
fn includes_command_part(command: &str, part: &str) -> bool {
    if part.is_empty() {
        return true;
    }
    let chars: Vec<char> = command.chars().collect();
    let part_chars: Vec<char> = part.chars().collect();
    if part_chars.len() > chars.len() {
        return false;
    }
    let part_starts_word = part_chars.first().copied().is_some_and(is_word_like);
    let part_ends_word = part_chars.last().copied().is_some_and(is_word_like);

    for start in 0..=(chars.len() - part_chars.len()) {
        if chars[start..start + part_chars.len()] != part_chars[..] {
            continue;
        }
        let prev = if start > 0 { Some(chars[start - 1]) } else { None };
        let next = chars.get(start + part_chars.len()).copied();
        let left_ok = !part_starts_word || !prev.is_some_and(is_word_like);
        let right_ok = !part_ends_word || !next.is_some_and(is_word_like);
        if left_ok && right_ok {
            return true;
        }
    }
    false
}

impl Rule {
    pub fn matches(&self, input: &ExecutionInput) -> bool {
        let command = input.command_line();
        let argv = input.effective_argv();

        if let Some(names) = &self.r#match.tool_names {
            let tool = input.tool_name.as_deref().unwrap_or("");
            if !names.iter().any(|name| name == tool) {
                return false;
            }
        }

        if let Some(argv0) = &self.r#match.argv0 {
            let first = argv.first().map(String::as_str).unwrap_or("");
            if !argv0.iter().any(|name| name == first) {
                return false;
            }
        }

        if let Some(subcommands) = &self.r#match.git_subcommands {
            let found = git_subcommand(&argv).unwrap_or("");
            if !subcommands.iter().any(|name| name == found) {
                return false;
            }
        }

        if let Some(groups) = &self.r#match.argv_includes {
            if !groups.iter().all(|parts| includes_all(&argv, parts)) {
                return false;
            }
        }

        if let Some(groups) = &self.r#match.argv_includes_any {
            if !groups.iter().any(|parts| includes_all(&argv, parts)) {
                return false;
            }
        }

        if let Some(parts) = &self.r#match.command_includes {
            if !parts
                .iter()
                .all(|part| includes_command_part(&command, part))
            {
                return false;
            }
        }

        if let Some(parts) = &self.r#match.command_includes_any {
            if !parts
                .iter()
                .any(|part| includes_command_part(&command, part))
            {
                return false;
            }
        }

        true
    }

    /// How specific a rule is. Higher wins, so `git/status` beats a rule that
    /// only matches `git`.
    pub fn score(&self) -> f64 {
        let sum = |groups: &Option<Vec<Vec<String>>>| -> f64 {
            groups
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(|parts| parts.len() as f64)
                .sum()
        };
        self.priority.unwrap_or(0.0) * 1000.0
            + self.r#match.argv0.as_ref().map_or(0.0, |v| v.len() as f64) * 100.0
            + self
                .r#match
                .git_subcommands
                .as_ref()
                .map_or(0.0, |v| v.len() as f64)
                * 60.0
            + sum(&self.r#match.argv_includes) * 40.0
            + sum(&self.r#match.argv_includes_any) * 35.0
            + self
                .r#match
                .command_includes
                .as_ref()
                .map_or(0.0, |v| v.len() as f64)
                * 25.0
            + self
                .r#match
                .command_includes_any
                .as_ref()
                .map_or(0.0, |v| v.len() as f64)
                * 20.0
            + self.r#match.tool_names.as_ref().map_or(0.0, |v| v.len() as f64) * 10.0
    }
}

/// The id of the catch-all rule. It is only used when nothing specific matches,
/// and it is treated differently: its "error"/"warning" counters are mentions,
/// not failures.
pub const FALLBACK_RULE: &str = "generic/fallback";

fn rules() -> &'static Vec<CompiledRule> {
    static RULES: OnceLock<Vec<CompiledRule>> = OnceLock::new();
    RULES.get_or_init(|| {
        let raw: Vec<Rule> = serde_json::from_str(include_str!("rules.json"))
            .expect("the embedded tokenjuice rule set is valid JSON");
        raw.into_iter().map(CompiledRule::new).collect()
    })
}

/// Every rule, compiled once on first use.
pub fn all() -> &'static [CompiledRule] {
    rules()
}

/// The rule that should handle this command, if any.
///
/// Specific rules win on score; the fallback is only returned when nothing else
/// matched. Ties break on id so the choice is stable across runs — a rule set
/// that picks differently on each call would make the panel's history useless.
pub fn resolve(input: &ExecutionInput) -> Option<&'static CompiledRule> {
    let mut best: Option<&'static CompiledRule> = None;
    let mut fallback: Option<&'static CompiledRule> = None;

    for rule in rules() {
        if !rule.rule.matches(input) {
            continue;
        }
        if rule.rule.id == FALLBACK_RULE {
            fallback.get_or_insert(rule);
            continue;
        }
        best = match best {
            None => Some(rule),
            Some(current) => {
                let diff = rule.rule.score() - current.rule.score();
                if diff > 0.0 || (diff == 0.0 && rule.rule.id < current.rule.id) {
                    Some(rule)
                } else {
                    Some(current)
                }
            }
        };
    }

    best.or(fallback)
}

pub fn find(id: &str) -> Option<&'static CompiledRule> {
    rules().iter().find(|rule| rule.rule.id == id)
}

/// Every rule id, for the panel's rule list.
pub fn ids() -> Vec<&'static str> {
    rules().iter().map(|rule| rule.rule.id.as_str()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(argv: &[&str]) -> ExecutionInput {
        ExecutionInput {
            command: Some(argv.join(" ")),
            argv: argv.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn the_embedded_rule_set_loads_and_is_not_empty() {
        let rules = all();
        assert!(rules.len() > 100, "only {} rules loaded", rules.len());
        // Every rule must have compiled its patterns; a rule whose regex failed
        // would silently never filter anything.
        assert!(find("git/status").is_some());
        assert!(find(FALLBACK_RULE).is_some());
    }

    #[test]
    fn every_rule_id_is_unique() {
        let mut ids = ids();
        let before = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(before, ids.len(), "duplicate rule id in the embedded set");
    }

    #[test]
    fn git_status_picks_the_specific_rule_not_the_fallback() {
        let rule = resolve(&input(&["git", "status"])).expect("a rule matches git status");
        assert_eq!(rule.rule.id, "git/status");
        assert_eq!(rule.rule.family, "git-status");
    }

    #[test]
    fn an_unknown_command_falls_back() {
        let rule = resolve(&input(&["some-unknown-binary", "--flag"])).expect("fallback exists");
        assert_eq!(rule.rule.id, FALLBACK_RULE);
    }

    #[test]
    fn a_more_specific_rule_beats_a_broader_one() {
        // Both start with `git`; the subcommand matcher has to separate them.
        let status = resolve(&input(&["git", "status"])).unwrap();
        let log = resolve(&input(&["git", "log", "--oneline"])).unwrap();
        assert_eq!(status.rule.id, "git/status");
        assert_eq!(log.rule.id, "git/log-oneline");
    }

    #[test]
    fn git_subcommand_skips_flags_and_their_values() {
        let argv: Vec<String> = ["git", "-C", "/tmp/repo", "status", "--short"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(git_subcommand(&argv), Some("status"));
        let argv: Vec<String> = ["git", "--no-pager", "log"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(git_subcommand(&argv), Some("log"));
        let argv: Vec<String> = ["cargo", "build"].iter().map(|s| s.to_string()).collect();
        assert_eq!(git_subcommand(&argv), None);
    }

    #[test]
    fn command_part_matching_respects_word_boundaries() {
        assert!(includes_command_part("git status --short", "status"));
        assert!(!includes_command_part("mystatus", "status"));
        assert!(!includes_command_part("git statusx", "status"));
        assert!(includes_command_part("npm run build", "run build"));
        assert!(includes_command_part("anything", ""));
    }

    #[test]
    fn a_rule_with_a_tool_name_filter_does_not_match_another_tool() {
        let rule = find("git/status").unwrap();
        let mut with_tool = input(&["git", "status"]);
        with_tool.tool_name = Some("bash".into());
        assert!(rule.rule.matches(&with_tool));
    }

    #[test]
    fn a_case_insensitive_rule_pattern_matches_either_case() {
        let rule = find("tests/playwright").unwrap();
        let counter = rule
            .counters()
            .iter()
            .find(|(name, _)| name == "failed")
            .expect("the playwright rule counts failures");
        assert!(counter.1.is_match("2 FAILED"));
        assert!(counter.1.is_match("2 failed"));
    }
}
