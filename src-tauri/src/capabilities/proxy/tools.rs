//! Tool-result interception, at the one boundary the proxy always owns.
//!
//! Reading and Terminal were built against ACP boundaries — `fs/read_text_file`
//! and `terminal/output` — on the assumption that an agent offered client file
//! and shell tools will use them. It does not. Vibe ships its own `read_file`
//! and `bash`, calls those, and never sends a single `fs/*` or `terminal/*`
//! request. A capability wired to a boundary the agent never crosses is a
//! capability that never runs, whatever the panel says.
//!
//! The proxy is different. Every token the agent sends upstream passes through
//! it, including the results of the agent's own tools. So the honest place to
//! apply SparseReading and tokenjuice is here: on the tool result, on its way
//! back to the model.
//!
//! ## Why the document, not the decoded conversation
//!
//! This rewrites the request body in place rather than decoding it to
//! [`CoreMessage`]s and re-encoding. Re-encoding is lossy in ways that matter:
//! `encode_anthropic` rebuilds content blocks from scratch, so a `cache_control`
//! marker on a block would be dropped and the user's prompt cache would go
//! cold on every request. Touching only the field this module means to touch
//! keeps the rest of the body byte-identical.
//!
//! ## What this does not do
//!
//! It does not invent a tool, rename one, or hide one. The agent asked for a
//! file and gets a file — or, when the gate says the file is a long document
//! worth mapping first, a card that names the exact `offset`/`limit` calls that
//! fetch the bytes. The agent's own paging parameters are the escape hatch, and
//! a request that already carries them is never second-guessed.
//!
//! A result whose shape this does not recognise is left exactly as it is.
//! Guessing at an unknown shape is how a capability corrupts a turn it was
//! supposed to help.

use std::collections::HashMap;
use std::path::Path;

use serde_json::Value;

use super::super::compact::types::estimate_tokens_chars;
use super::super::compact::wire::Dialect;
use super::super::{now, registry, CapabilityEvent, CapabilityId};

/// Tool names whose result is a file's contents.
///
/// The first is what Vibe actually sends; the rest are the names other agents
/// use for the same thing, so the boundary keeps working if the agent changes.
const READ_TOOLS: [&str; 6] = [
    "read_file",
    "read",
    "read_text_file",
    "view_file",
    "cat_file",
    "Read",
];

/// Tool names whose result is a command's output.
const SHELL_TOOLS: [&str; 7] = [
    "bash",
    "execute",
    "shell",
    "run_command",
    "terminal",
    "exec",
    "Bash",
];

/// A rewrite that saves less than this is not worth applying.
///
/// The engine's rule is "never make it worse", which lets a 74-character output
/// through as a 73-character one — one trailing newline trimmed. That is not
/// worse, but it is not worth anything either: it costs the agent a re-read of
/// text it already had, and it puts a meaningless row in the panel. The
/// boundary is where "is this worth doing" belongs, so the floor lives here.
const MIN_SAVED_CHARS: usize = 40;

/// What one pass over a request changed.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    /// Reads the gate carded.
    pub reads_intercepted: usize,
    /// Reads the gate let through, counted so the panel can show the ratio.
    pub reads_passed: usize,
    /// Reads the agent paged itself, which is the card's escape hatch working.
    pub reads_paged: usize,
    /// Command outputs tokenjuice rewrote.
    pub commands_compacted: usize,
    /// Command outputs it left alone.
    pub commands_passed: usize,
    /// Results that were not the exec shape and were clamped to the budget.
    pub plain_clamped: usize,
    /// Characters removed from the request.
    pub saved_chars: usize,
}

impl Report {
    pub fn touched(&self) -> bool {
        self.reads_intercepted > 0 || self.commands_compacted > 0 || self.plain_clamped > 0
    }
}

/// Tool call ids already counted, so a multi-round turn counts a result once.
///
/// The agent re-sends the whole conversation on every round of a turn, so the
/// same tool result arrives at this boundary several times. The rewrite has to
/// happen every time — each round is a fresh body — but the *statistics* must
/// not: a panel that reports three reads for one file is lying about the work.
fn already_counted(id: &str) -> bool {
    const CEILING: usize = 4096;
    static SEEN: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    let seen = SEEN.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()));
    let mut seen = seen.lock().unwrap_or_else(|error| error.into_inner());
    if seen.len() >= CEILING {
        seen.clear();
    }
    !seen.insert(id.to_string())
}

/// One tool call, as the request declared it.
struct Call {
    name: String,
    arguments: Value,
}

/// Rewrite tool results in the request body, in place.
///
/// Runs before the kernel decodes the conversation, so compaction measures the
/// payload that actually goes upstream rather than the one the agent produced.
pub fn intercept(dialect: Dialect, document: &mut Value) -> Report {
    let mut report = Report::default();
    let reading = registry().is_enabled(CapabilityId::Reading);
    let terminal = registry().is_enabled(CapabilityId::Terminal);
    if !reading && !terminal {
        return report;
    }

    let calls = match dialect {
        Dialect::OpenAi => openai_calls(document),
        Dialect::Anthropic => anthropic_calls(document),
    };
    if calls.is_empty() {
        return report;
    }

    let Some(messages) = document.get_mut("messages").and_then(Value::as_array_mut) else {
        return report;
    };

    for message in messages.iter_mut() {
        match dialect {
            Dialect::OpenAi => openai_results(message, &calls, reading, terminal, &mut report),
            Dialect::Anthropic => {
                anthropic_results(message, &calls, reading, terminal, &mut report)
            }
        }
    }

    report
}

/// Index the calls an OpenAI-shaped request declared.
fn openai_calls(document: &Value) -> HashMap<String, Call> {
    let mut index = HashMap::new();
    let Some(messages) = document.get("messages").and_then(Value::as_array) else {
        return index;
    };
    for message in messages {
        let Some(calls) = message.get("tool_calls").and_then(Value::as_array) else {
            continue;
        };
        for call in calls {
            let Some(id) = call.get("id").and_then(Value::as_str) else {
                continue;
            };
            let function = call.get("function");
            let name = function
                .and_then(|f| f.get("name"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            let arguments = function
                .and_then(|f| f.get("arguments"))
                .map(parse_arguments)
                .unwrap_or(Value::Null);
            index.insert(
                id.to_string(),
                Call {
                    name: name.to_string(),
                    arguments,
                },
            );
        }
    }
    index
}

/// Index the calls an Anthropic-shaped request declared.
fn anthropic_calls(document: &Value) -> HashMap<String, Call> {
    let mut index = HashMap::new();
    let Some(messages) = document.get("messages").and_then(Value::as_array) else {
        return index;
    };
    for message in messages {
        let Some(blocks) = message.get("content").and_then(Value::as_array) else {
            continue;
        };
        for block in blocks {
            if block.get("type").and_then(Value::as_str) != Some("tool_use") {
                continue;
            }
            let Some(id) = block.get("id").and_then(Value::as_str) else {
                continue;
            };
            index.insert(
                id.to_string(),
                Call {
                    name: block
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    arguments: block.get("input").cloned().unwrap_or(Value::Null),
                },
            );
        }
    }
    index
}

/// `arguments` is a JSON string on the OpenAI wire and an object on Anthropic's.
fn parse_arguments(value: &Value) -> Value {
    match value {
        Value::String(text) => serde_json::from_str(text).unwrap_or(Value::Null),
        other => other.clone(),
    }
}

fn openai_results(
    message: &mut Value,
    calls: &HashMap<String, Call>,
    reading: bool,
    terminal: bool,
    report: &mut Report,
) {
    if message.get("role").and_then(Value::as_str) != Some("tool") {
        return;
    }
    let Some(id) = message
        .get("tool_call_id")
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        return;
    };
    let Some(call) = calls.get(&id) else {
        return;
    };
    let Some(content) = message.get_mut("content") else {
        return;
    };
    rewrite_payload(content, &id, call, reading, terminal, report);
}

fn anthropic_results(
    message: &mut Value,
    calls: &HashMap<String, Call>,
    reading: bool,
    terminal: bool,
    report: &mut Report,
) {
    let Some(blocks) = message.get_mut("content").and_then(Value::as_array_mut) else {
        return;
    };
    for block in blocks.iter_mut() {
        if block.get("type").and_then(Value::as_str) != Some("tool_result") {
            continue;
        }
        let Some(id) = block
            .get("tool_use_id")
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            continue;
        };
        let Some(call) = calls.get(&id) else {
            continue;
        };
        let Some(content) = block.get_mut("content") else {
            continue;
        };
        rewrite_payload(content, &id, call, reading, terminal, report);
    }
}

/// Apply the capability that owns this tool to one result payload.
///
/// The payload is a string on both dialects — OpenAI's `content`, Anthropic's
/// `tool_result.content` when it is not a block list. A block list is only
/// rewritten when it is a single text block, because that is the only shape
/// where "replace the text" is unambiguous.
fn rewrite_payload(
    content: &mut Value,
    id: &str,
    call: &Call,
    reading: bool,
    terminal: bool,
    report: &mut Report,
) {
    match content {
        Value::String(text) => {
            if let Some(next) = rewrite_text(text, id, call, reading, terminal, report) {
                *text = next;
            }
        }
        Value::Array(blocks) => {
            if blocks.len() != 1 {
                return;
            }
            let Some(block) = blocks.first_mut() else {
                return;
            };
            if block.get("type").and_then(Value::as_str) != Some("text") {
                return;
            }
            let Some(text) = block.get_mut("text").and_then(|value| value.as_str()) else {
                return;
            };
            let current = text.to_string();
            if let Some(next) = rewrite_text(&current, id, call, reading, terminal, report) {
                if let Some(slot) = block.get_mut("text") {
                    *slot = Value::String(next);
                }
            }
        }
        _ => {}
    }
}

/// The one place a tool result is actually rewritten.
fn rewrite_text(
    text: &str,
    id: &str,
    call: &Call,
    reading: bool,
    terminal: bool,
    report: &mut Report,
) -> Option<String> {
    if reading && is_read_tool(&call.name) {
        return rewrite_read(text, id, &call.name, &call.arguments, report);
    }
    if terminal && is_shell_tool(&call.name) {
        return rewrite_command(text, id, &call.arguments, report);
    }
    None
}

fn is_read_tool(name: &str) -> bool {
    READ_TOOLS.iter().any(|known| known.eq_ignore_ascii_case(name))
}

fn is_shell_tool(name: &str) -> bool {
    SHELL_TOOLS.iter().any(|known| known.eq_ignore_ascii_case(name))
}

/// The path a read tool was pointed at.
fn read_path(arguments: &Value) -> Option<String> {
    for key in ["path", "file_path", "filePath", "filename"] {
        if let Some(path) = arguments.get(key).and_then(Value::as_str) {
            return Some(path.to_string());
        }
    }
    None
}

/// True when the agent asked for a slice rather than the whole file.
///
/// A paged read is the agent asking for exact bytes, which is the whole point of
/// the card. Second-guessing it would break the loop the card exists to close.
fn is_paged(arguments: &Value) -> bool {
    ["offset", "limit", "line", "start_line", "end_line"]
        .iter()
        .any(|key| arguments.get(key).is_some_and(|value| !value.is_null()))
}

fn rewrite_read(
    text: &str,
    id: &str,
    call_name: &str,
    arguments: &Value,
    report: &mut Report,
) -> Option<String> {
    if is_paged(arguments) {
        if !already_counted(id) {
            report.reads_paged += 1;
            registry().bump(CapabilityId::Reading, "paged", 1);
        }
        return None;
    }
    let path = read_path(arguments)?;
    let mut result = serde_json::from_str::<Value>(text).ok()?;
    let content = result.get("content").and_then(Value::as_str)?.to_string();

    // The card ends with the calls that fetch the bytes, so it has to name the
    // tool this agent actually has. Vibe's `read_file` takes a zero-based
    // `offset`; ACP's `fs/read_text_file` takes a one-based `line`.
    registry().note_read_tool(call_name);
    let tool = super::super::sparse::ReadTool::agent(call_name);
    let outcome = super::super::sparse::apply_with(Path::new(&path), content, false, &tool);
    if !outcome.intercepted {
        if !already_counted(id) {
            report.reads_passed += 1;
            registry().bump(CapabilityId::Reading, "passed", 1);
        }
        return None;
    }

    *result.get_mut("content")? = Value::String(outcome.content.clone());
    if !already_counted(id) {
        report.reads_intercepted += 1;
        report.saved_chars += outcome.saved_chars();
        record_reading(&path, &outcome);
    }
    Some(result.to_string())
}

fn record_reading(path: &str, outcome: &super::super::sparse::ReadOutcome) {
    let registry = registry();
    registry.bump(CapabilityId::Reading, outcome.decision.action(), 1);
    registry.bump(CapabilityId::Reading, outcome.decision.code, 1);
    for rule in &outcome.denoise_rules {
        registry.bump(CapabilityId::Reading, rule, 1);
    }
    let name = Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string());
    registry.record(
        CapabilityId::Reading,
        CapabilityEvent {
            at: now(),
            label: outcome.decision.code.to_string(),
            detail: format!("{name} — {}", outcome.decision.reason),
            bytes_in: outcome.raw_bytes,
            bytes_out: outcome.out_bytes,
            tokens_in: estimate_tokens_chars(outcome.raw_chars as u64),
            tokens_out: estimate_tokens_chars(outcome.out_chars as u64),
        },
    );
}

/// The file a command dumps whole, when it is a whole-file dump.
///
/// `cat README.md` is a read. It is the same read the agent's `read_file` tool
/// would have made, and it is the read the gate exists to intercept — so a card
/// that only guards `read_file` is a card the agent walks around with one shell
/// command. This recognises the narrow case: a single command, no pipes or
/// redirects, whose whole purpose is to print one file's bytes.
///
/// Anything with a range — `sed -n '28,35p'`, `head -20`, `tail -5` — is the
/// agent asking for a slice, which is the card's own escape hatch, and is left
/// alone. So is anything with a pipe, because the output is then not the file.
/// Drop a leading `cd <dir> &&` from a command line.
///
/// The agent's shell tool runs every command in a working directory, and it
/// spells that as `cd /path && <command>`. Without this, every command the agent
/// ever runs contains `&&`, and a guard that refuses anything with `&&` never
/// fires — which is exactly how the first version of this check was unreachable
/// in practice while passing its own tests.
fn strip_cd_prefix(command: &str) -> &str {
    let trimmed = command.trim();
    let Some(rest) = trimmed.strip_prefix("cd ") else {
        return trimmed;
    };
    let Some((_, tail)) = rest.split_once("&&") else {
        return trimmed;
    };
    tail.trim()
}

fn whole_file_dump(command: &str) -> Option<String> {
    let command = strip_cd_prefix(command);
    const SHELL_META: [&str; 9] = ["|", ">", "<", ";", "&&", "||", "$", "`", "&"];
    if SHELL_META.iter().any(|meta| command.contains(meta)) {
        return None;
    }
    let tokens: Vec<&str> = command.split_whitespace().collect();
    let (program, rest) = tokens.split_first()?;
    let program = program.rsplit(['/', '\\']).next().unwrap_or(program);
    let program = program.strip_suffix(".exe").unwrap_or(program);
    let whole_file = matches!(program, "cat" | "type" | "Get-Content" | "gc");
    if !whole_file {
        return None;
    }
    // A flag changes what is printed — `cat -n` numbers the lines, `cat -A`
    // shows the control characters — so the output is no longer the file.
    if rest.iter().any(|token| token.starts_with('-')) {
        return None;
    }
    if rest.len() != 1 {
        return None;
    }
    let path = rest[0];
    if !path.contains('.') && !path.contains('/') && !path.contains('\\') {
        return None;
    }
    Some(path.to_string())
}

fn rewrite_command(
    text: &str,
    id: &str,
    arguments: &Value,
    report: &mut Report,
) -> Option<String> {
    let Some(mut result) = serde_json::from_str::<Value>(text).ok() else {
        return rewrite_plain(text, id, terminal_options().max_inline_chars, report);
    };
    let stdout = result
        .get("stdout")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let stderr = result
        .get("stderr")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if stdout.is_empty() && stderr.is_empty() {
        return None;
    }

    let command = arguments
        .get("command")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            result
                .get("command")
                .and_then(Value::as_str)
                .map(str::to_string)
        });

    // A whole-file dump through the shell is a read, and the reading gate owns
    // reads. Without this, `cat README.md` is a one-command way around the card.
    if let Some(path) = command.as_deref().and_then(whole_file_dump) {
        if let Some(next) = rewrite_shell_read(&stdout, id, &path, report) {
            *result.get_mut("stdout")? = Value::String(next);
            return Some(result.to_string());
        }
    }

    let input = super::super::tokenjuice::ExecutionInput {
        argv: command
            .as_deref()
            .map(|line| line.split_whitespace().map(str::to_string).collect())
            .unwrap_or_default(),
        command,
        exit_code: result
            .get("returncode")
            .and_then(Value::as_i64)
            .map(|code| code as i32),
        stdout,
        stderr,
        tool_name: None,
    };

    let compacted = super::super::tokenjuice::compact(&input, &terminal_options());
    if !compacted.compacted || compacted.saved_chars() < MIN_SAVED_CHARS {
        if !already_counted(id) {
            report.commands_passed += 1;
            registry().bump(CapabilityId::Terminal, "passed", 1);
        }
        return None;
    }

    // The engine compacts stdout and stderr as one block, the way a terminal
    // shows them. Writing the result back as a single `stdout` keeps that
    // ordering; leaving a stale `stderr` beside it would show the agent the
    // noise twice.
    *result.get_mut("stdout")? = Value::String(compacted.text.clone());
    if let Some(slot) = result.get_mut("stderr") {
        *slot = Value::String(String::new());
    }

    if !already_counted(id) {
        report.commands_compacted += 1;
        report.saved_chars += compacted.saved_chars();
        record_terminal(&compacted);
    }
    Some(result.to_string())
}

/// Shrink a shell result that is not the exec shape.
///
/// The CLI offloads a very large tool result to a file and hands the model a
/// plain-text notice naming the path. That notice is not `{command, stdout,
/// stderr, returncode}`, so it never matched the exec shape and the terminal
/// budget never applied to it — the one result most in need of shrinking was
/// the one result the boundary could not reach. It is clamped from the middle
/// rather than the end because both ends carry meaning: the head says which tool
/// produced it and the tail carries the path the model has to read.
fn rewrite_plain(text: &str, id: &str, budget: usize, report: &mut Report) -> Option<String> {
    if budget == 0 {
        return None;
    }
    let before = text.chars().count();
    if before <= budget {
        return None;
    }
    let clamped = super::super::tokenjuice::text::clamp_text_middle(text, budget);
    let after = clamped.chars().count();
    if after >= before {
        return None;
    }
    if !already_counted(id) {
        report.plain_clamped += 1;
        report.saved_chars += before - after;
        registry().bump(CapabilityId::Terminal, "clamped", 1);
    }
    Some(clamped)
}

/// Apply the reading gate to a file the agent dumped through its shell.
///
/// The path may be relative, and the proxy does not know the agent's working
/// directory, so the size comes from the bytes rather than from a `stat`.
fn rewrite_shell_read(
    stdout: &str,
    id: &str,
    path: &str,
    report: &mut Report,
) -> Option<String> {
    if stdout.trim().is_empty() {
        return None;
    }
    let tool = super::super::sparse::ReadTool::agent(&registry().read_tool());
    let outcome = super::super::sparse::apply_content(
        Path::new(path),
        stdout.to_string(),
        false,
        &tool,
    );
    if !outcome.intercepted {
        if !already_counted(id) {
            report.reads_passed += 1;
            registry().bump(CapabilityId::Reading, "passed", 1);
        }
        return None;
    }
    if !already_counted(id) {
        report.reads_intercepted += 1;
        report.saved_chars += outcome.saved_chars();
        record_reading(path, &outcome);
    }
    Some(outcome.content)
}

fn record_terminal(compacted: &super::super::tokenjuice::CompactResult) {
    let registry = registry();
    if let Some(rule) = compacted.rule_id.as_deref() {
        registry.bump(CapabilityId::Terminal, rule, 1);
    }
    registry.bump(CapabilityId::Terminal, &compacted.family, 1);
    for (fact, count) in &compacted.facts {
        registry.bump(CapabilityId::Terminal, fact, *count);
    }
    registry.record(
        CapabilityId::Terminal,
        CapabilityEvent {
            at: now(),
            label: compacted
                .rule_id
                .clone()
                .unwrap_or_else(|| "no rule".to_string()),
            detail: compacted.headline(),
            bytes_in: compacted.raw_chars as u64,
            bytes_out: compacted.compact_chars as u64,
            tokens_in: estimate_tokens_chars(compacted.raw_chars as u64),
            tokens_out: estimate_tokens_chars(compacted.compact_chars as u64),
        },
    );
}

fn terminal_options() -> super::super::tokenjuice::Options {
    let settings = super::super::settings::load().terminal;
    super::super::tokenjuice::Options {
        max_inline_chars: settings.max_inline_chars as usize,
        disabled_rules: settings.disabled_rules,
        passthrough_commands: settings.passthrough_commands,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn openai_body() -> Value {
        json!({
            "model": "m",
            "messages": [
                { "role": "user", "content": "read it" },
                {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "c1",
                        "type": "function",
                        "function": { "name": "read_file", "arguments": "{\"path\":\"C:/x/y.txt\"}" }
                    }]
                },
                { "role": "tool", "tool_call_id": "c1", "content": "{\"content\":\"hi\"}" }
            ]
        })
    }

    #[test]
    fn the_call_index_reads_the_arguments_the_agent_sent() {
        let calls = openai_calls(&openai_body());
        assert_eq!(calls["c1"].name, "read_file");
        assert_eq!(calls["c1"].arguments["path"], "C:/x/y.txt");
    }

    #[test]
    fn a_paged_read_is_never_second_guessed() {
        assert!(is_paged(&json!({ "path": "a", "offset": 10 })));
        assert!(is_paged(&json!({ "path": "a", "limit": 20 })));
        assert!(!is_paged(&json!({ "path": "a" })));
        assert!(!is_paged(&json!({ "path": "a", "offset": null })));
    }

    #[test]
    fn the_tool_names_vibe_actually_sends_are_recognised() {
        assert!(is_read_tool("read_file"));
        assert!(is_shell_tool("bash"));
        assert!(!is_read_tool("bash"));
        assert!(!is_shell_tool("read_file"));
    }

    #[test]
    fn a_result_that_does_not_parse_is_left_alone() {
        let mut body = openai_body();
        body["messages"][2]["content"] = json!("not json at all");
        let before = body.clone();
        let report = intercept(Dialect::OpenAi, &mut body);
        assert_eq!(body, before);
        assert_eq!(report, Report::default());
    }

    /// The CLI offloads a very large tool result to a file and hands the model a
    /// plain-text notice naming the path. That notice is not the exec shape, so
    /// before this the terminal budget never applied to it: the largest result
    /// in the request was the one result the boundary could not reach.
    #[test]
    fn an_oversized_plain_result_is_clamped_to_the_budget() {
        let notice = format!(
            "<system>The tool output is large and was saved to the file system under \
             C:/tmp/offload/abc.txt. Read it back with read_file.</system>\n{}",
            "x".repeat(20_000)
        );
        let mut report = Report::default();
        let out = rewrite_plain(&notice, "plain-1", 1_200, &mut report).expect("clamped");
        assert!(out.chars().count() <= 1_200, "{}", out.chars().count());
        assert!(out.starts_with("<system>The tool output is large"));
        assert!(out.contains("C:/tmp/offload/abc.txt"), "the path must survive");
        assert_eq!(report.plain_clamped, 1);
        assert_eq!(report.saved_chars, notice.chars().count() - out.chars().count());
        assert!(report.touched());
    }

    #[test]
    fn a_plain_result_inside_the_budget_is_left_alone() {
        let mut report = Report::default();
        assert!(rewrite_plain("short notice", "plain-2", 1_200, &mut report).is_none());
        assert_eq!(report, Report::default());
    }

    #[test]
    fn a_zero_budget_means_no_clamp_rather_than_clamp_to_nothing() {
        let mut report = Report::default();
        let text = "x".repeat(5_000);
        assert!(rewrite_plain(&text, "plain-3", 0, &mut report).is_none());
        assert_eq!(report, Report::default());
    }

    #[test]
    fn a_read_with_no_path_in_its_call_is_left_alone() {
        let mut body = openai_body();
        body["messages"][1]["tool_calls"][0]["function"]["arguments"] = json!("{}");
        let before = body.clone();
        intercept(Dialect::OpenAi, &mut body);
        assert_eq!(body, before);
    }

    #[test]
    fn a_result_with_no_matching_call_is_left_alone() {
        let mut body = openai_body();
        body["messages"][2]["tool_call_id"] = json!("orphan");
        let before = body.clone();
        intercept(Dialect::OpenAi, &mut body);
        assert_eq!(body, before);
    }

    #[test]
    fn a_body_with_no_tool_calls_is_untouched() {
        let mut body = json!({
            "model": "m",
            "messages": [{ "role": "user", "content": "hello" }]
        });
        let before = body.clone();
        let report = intercept(Dialect::OpenAi, &mut body);
        assert_eq!(body, before);
        assert_eq!(report, Report::default());
    }

    #[test]
    fn a_result_seen_twice_is_rewritten_twice_but_counted_once() {
        // The agent re-sends the whole conversation on every round of a turn,
        // so the same tool result arrives at this boundary again and again. The
        // rewrite has to happen each time — every round is a fresh body — but
        // the panel must not report three reads for one file.
        //
        // The id is unique to this test because the "already counted" set is
        // process-wide, and a shared id would make the result depend on which
        // test ran first.
        let id = "dedupe-read-1";
        let mut body = openai_body();
        body["messages"][1]["tool_calls"][0]["id"] = json!(id);
        body["messages"][2]["tool_call_id"] = json!(id);
        body["messages"][1]["tool_calls"][0]["function"]["arguments"] =
            json!("{\"path\":\"C:/x/long.md\"}");
        body["messages"][2]["content"] = json!(
            serde_json::json!({
                "content": "x".repeat(9000),
                "file_size_bytes": 9000
            })
            .to_string()
        );

        let mut first = body.clone();
        let first_report = intercept(Dialect::OpenAi, &mut first);
        let mut second = body.clone();
        let second_report = intercept(Dialect::OpenAi, &mut second);

        assert_eq!(first, second, "the rewrite must be identical every round");
        assert_eq!(
            second_report,
            Report::default(),
            "the second sighting must not be counted again"
        );
        assert!(
            first_report.reads_intercepted + first_report.reads_passed > 0,
            "the first sighting must be counted"
        );
    }

    #[test]
    fn the_anthropic_shape_is_indexed_from_tool_use_blocks() {
        let body = json!({
            "messages": [{
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": "t1",
                    "name": "bash",
                    "input": { "command": "ls" }
                }]
            }]
        });
        let calls = anthropic_calls(&body);
        assert_eq!(calls["t1"].name, "bash");
        assert_eq!(calls["t1"].arguments["command"], "ls");
    }

    #[test]
    fn an_anthropic_tool_result_the_gate_declines_is_untouched() {
        let mut body = json!({
            "messages": [
                {
                    "role": "assistant",
                    "content": [{
                        "type": "tool_use",
                        "id": "t1",
                        "name": "read_file",
                        "input": { "path": "C:/x/y.txt" }
                    }]
                },
                {
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": "t1",
                        "content": "{\"content\":\"hi\"}"
                    }]
                }
            ]
        });
        let before = body.clone();
        // The gate declines a two-character file, so nothing changes — which is
        // the point: the pass-through path must not rewrite the block either.
        intercept(Dialect::Anthropic, &mut body);
        assert_eq!(body, before);
    }

    #[test]
    fn a_multi_block_tool_result_is_left_alone() {
        let mut body = json!({
            "messages": [
                {
                    "role": "assistant",
                    "content": [{
                        "type": "tool_use",
                        "id": "t1",
                        "name": "read_file",
                        "input": { "path": "C:/x/y.txt" }
                    }]
                },
                {
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": "t1",
                        "content": [
                            { "type": "text", "text": "a" },
                            { "type": "text", "text": "b" }
                        ]
                    }]
                }
            ]
        });
        let before = body.clone();
        intercept(Dialect::Anthropic, &mut body);
        assert_eq!(body, before);
    }

    #[test]
    fn a_rewrite_that_saves_almost_nothing_is_not_applied() {
        // The engine's rule is "never make it worse", which lets a 74-character
        // output through as a 73-character one. That is not worse, but it is
        // not worth doing either, so the boundary declines it.
        let mut body = openai_body();
        body["messages"][1]["tool_calls"][0]["id"] = json!("floor-1");
        body["messages"][1]["tool_calls"][0]["function"]["name"] = json!("bash");
        body["messages"][1]["tool_calls"][0]["function"]["arguments"] =
            json!("{\"command\":\"pwd\"}");
        body["messages"][2]["tool_call_id"] = json!("floor-1");
        body["messages"][2]["content"] = json!(
            serde_json::json!({
                "command": "pwd",
                "returncode": 0,
                "stderr": "",
                "stdout": "/c/Users/Santera/AppData/Local/Temp/vd-game/skate
"
            })
            .to_string()
        );

        let before = body.clone();
        let report = intercept(Dialect::OpenAi, &mut body);
        assert_eq!(body, before, "a one-character saving must not be applied");
        assert_eq!(report.commands_compacted, 0);
    }

    #[test]
    fn a_whole_file_dump_through_the_shell_is_recognised() {
        assert_eq!(whole_file_dump("cat README.md").as_deref(), Some("README.md"));
        assert_eq!(
            whole_file_dump("cat /c/x/notes.md").as_deref(),
            Some("/c/x/notes.md")
        );
        assert_eq!(whole_file_dump("type C:/x/a.txt").as_deref(), Some("C:/x/a.txt"));
        assert_eq!(
            whole_file_dump("Get-Content ./spec.md").as_deref(),
            Some("./spec.md")
        );
    }

    #[test]
    fn a_slice_or_a_pipeline_is_not_a_whole_file_dump() {
        // A range is the card's own escape hatch, and a pipe means the output is
        // not the file. Neither may be carded.
        assert_eq!(whole_file_dump("sed -n '28,35p' README.md"), None);
        assert_eq!(whole_file_dump("head -20 README.md"), None);
        assert_eq!(whole_file_dump("tail -5 README.md"), None);
        assert_eq!(whole_file_dump("cat README.md | head -20"), None);
        assert_eq!(whole_file_dump("cat README.md > out.txt"), None);
        assert_eq!(whole_file_dump("cat -n README.md"), None);
        assert_eq!(whole_file_dump("cat a.md b.md"), None);
        assert_eq!(whole_file_dump("ls -la"), None);
        assert_eq!(whole_file_dump("cat"), None);
    }

    #[test]
    fn the_agents_cd_prefix_does_not_hide_a_whole_file_dump() {
        // The shell tool runs every command as `cd <dir> && <command>`. A guard
        // that refuses anything containing `&&` therefore never fires, which is
        // how the first version of this check was unreachable in practice while
        // passing its own tests.
        assert_eq!(
            whole_file_dump("cd /c/x/skate && cat README.md").as_deref(),
            Some("README.md")
        );
        assert_eq!(
            whole_file_dump("cd /c/x/skate && cat README.md | head -5"),
            None
        );
        assert_eq!(whole_file_dump("cd /c/x/skate && ls -la"), None);
        assert_eq!(strip_cd_prefix("cd /c/x && cat a.md"), "cat a.md");
        assert_eq!(strip_cd_prefix("cat a.md"), "cat a.md");
    }
}
