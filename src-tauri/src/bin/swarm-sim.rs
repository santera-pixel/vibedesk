//! A scripted ACP agent, so the swarm's drive loop can be proven without a model.
//!
//! The engine spawns one `vibe-acp` per member through `paths::resolve_acp_binary`,
//! which honours `VIBEDESK_ACP_BIN`. Point that at this binary and every member
//! becomes a scripted agent: the real drive loop, the real worktrees, the real
//! git, the real two-tree proof, the real integration gate, and no tokens.
//!
//! That is the whole point. Before this, `drive()` was 1457 lines no test could
//! reach, so the suite proved every helper and skipped the assembly. A run that
//! could not finish was green.
//!
//! The script is a JSON file named by `VIBEDESK_SIM_SCRIPT`. Each member gets a
//! list of steps; each `session/prompt` consumes the next one. A step may write
//! files and run shell commands in the member's own working directory.
//!
//! The member's identity comes from its brief, which names its inbox. That is not
//! a shortcut: the brief is the only thing a real agent has to go on either.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};

fn main() {
    let script = load_script();
    let state = state_dir();
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();

    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        // A notification has no id and needs no answer. `session/cancel` is the
        // one that arrives, and a scripted agent has nothing to abandon.
        let Some(id) = message.get("id").cloned() else {
            continue;
        };
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let params = message.get("params").cloned().unwrap_or(Value::Null);

        let result = match method.as_str() {
            "initialize" => json!({
                "protocolVersion": 1,
                "agentInfo": { "name": "swarm-sim", "version": "sim-1" }
            }),
            "session/new" => json!({
                "sessionId": format!("sim-{}", std::process::id()),
                "configOptions": [
                    {
                        "id": "model",
                        "currentValue": "sim",
                        "options": [{ "value": "sim", "name": "sim" }]
                    },
                    {
                        "id": "mode",
                        "currentValue": "auto",
                        "options": [{ "value": "auto", "name": "auto" }]
                    }
                ]
            }),
            "session/set_config_option" => json!({ "configOptions": [] }),
            "session/prompt" => {
                let text = prompt_text(&params);
                run_step(&script, &state, &text);
                json!({
                    "usage": {
                        "inputTokens": 100,
                        "outputTokens": 20,
                        "cachedReadTokens": 80,
                        "totalTokens": 120
                    }
                })
            }
            _ => json!({}),
        };

        let reply = json!({ "jsonrpc": "2.0", "id": id, "result": result });
        if writeln!(out, "{reply}").is_err() {
            break;
        }
        let _ = out.flush();
    }
}

fn load_script() -> Value {
    let path = std::env::var("VIBEDESK_SIM_SCRIPT").unwrap_or_default();
    let body = std::fs::read_to_string(&path).unwrap_or_default();
    serde_json::from_str(&body).unwrap_or_else(|_| json!({ "members": {} }))
}

/// Where the per-member step counters live.
///
/// A file rather than memory because each member is its own process: the counter
/// has to survive the process that increments it.
fn state_dir() -> PathBuf {
    let dir = std::env::var("VIBEDESK_SIM_STATE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("vibedesk-sim"));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn prompt_text(params: &Value) -> String {
    params
        .get("prompt")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// The member's inbox directory, taken from the brief that names it.
///
/// The brief writes `` `<inbox>/plan.json` ``, so the path runs past the member.
/// The member is the segment after `inbox`, not the last segment: taking the last
/// one keys the script on `plan.json` and every step silently does nothing.
///
/// The occurrence has to be preceded by a separator, because the brief also uses
/// the word in prose and the first match is not always the path.
fn inbox_from_prompt(prompt: &str) -> Option<String> {
    let bytes = prompt.as_bytes();
    let at = prompt.match_indices("inbox").find_map(|(at, _)| {
        let before = at.checked_sub(1).map(|i| bytes[i]);
        matches!(before, Some(b'/') | Some(b'\\')).then_some(at)
    })?;

    let mut start = at;
    while start > 0 {
        let ch = bytes[start - 1];
        if ch.is_ascii_whitespace() || ch == b'`' || ch == b'"' || ch == b'\'' || ch == b'(' {
            break;
        }
        start -= 1;
    }

    // Forward past `inbox` and the one segment that names the member.
    let mut end = at + "inbox".len();
    let mut separators = 0;
    while end < bytes.len() {
        let ch = bytes[end];
        if ch.is_ascii_whitespace() || ch == b'`' || ch == b'"' || ch == b'\'' || ch == b')' {
            break;
        }
        if ch == b'/' || ch == b'\\' {
            if separators == 1 {
                break;
            }
            separators += 1;
        }
        end += 1;
    }
    Some(prompt[start..end].to_string())
}

/// The member id, which is the last segment of its inbox directory.
fn member_from_inbox(inbox: &str) -> String {
    inbox
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("unknown")
        .to_string()
}

fn run_step(script: &Value, state: &Path, prompt: &str) {
    let Some(inbox) = inbox_from_prompt(prompt) else {
        eprintln!("sim: no inbox path in the brief, nothing to do");
        return;
    };
    let member = member_from_inbox(&inbox);
    let Some(steps) = script
        .get("members")
        .and_then(|m| m.get(&member))
        .and_then(Value::as_array)
    else {
        eprintln!("sim: {member} has no script");
        return;
    };

    let counter = state.join(format!("{member}.n"));
    let index: usize = std::fs::read_to_string(&counter)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    let Some(step) = steps.get(index) else {
        eprintln!("sim: {member} is out of steps at {index}");
        return;
    };
    let _ = std::fs::write(&counter, (index + 1).to_string());
    eprintln!("sim: {member} step {index}");

    if let Some(files) = step.get("files").and_then(Value::as_object) {
        for (path, body) in files {
            let path = path.replace("<inbox>", &inbox);
            if let Some(parent) = Path::new(&path).parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let text = match body.as_str() {
                Some(text) => text.to_string(),
                None => body.to_string(),
            };
            let _ = std::fs::write(&path, text);
        }
    }

    if let Some(commands) = step.get("sh").and_then(Value::as_array) {
        let cwd = std::env::current_dir().unwrap_or_default();
        for command in commands.iter().filter_map(Value::as_str) {
            let _ = Command::new(bash())
                .arg("-c")
                .arg(command)
                .current_dir(&cwd)
                .output();
        }
    }
}

/// The shell the engine's own proof uses, so a scripted criterion behaves the
/// same way it will when a real verifier runs it.
fn bash() -> String {
    if cfg!(windows) {
        for candidate in [
            "C:/Program Files/Git/bin/bash.exe",
            "C:/Program Files/Git/usr/bin/bash.exe",
            "C:/Program Files (x86)/Git/bin/bash.exe",
        ] {
            if Path::new(candidate).is_file() {
                return candidate.to_string();
            }
        }
    }
    "bash".to_string()
}
