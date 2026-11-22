use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::{Mutex, Notify};

use crate::error::{AppError, AppResult};

/// Shrink one command's output before the agent reads it.
///
/// Two conditions gate this, and both matter. The process must have exited: a
/// rule that summarises "12 tests passed" is a lie about a run that is still
/// going. And the capability must be on: the raw bytes are gone once this
/// returns, so it is the user's call whether that trade is worth it.
async fn compact_output(terminal: &Terminal, text: String) -> String {
    use crate::capabilities::tokenjuice::{self, ExecutionInput, Options};
    use crate::capabilities::compact::types::estimate_tokens_chars;
    use crate::capabilities::{now, registry, CapabilityEvent, CapabilityId};

    if !registry().is_enabled(CapabilityId::Terminal) {
        return text;
    }
    let exit = *terminal.exit.lock().await;
    let Some(exit) = exit else {
        return text;
    };

    let input = ExecutionInput {
        command: Some(terminal.command.clone()),
        argv: terminal.argv.clone(),
        exit_code: exit.exit_code,
        stdout: text.clone(),
        stderr: String::new(),
        // The rules filter on `toolNames: ["exec"]`; this is that tool.
        tool_name: Some("exec".to_string()),
    };
    let result = tokenjuice::compact(&input, &Options::default());

    if !terminal.reported.swap(true, Ordering::SeqCst) {
        let registry = registry();
        registry.bump(
            CapabilityId::Terminal,
            result.rule_id.as_deref().unwrap_or("no-rule"),
            1,
        );
        for (fact, count) in &result.facts {
            registry.bump(CapabilityId::Terminal, fact, *count);
        }
        registry.record(
            CapabilityId::Terminal,
            CapabilityEvent {
                at: now(),
                label: result
                    .rule_id
                    .clone()
                    .unwrap_or_else(|| result.family.clone()),
                detail: format!("{} — {}", terminal.command, result.headline()),
                bytes_in: result.raw_chars as u64,
                bytes_out: result.compact_chars as u64,
                tokens_in: estimate_tokens_chars(result.raw_chars as u64),
                tokens_out: estimate_tokens_chars(result.compact_chars as u64),
            },
        );
    }

    result.text
}

/// Client-side implementation of the ACP `terminal/*` methods.
///
/// Vibe routes shell tool calls through the client when the client advertises
/// `terminal: true`, so this is the code that actually runs `bash`, `git`,
/// `npm` and friends on behalf of the agent.
#[derive(Default)]
pub struct TerminalRegistry {
    terminals: Mutex<HashMap<String, Arc<Terminal>>>,
    next_id: AtomicU64,
}

struct Terminal {
    child: Mutex<Option<tokio::process::Child>>,
    output: Mutex<Vec<u8>>,
    limit: usize,
    truncated: AtomicBool,
    exit: Mutex<Option<ExitInfo>>,
    done: Notify,
    /// The command line, kept so the compaction rules can match on it.
    command: String,
    argv: Vec<String>,
    /// Whether this terminal's output has already been reported to the
    /// capability registry. `terminal/output` may be polled, and a poll is not
    /// a second compaction.
    reported: AtomicBool,
}

#[derive(Clone, Copy)]
struct ExitInfo {
    exit_code: Option<i32>,
    signal: Option<i32>,
}

impl TerminalRegistry {
    pub async fn create(&self, params: &Value) -> AppResult<Value> {
        let command = params
            .get("command")
            .and_then(Value::as_str)
            .ok_or_else(|| AppError::message("terminal/create requires a command"))?;

        let args: Vec<String> = params
            .get("args")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();

        let cwd = params.get("cwd").and_then(Value::as_str);
        let limit = params
            .get("outputByteLimit")
            .and_then(Value::as_u64)
            .map(|v| v as usize)
            .unwrap_or(1_048_576);

        let mut cmd = Command::new(command);
        cmd.args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }

        if let Some(env) = params.get("env").and_then(Value::as_array) {
            for entry in env {
                let name = entry.get("name").and_then(Value::as_str);
                let value = entry.get("value").and_then(Value::as_str);
                if let (Some(name), Some(value)) = (name, value) {
                    cmd.env(name, value);
                }
            }
        }

        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }

        let mut child = cmd.spawn().map_err(|e| {
            AppError::message(format!("could not start `{command}`: {e}"))
        })?;

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let id = format!("term-{}", self.next_id.fetch_add(1, Ordering::SeqCst) + 1);
        let terminal = Arc::new(Terminal {
            child: Mutex::new(Some(child)),
            output: Mutex::new(Vec::new()),
            limit,
            truncated: AtomicBool::new(false),
            exit: Mutex::new(None),
            done: Notify::new(),
            command: command.to_string(),
            argv: args,
            reported: AtomicBool::new(false),
        });

        if let Some(stream) = stdout {
            spawn_pump(terminal.clone(), stream);
        }
        if let Some(stream) = stderr {
            spawn_pump(terminal.clone(), stream);
        }
        spawn_waiter(terminal.clone());

        self.terminals
            .lock()
            .await
            .insert(id.clone(), terminal);

        Ok(json!({ "terminalId": id }))
    }

    pub async fn output(&self, params: &Value) -> AppResult<Value> {
        let terminal = self.get(params).await?;
        let text = {
            let buffer = terminal.output.lock().await;
            String::from_utf8_lossy(&buffer).to_string()
        };
        let truncated = terminal.truncated.load(Ordering::SeqCst);
        let text = compact_output(&terminal, text).await;
        Ok(json!({
            "output": text,
            "truncated": truncated,
        }))
    }

    pub async fn wait_for_exit(&self, params: &Value) -> AppResult<Value> {
        let terminal = self.get(params).await?;
        for _ in 0..6_000 {
            if let Some(info) = *terminal.exit.lock().await {
                return Ok(json!({
                    "exitCode": info.exit_code,
                    "signal": info.signal,
                }));
            }
            let _ = tokio::time::timeout(Duration::from_millis(100), terminal.done.notified()).await;
        }
        Err(AppError::message("terminal did not exit within 10 minutes"))
    }

    pub async fn kill(&self, params: &Value) -> AppResult<Value> {
        let terminal = self.get(params).await?;
        let mut guard = terminal.child.lock().await;
        if let Some(child) = guard.as_mut() {
            let _ = child.start_kill();
        }
        Ok(Value::Null)
    }

    pub async fn release(&self, params: &Value) -> AppResult<Value> {
        let id = params
            .get("terminalId")
            .and_then(Value::as_str)
            .ok_or_else(|| AppError::message("terminal/release requires a terminalId"))?;
        if let Some(terminal) = self.terminals.lock().await.remove(id) {
            let mut guard = terminal.child.lock().await;
            if let Some(child) = guard.as_mut() {
                let _ = child.start_kill();
            }
        }
        Ok(Value::Null)
    }

    pub async fn kill_all(&self) {
        let terminals: Vec<Arc<Terminal>> =
            self.terminals.lock().await.drain().map(|(_, v)| v).collect();
        for terminal in terminals {
            let mut guard = terminal.child.lock().await;
            if let Some(child) = guard.as_mut() {
                let _ = child.start_kill();
            }
        }
    }

    async fn get(&self, params: &Value) -> AppResult<Arc<Terminal>> {
        let id = params
            .get("terminalId")
            .and_then(Value::as_str)
            .ok_or_else(|| AppError::message("missing terminalId"))?;
        self.terminals
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| AppError::message(format!("unknown terminal {id}")))
    }
}

fn spawn_pump<R>(terminal: Arc<Terminal>, mut stream: R)
where
    R: AsyncReadExt + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut chunk = [0u8; 8192];
        loop {
            match stream.read(&mut chunk).await {
                Ok(0) => break,
                Ok(n) => {
                    let mut buffer = terminal.output.lock().await;
                    if buffer.len() + n > terminal.limit {
                        let room = terminal.limit.saturating_sub(buffer.len());
                        buffer.extend_from_slice(&chunk[..room]);
                        terminal.truncated.store(true, Ordering::SeqCst);
                    } else {
                        buffer.extend_from_slice(&chunk[..n]);
                    }
                }
                Err(_) => break,
            }
        }
    });
}

fn spawn_waiter(terminal: Arc<Terminal>) {
    tokio::spawn(async move {
        loop {
            let status = {
                let mut guard = terminal.child.lock().await;
                match guard.as_mut() {
                    Some(child) => child.try_wait(),
                    None => break,
                }
            };
            match status {
                Ok(Some(status)) => {
                    *terminal.exit.lock().await = Some(ExitInfo {
                        exit_code: status.code(),
                        signal: None,
                    });
                    terminal.done.notify_waiters();
                    break;
                }
                Ok(None) => tokio::time::sleep(Duration::from_millis(60)).await,
                Err(_) => break,
            }
        }
    });
}
