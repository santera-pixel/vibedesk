//! A minimal MCP stdio client.
//!
//! VibeDesk registers MCP servers for the agent; this is the other direction.
//! The panels need to ask a server what it knows — which projects are indexed,
//! how many nodes and edges they hold — and the only way to ask is the protocol
//! the server already speaks.
//!
//! Deliberately small: `initialize`, then `tools/call`. No resources, no
//! prompts, no sampling. A server that needs more than that is a server the
//! panel has no question for.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use serde_json::{json, Value};

use crate::error::{AppError, AppResult};

/// A server that cannot answer a handshake in this long is not going to.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);

/// A tool call that takes longer than this is indexing, not answering a panel.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// One running MCP server, spoken to over stdio.
///
/// The child is killed on drop, so a panel that stops asking does not leave a
/// 300 MB binary resident.
pub struct McpClient {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    next_id: u64,
}

impl McpClient {
    pub fn start(command: &str, args: &[String], cwd: Option<&Path>) -> AppResult<Self> {
        let mut cmd = Command::new(command);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if let Some(cwd) = cwd {
            cmd.current_dir(cwd);
        }

        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }

        let mut child = cmd
            .spawn()
            .map_err(|error| AppError::message(format!("could not start {command}: {error}")))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| AppError::message("the server has no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AppError::message("the server has no stdout"))?;

        // A reader thread, because a blocking read on the child's stdout cannot
        // be given a deadline and a panel must never hang on a wedged server.
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(line) => {
                        if tx.send(line).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(Self {
            child,
            stdin,
            lines: rx,
            next_id: 1,
        })
    }

    /// The MCP handshake, including the `initialized` notification the spec
    /// requires before any request.
    pub fn initialize(&mut self) -> AppResult<Value> {
        let id = self.take_id();
        self.write(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {
                    "name": "vibedesk",
                    "version": env!("CARGO_PKG_VERSION"),
                },
            },
        }))?;
        let result = self.await_response(id, HANDSHAKE_TIMEOUT)?;
        self.write(&json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {},
        }))?;
        Ok(result)
    }

    /// Call one tool and return its text content.
    ///
    /// A tool that reports `isError` becomes an `Err` carrying the server's own
    /// message, because that message is the useful part — "missing required
    /// argument: project" tells the caller what to fix.
    pub fn call_tool(&mut self, tool: &str, arguments: Value) -> AppResult<String> {
        let id = self.take_id();
        self.write(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": tool, "arguments": arguments },
        }))?;
        let result = self.await_response(id, CALL_TIMEOUT)?;

        if result.get("isError").and_then(Value::as_bool) == Some(true) {
            let text = text_content(&result);
            return Err(AppError::message(if text.is_empty() {
                format!("{tool} reported an error")
            } else {
                text
            }));
        }

        Ok(text_content(&result))
    }

    fn take_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn write(&mut self, message: &Value) -> AppResult<()> {
        let mut line = serde_json::to_string(message)
            .map_err(|error| AppError::message(format!("could not encode a request: {error}")))?;
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .and_then(|_| self.stdin.flush())
            .map_err(|error| AppError::message(format!("could not write to the server: {error}")))
    }

    fn await_response(&mut self, id: u64, timeout: Duration) -> AppResult<Value> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(AppError::message("the server did not answer in time"));
            }

            let line = self
                .lines
                .recv_timeout(remaining)
                .map_err(|_| AppError::message("the server did not answer in time"))?;

            // Notifications and unrelated responses share the stream; only the
            // id we asked for is an answer.
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if message.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }

            if let Some(error) = message.get("error") {
                let text = error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("the server refused the request");
                return Err(AppError::message(text.to_string()));
            }

            return Ok(message.get("result").cloned().unwrap_or(Value::Null));
        }
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The text blocks of a `tools/call` result, joined by newlines.
pub fn text_content(result: &Value) -> String {
    let Some(blocks) = result.get("content").and_then(Value::as_array) else {
        return String::new();
    };
    blocks
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_content_joins_only_text_blocks() {
        let result = json!({
            "content": [
                { "type": "text", "text": "first" },
                { "type": "image", "data": "..." },
                { "type": "text", "text": "second" },
            ]
        });
        assert_eq!(text_content(&result), "first\nsecond");
    }

    #[test]
    fn a_result_without_content_is_empty_not_a_panic() {
        assert_eq!(text_content(&json!({})), "");
        assert_eq!(text_content(&json!({ "content": [] })), "");
    }

    #[test]
    fn a_binary_that_does_not_exist_is_an_error_not_a_panic() {
        let started = McpClient::start("vibedesk-definitely-not-a-binary", &[], None);
        assert!(started.is_err());
    }
}
