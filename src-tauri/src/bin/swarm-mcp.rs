//! The swarm, as a tool the agent can call.
//!
//! This is an MCP server over stdio. `vibe-acp` spawns it for a session, and the
//! agent the user is talking to gets four tools: start a swarm, read it, approve
//! its plan, stop it. That is the difference between a feature that lives in a
//! panel and a feature the user can reach by saying what they want.
//!
//! It is a *client*, not a driver. The app owns the engine and the driver lock,
//! so `start`, `approve` and `stop` are written to the command queue and answered
//! by the app. `status` needs no owner: a run is a file, and reading it is not an
//! action. The split matters — a run started here belongs to the app, so it
//! survives the chat that asked for it and can still be approved or stopped from
//! the panel.
//!
//! Nothing but JSON-RPC goes to stdout. Diagnostics go to stderr, because a stray
//! `println!` is a protocol error the agent sees as a broken tool.

use std::io::{BufRead, Write};
use std::time::Duration;

use serde_json::{json, Value};

use vibedesk_lib::swarm::board::Board;
use vibedesk_lib::swarm::commands::{self, Command};
use vibedesk_lib::swarm::{list_runs, run_dir, Run, RunState};

/// How long a command may wait for the app before the tool reports that the app
/// is not running. Long enough for a cold start, short enough that a tool call
/// does not look hung.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(25);

/// How many board entries a status call returns.
///
/// The board is the run's memory and it grows without bound; an agent that reads
/// all of it spends its context on history it cannot act on. The tail is what a
/// decision needs.
const BOARD_TAIL: usize = 14;

fn main() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut line = String::new();

    loop {
        line.clear();
        match stdin.lock().read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(error) => {
                eprintln!("swarm-mcp: could not read stdin: {error}");
                break;
            }
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(request) = serde_json::from_str::<Value>(trimmed) else {
            eprintln!("swarm-mcp: ignoring a line that is not JSON");
            continue;
        };

        // A notification has no id and takes no answer. `notifications/initialized`
        // is the only one that matters, and it needs nothing.
        let Some(id) = request.get("id").cloned() else {
            continue;
        };
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let params = request.get("params").cloned().unwrap_or(Value::Null);

        let response = match method.as_str() {
            "initialize" => json!({
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "vibedesk-swarm", "version": env!("CARGO_PKG_VERSION") },
            }),
            "ping" => json!({}),
            "tools/list" => json!({ "tools": tools() }),
            "tools/call" => call(&params),
            other => {
                let _ = writeln!(
                    stdout,
                    "{}",
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": { "code": -32601, "message": format!("no method `{other}`") },
                    })
                );
                let _ = stdout.flush();
                continue;
            }
        };

        let _ = writeln!(
            stdout,
            "{}",
            json!({ "jsonrpc": "2.0", "id": id, "result": response })
        );
        let _ = stdout.flush();
    }
}

/// The tool list.
///
/// The descriptions are written for a model deciding *whether* to call, not for a
/// human reading a manual: each one says what the tool is for and when it is the
/// wrong tool.
fn tools() -> Value {
    json!([
        {
            "name": "swarm_start",
            "description": "Start a swarm on a goal: a lead agent plans the work, workers implement each task in its own git worktree, and independent verifiers try to break what they produced. Use this when the work is large enough to split into tasks that can be built in parallel — a feature spanning several files, a game, a migration. Do not use it for a one-file change or a question; a swarm costs many times a chat. The run parks at `awaiting-approval` so the plan can be reviewed before any worker is spawned.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "goal": {
                        "type": "string",
                        "description": "One sentence: what the run produces. The lead plans from this and nothing else."
                    },
                    "workspace": {
                        "type": "string",
                        "description": "Absolute path to the folder the run works in. Omit to use the session's own directory. The run reads and writes this folder only, and the answer echoes the path it resolved, so a mismatch is visible before the lead plans."
                    },
                    "concurrency": {
                        "type": "integer",
                        "description": "How many tasks may hold a worker slot at once. Defaults to 4."
                    },
                    "model": {
                        "type": "string",
                        "description": "The model the lead and workers run on. Omit to use the workspace's active model. The verifier is always picked for diversity, so this cannot collapse the run onto one model."
                    },
                    "budgetTokens": {
                        "type": "integer",
                        "description": "The run's token ceiling. Defaults to 25,000,000. One worker turn on a real repository reports about 1.3M input tokens because the agent re-sends its context on every step, so a ceiling below a few million stops a healthy run during its first task. The count includes cached tokens; the cache share is reported beside it."
                    }
                },
                "required": ["goal"]
            }
        },
        {
            "name": "swarm_status",
            "description": "Read the current swarm run: its state, every task with its owner and verdict, the roster of agents, and the tail of the board where they talk to each other. Call this after `swarm_start` to see the plan, and while a run is going to report progress. Returns `{run: null}` when no run exists.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "swarm_approve",
            "description": "Approve the plan a swarm is waiting on and start its workers. Only valid when `swarm_status` reports `awaiting-approval`. Read the plan to the user first — approval spends the run's whole budget.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "swarm_stop",
            "description": "Stop the live swarm run. Worktrees are kept and named so the work is not lost. Use this when the user asks to stop, or when the run has failed and is holding slots.",
            "inputSchema": { "type": "object", "properties": {} }
        }
    ])
}

/// Dispatch one `tools/call`.
fn call(params: &Value) -> Value {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

    let outcome = match name {
        "swarm_start" => start(&arguments),
        "swarm_status" => Ok(status()),
        "swarm_approve" => ask(&Command::approve()),
        "swarm_stop" => ask(&Command::stop()),
        other => Err(format!("no tool `{other}`")),
    };

    match outcome {
        Ok(value) => json!({
            "content": [{ "type": "text", "text": serde_json::to_string_pretty(&value).unwrap_or_default() }],
            "isError": false,
        }),
        Err(reason) => json!({
            "content": [{ "type": "text", "text": reason }],
            "isError": true,
        }),
    }
}

fn start(arguments: &Value) -> Result<Value, String> {
    let goal = arguments
        .get("goal")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if goal.is_empty() {
        return Err("`goal` is required: one sentence describing what the run produces".into());
    }

    // The folder, named or inherited.
    //
    // Inheriting the session's directory is the right default — the run edits a
    // repository the user is looking at — but it was the *only* option, and it
    // is not the user's choice: the session directory comes from the app's
    // remembered folder, so a stale one bound every run to a directory nobody
    // named, with no way to say otherwise from here. A named folder is used
    // exactly as given, after checking it exists, and the answer echoes it.
    let named = arguments
        .get("workspace")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());

    let workspace = match named {
        Some(path) => {
            let dir = std::path::Path::new(path);
            if !dir.is_absolute() {
                return Err(format!("`workspace` must be an absolute path, got {path:?}"));
            }
            if !dir.is_dir() {
                return Err(format!("`workspace` is not a directory: {path}"));
            }
            path.to_string()
        }
        None => std::env::current_dir()
            .map(|dir| dir.display().to_string())
            .map_err(|error| format!("no `workspace` was named and the session has no directory: {error}"))?,
    };

    let mut command = Command::start(goal, workspace);
    command.concurrency = arguments
        .get("concurrency")
        .and_then(Value::as_u64)
        .map(|n| n as usize);
    command.model = arguments
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    command.budget_tokens = arguments.get("budgetTokens").and_then(Value::as_u64);
    command.budget_micros = arguments.get("budgetMicros").and_then(Value::as_u64);

    ask(&command)
}

/// Send a command to the app and unwrap its answer.
fn ask(command: &Command) -> Result<Value, String> {
    let answer = commands::submit(command, ANSWER_TIMEOUT)?;
    if answer.get("ok").and_then(Value::as_bool) == Some(true) {
        Ok(answer.get("result").cloned().unwrap_or(Value::Null))
    } else {
        Err(answer
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("the app refused the command without saying why")
            .to_string())
    }
}

/// The live run, read from disk.
///
/// No engine, no lock, no writes: this is the same file the panel reads. A run
/// that is live is the newest one that has not finished; a finished run is
/// reported only when it is the newest thing on disk, so the agent can talk about
/// what just happened.
fn status() -> Value {
    let runs = list_runs();
    let Some(run) = runs
        .iter()
        .find(|run| run.state.is_live())
        .or_else(|| runs.first())
    else {
        return json!({ "run": null });
    };
    describe(run)
}

fn describe(run: &Run) -> Value {
    let board = Board::load_at(&run_dir(&run.id));

    let tasks: Vec<Value> = run
        .tasks
        .iter()
        .map(|task| {
            json!({
                "id": task.id,
                "title": task.title,
                "state": task.state,
                "owner": task.owner,
                "attempts": task.attempts,
                "deps": task.deps,
                // What the task has actually produced, as opposed to what it
                // reports. `stalled` is set when the branch tip is still the commit
                // it started from long after it started, which is the state the
                // audited run could not distinguish from `working`.
                "filesChanged": task.files_changed,
                "commits": task.commits,
                "lastCommitAt": task.last_commit_at,
                "stalled": task.stalled,
                "verdict": task.verdict.as_ref().map(|verdict| json!({
                    "passed": verdict.passed,
                    "reason": verdict.reason,
                })),
            })
        })
        .collect();

    let members: Vec<Value> = run
        .roster()
        .map(|member| {
            json!({
                "id": member.id,
                "role": member.role,
                "state": member.state,
                "model": member.model,
                "task": member.task,
                "error": member.error,
                // Which clock fired, per cause. `restarts` alone could not answer
                // the audit's question: a dead process and a wedged turn both
                // showed up as a restart.
                "restarts": member.respawns,
                "restartsInfra": member.restarts_infra,
                "restartsTimeout": member.restarts_timeout,
                "restartsProvider": member.restarts_provider,
            })
        })
        .collect();

    let entries: Vec<Value> = board
        .tail(BOARD_TAIL)
        .iter()
        .map(|entry| {
            json!({
                "at": entry.at,
                "from": entry.from,
                "kind": entry.kind,
                "task": entry.task,
                "text": entry.text,
            })
        })
        .collect();

    let mut out = json!({
        "runId": run.id,
        "state": run.state,
        "goal": run.goal,
        "workspace": run.workspace,
        "tasks": tasks,
        "members": members,
        "board": entries,
        "budget": {
            "usedTokens": run.budget.used_tokens,
            "maxTokens": run.budget.max_tokens,
            "cachedTokens": run.budget.cached_tokens,
            "spentTokens": run.budget.spent_tokens,
            "spendMicros": run.budget.spend_micros,
            "maxSpendMicros": run.budget.max_spend_micros,
            "spendPct": run.budget.spend_pct,
            "tokenPct": run.budget.token_pct,
            "hitRate": run.budget.hit_rate,
            "unpricedModels": run.budget.unpriced_models,
            "ratesVersion": run.budget.rates_version,
        },
        "integration": {
            "state": run.integration.state,
            "branch": run.integration.branch,
            "merged": run.integration.merged,
            "suite": run.integration.suite,
            "suitePassed": run.integration.suite_passed,
            "conflict": run.integration.conflict.as_ref().map(|conflict| json!({
                "task": conflict.task,
                "files": conflict.files,
                "detail": conflict.detail,
            })),
        },
    });

    // The plan is only worth sending while it is still a decision. Once the run
    // is going, the tasks above already carry the state.
    if run.state == RunState::AwaitingApproval {
        let plan: Vec<Value> = run
            .tasks
            .iter()
            .map(|task| {
                json!({
                    "id": task.id,
                    "title": task.title,
                    "detail": task.detail,
                    "acceptance": task.acceptance,
                    "files": task.files,
                    "deps": task.deps,
                })
            })
            .collect();
        out["plan"] = json!({
            "tasks": plan,
            "integrationSuite": run.integration.suite,
        });
    }

    if let Some(reflection) = &run.reflection {
        out["reflection"] = json!({
            "summary": reflection.summary,
            "gotchas": reflection.gotchas.iter().map(|gotcha| json!({
                "file": gotcha.file,
                "text": gotcha.text,
                "source": gotcha.source,
            })).collect::<Vec<_>>(),
        });
    }

    if let Some(failure) = &run.failure {
        out["failure"] = json!(failure);
    }

    out
}
