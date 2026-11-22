//! The channeler's MCP server.
//!
//! The agent reaches the channeler the same way it reaches every other
//! capability: as an MCP server. That is deliberate. A second protocol would
//! mean a second client in the agent, a second place for auth to be wrong, and a
//! second thing to keep in step with the session lifecycle. This is one server
//! on loopback with a bearer token, registered beside `swarm-mcp` and scoped to
//! the session that opened it.
//!
//! Six tools, and the shape of each is the shape of the bottleneck. A CDP call
//! costs milliseconds; an agent turn costs seconds. So `channel_act` takes a
//! sequence and returns every result plus everything that broke, rather than
//! making the agent spend a turn per click.
//!
//! The registry holds one backend per open channel. A call takes its backend out
//! of the map, uses it, and puts it back, so the lock is never held across a
//! protocol call — a call that pumps the webview's message queue while holding a
//! lock is a deadlock waiting for the right re-entrant path.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::{json, Value};
use tokio::net::TcpListener;

use crate::channel::backend::{Action, Backend};
use crate::channel::issues::Level;
use crate::channel::sequence;
use crate::error::AppError;

static PORT: OnceLock<u16> = OnceLock::new();
static TOKEN: OnceLock<String> = OnceLock::new();
static APP: OnceLock<tauri::AppHandle> = OnceLock::new();
static CHANNELS: OnceLock<Mutex<HashMap<String, Box<dyn Backend>>>> = OnceLock::new();
static NEXT: AtomicU64 = AtomicU64::new(1);

fn registry() -> &'static Mutex<HashMap<String, Box<dyn Backend>>> {
    CHANNELS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Bind the server and start answering. Returns the port it got.
///
/// Any free loopback port rather than a fixed one: a fixed port collides with a
/// second VibeDesk or a stale process, and a channeler that refuses to start
/// because of a port collision is worse than one whose port the config records.
pub async fn ensure_started(app: tauri::AppHandle) -> Result<u16, AppError> {
    let _ = APP.set(app);
    if let Some(port) = PORT.get() {
        return Ok(*port);
    }

    let listener = TcpListener::bind(("127.0.0.1", 0)).await.map_err(|error| {
        AppError::message(format!(
            "the channeler could not bind a loopback port: {error}"
        ))
    })?;
    let port = listener
        .local_addr()
        .map_err(|error| AppError::message(format!("the channeler has no local address: {error}")))?
        .port();

    let _ = TOKEN.set(token());
    let _ = PORT.set(port);

    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                continue;
            };
            tokio::spawn(async move {
                let io = TokioIo::new(stream);
                let service = service_fn(|request: Request<Incoming>| async move {
                    Ok::<_, Infallible>(handle(request).await)
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(io, service)
                    .await;
            });
        }
    });

    Ok(port)
}

/// The ACP entry for this server, once it is listening.
///
/// `None` before `ensure_started` has run, which is what a session opened before
/// the app finished booting should see — no server rather than a dead URL.
///
/// `type` is not decoration. The agent parses `mcpServers` as a tagged union and
/// answers a bare `url` entry with `-32602 Invalid params`, which fails
/// `session/new` and leaves the app unable to open a session at all.
pub fn server_entry() -> Option<Value> {
    let port = *PORT.get()?;
    let token = TOKEN.get()?;
    Some(json!({
        "type": "http",
        "name": "vibedesk-channeler",
        "url": format!("http://127.0.0.1:{port}/mcp"),
        "headers": [{ "name": "Authorization", "value": format!("Bearer {token}") }],
    }))
}

/// What the panel shows: whether the server is listening, and what is open.
pub fn status() -> Value {
    let channels: Vec<Value> = registry()
        .lock()
        .map(|channels| {
            channels
                .iter()
                .map(|(id, backend)| {
                    let capabilities = backend.capabilities();
                    json!({
                        "id": id,
                        "backend": capabilities.backend,
                        "host": capabilities.host,
                        "actions": capabilities.actions,
                        "capture": capabilities.capture,
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    json!({
        "listening": PORT.get().is_some(),
        "port": PORT.get().copied(),
        "url": PORT.get().map(|port| format!("http://127.0.0.1:{port}/mcp")),
        "channels": channels,
    })
}

/// A bearer token for this process.
///
/// Seeded from the standard library's `RandomState`, which is seeded from the OS
/// once per thread. The token stops a different local MCP client from reaching
/// this server by accident; it is not a defence against a local attacker, who
/// could read the same config the agent reads.
fn token() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};

    let mut bytes = Vec::with_capacity(32);
    for round in 0..4u64 {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u64(round);
        hasher.write_u64(std::process::id() as u64);
        bytes.extend_from_slice(&hasher.finish().to_le_bytes());
    }
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

async fn handle(request: Request<Incoming>) -> Response<Full<Bytes>> {
    let authorized = TOKEN
        .get()
        .map(|expected| {
            request
                .headers()
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .map(|value| value == format!("Bearer {expected}"))
                .unwrap_or(false)
        })
        .unwrap_or(false);

    if !authorized {
        return reply(StatusCode::UNAUTHORIZED, json!({ "error": "no token" }));
    }

    let body = match request.into_body().collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(error) => {
            return reply(
                StatusCode::BAD_REQUEST,
                json!({ "error": format!("could not read the request: {error}") }),
            )
        }
    };

    let parsed: Value = match serde_json::from_slice(&body) {
        Ok(parsed) => parsed,
        Err(error) => {
            return reply(
                StatusCode::BAD_REQUEST,
                json!({ "error": format!("the request was not json: {error}") }),
            )
        }
    };

    match dispatch(&parsed) {
        Some(answer) => reply(StatusCode::OK, answer),
        // A notification gets no answer, which is what the protocol says.
        None => Response::builder()
            .status(StatusCode::ACCEPTED)
            .body(Full::new(Bytes::new()))
            .expect("a status and an empty body always build"),
    }
}

fn reply(status: StatusCode, body: Value) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(body.to_string())))
        .expect("a status, a content type and a body always build")
}

pub fn dispatch(request: &Value) -> Option<Value> {
    let id = request.get("id").cloned()?;
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    let params = request.get("params").cloned().unwrap_or(Value::Null);

    let outcome = match method {
        "initialize" => Ok(json!({
            "protocolVersion": "2024-11-05",
            "capabilities": { "tools": {} },
            "serverInfo": {
                "name": "vibedesk-channeler",
                "version": env!("CARGO_PKG_VERSION"),
            },
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools() })),
        "tools/call" => call_tool(&params),
        other => Err(AppError::message(format!(
            "{other} is not a method this server has"
        ))),
    };

    Some(match outcome {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err(error) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32603, "message": error.to_string() },
        }),
    })
}

fn call_tool(params: &Value) -> Result<Value, AppError> {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

    let outcome = match name {
        "channel_open" => open(&arguments),
        "channel_look" => look(&arguments),
        "channel_act" => act(&arguments),
        "channel_expect" => expect(&arguments),
        "channel_issues" => issues(&arguments),
        "channel_close" => close(&arguments),
        other => Err(AppError::message(format!(
            "{other} is not a tool this server has"
        ))),
    };

    Ok(match outcome {
        Ok(text) => json!({ "content": [{ "type": "text", "text": text }] }),
        Err(error) => json!({
            "content": [{ "type": "text", "text": error.to_string() }],
            "isError": true,
        }),
    })
}

fn open(arguments: &Value) -> Result<String, AppError> {
    let backend = arguments
        .get("backend")
        .and_then(Value::as_str)
        .unwrap_or("webview");
    let capture = arguments
        .get("capture")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let mut channel: Box<dyn Backend> = match backend {
        "webview" => {
            #[cfg(windows)]
            {
                let label = arguments
                    .get("label")
                    .and_then(Value::as_str)
                    .unwrap_or("main");
                let app = APP
                    .get()
                    .ok_or_else(|| AppError::message("the app is not running"))?
                    .clone();
                Box::new(crate::channel::webview::WebviewBackend::open(app, label)?)
            }
            #[cfg(not(windows))]
            {
                return Err(AppError::message("the webview backend needs Windows"));
            }
        }
        "window" => {
            #[cfg(windows)]
            {
                let spec = crate::channel::window::WindowSpec {
                    title: arguments
                        .get("title")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    class: arguments
                        .get("class")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    command: arguments
                        .get("command")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    args: arguments
                        .get("args")
                        .and_then(Value::as_array)
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(|item| item.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default(),
                    host: match arguments.get("host").and_then(Value::as_str) {
                        Some("hidden-desktop") => {
                            Some(crate::channel::host::HostKind::HiddenDesktop)
                        }
                        Some("desktop") | None => Some(crate::channel::host::HostKind::Desktop),
                        Some(other) => {
                            return Err(AppError::message(format!(
                                "{other} is not a host — use desktop or hidden-desktop"
                            )))
                        }
                    },
                };
                Box::new(crate::channel::window::WindowBackend::open(spec)?)
            }
            #[cfg(not(windows))]
            {
                return Err(AppError::message("the window backend needs Windows"));
            }
        }
        "process" => {
            let command = arguments
                .get("command")
                .and_then(Value::as_str)
                .ok_or_else(|| AppError::message("a process channel needs a command"))?;
            let args: Vec<String> = arguments
                .get("args")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let cwd = arguments.get("cwd").and_then(Value::as_str);
            Box::new(crate::channel::process::ProcessBackend::open(
                command, &args, cwd,
            )?)
        }
        "engine" => {
            let command = arguments
                .get("command")
                .and_then(Value::as_str)
                .ok_or_else(|| AppError::message("an engine channel needs a command"))?;
            let args: Vec<String> = strings(arguments, "args");
            let cwd = arguments.get("cwd").and_then(Value::as_str);
            let format = match arguments.get("format").and_then(Value::as_str) {
                Some(name) => crate::channel::engine::Format::parse(name).ok_or_else(|| {
                    AppError::message(format!(
                        "{name} is not a format — use exit, tap, cargo or pytest"
                    ))
                })?,
                None => crate::channel::engine::Format::Exit,
            };
            let filter_args = strings(arguments, "filter_args");
            let timeout_ms = arguments.get("timeout_ms").and_then(Value::as_u64);
            Box::new(crate::channel::engine::EngineBackend::open(
                command,
                &args,
                cwd,
                format,
                &filter_args,
                timeout_ms,
            )?)
        }
        other => {
            return Err(AppError::message(format!(
                "{other} is not a backend this build has"
            )))
        }
    };

    let capabilities = channel.capabilities();
    let first = channel.look(capture)?;
    let id = format!("ch{}", NEXT.fetch_add(1, Ordering::Relaxed));

    registry()
        .lock()
        .map_err(|_| AppError::message("the channel registry is poisoned"))?
        .insert(id.clone(), channel);

    Ok(serde_json::to_string_pretty(&json!({
        "id": id,
        "capabilities": capabilities,
        "look": first,
    }))?)
}

fn look(arguments: &Value) -> Result<String, AppError> {
    let id = id_of(arguments)?;
    let capture = arguments
        .get("capture")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    with_channel(&id, |channel| {
        Ok(serde_json::to_string_pretty(&channel.look(capture)?)?)
    })
}

fn act(arguments: &Value) -> Result<String, AppError> {
    let id = id_of(arguments)?;
    let raw = arguments
        .get("actions")
        .cloned()
        .ok_or_else(|| AppError::message("act needs actions"))?;
    let actions: Vec<Action> = serde_json::from_value(raw)
        .map_err(|error| AppError::message(format!("the actions did not parse: {error}")))?;
    let allow = arguments
        .get("allow_destructive")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    with_channel(&id, |channel| {
        let scope = channel.stage();
        let outcome = sequence::run(channel, &actions, &scope, allow);
        Ok(serde_json::to_string_pretty(&json!({
            "ok": outcome.ok(),
            "results": outcome.results,
            "stopped_at": outcome.stopped_at,
            "blocked": outcome.blocked,
            "issues": outcome.issues,
            "more_issues": outcome.more_issues,
        }))?)
    })
}

fn expect(arguments: &Value) -> Result<String, AppError> {
    let id = id_of(arguments)?;
    let allow = arguments
        .get("allow_issues")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    with_channel(&id, |channel| {
        let issues = channel.issues()?;
        let failed = !allow && issues.iter().any(|issue| issue.level >= Level::Error);
        Ok(serde_json::to_string_pretty(&json!({
            "ok": !failed,
            "issues": issues,
        }))?)
    })
}

fn issues(arguments: &Value) -> Result<String, AppError> {
    let id = id_of(arguments)?;
    with_channel(&id, |channel| {
        Ok(serde_json::to_string_pretty(&json!({
            "issues": channel.issues()?,
        }))?)
    })
}

fn close(arguments: &Value) -> Result<String, AppError> {
    let id = id_of(arguments)?;
    let mut channel = registry()
        .lock()
        .map_err(|_| AppError::message("the channel registry is poisoned"))?
        .remove(&id)
        .ok_or_else(|| AppError::message(format!("there is no channel called {id}")))?;
    channel.close()?;
    Ok(json!({ "closed": id }).to_string())
}

fn id_of(arguments: &Value) -> Result<String, AppError> {
    arguments
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| AppError::message("this tool needs a channel id"))
}

/// A string array from the arguments, ignoring anything that is not a string.
fn strings(arguments: &Value, key: &str) -> Vec<String> {
    arguments
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Take a channel out of the registry, use it, and put it back.
///
/// The lock is released before the work runs. A protocol call pumps the
/// webview's message queue, and a queue that re-enters this module while the
/// registry is locked would deadlock.
fn with_channel<T>(
    id: &str,
    work: impl FnOnce(&mut dyn Backend) -> Result<T, AppError>,
) -> Result<T, AppError> {
    let mut channel = registry()
        .lock()
        .map_err(|_| AppError::message("the channel registry is poisoned"))?
        .remove(id)
        .ok_or_else(|| {
            AppError::message(format!("there is no channel called {id} — call channel_open first"))
        })?;

    let outcome = work(channel.as_mut());

    if let Ok(mut channels) = registry().lock() {
        channels.insert(id.to_string(), channel);
    }
    outcome
}

fn tools() -> Value {
    json!([
        {
            "name": "channel_open",
            "description": "Open a channel onto something you built, so you can drive it and see what breaks. Returns an id, what the backend can do, and a first look. Backends: `webview` (a page in this app, driven over the DevTools protocol — input lands at the renderer, so the cursor does not move and the window need not be in front), `window` (any native window, by title or class, or by launching the program that makes one — read through UI Automation, so it needs no SDK and no cooperation from the target), `process` (a child process over its pipes), `engine` (a project that provides its own test hook — you declare the command that runs the suite and the format its output is in, and the channeler reports one outcome per test; it is headless, so it never touches the user's screen, and it works for any project with a test command rather than for one engine). The reply's `capabilities` says what you actually got: `foreground` is whether acting takes the user's screen, and `input` is `hardware` (real OS input, reaches everything including apps reading raw input) or `synthetic` (events delivered to the target's own event system, reaches any desktop but only a target that listens).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "backend": { "type": "string", "enum": ["webview", "window", "process", "engine"], "default": "webview" },
                    "label": { "type": "string", "description": "For `webview`: which webview. Defaults to `main`." },
                    "title": { "type": "string", "description": "For `window`: a substring of the window title." },
                    "class": { "type": "string", "description": "For `window`: an exact window class name." },
                    "command": { "type": "string", "description": "For `window`, `process` and `engine`: the executable to start." },
                    "args": { "type": "array", "items": { "type": "string" }, "description": "For `window`, `process` and `engine`: its arguments." },
                    "cwd": { "type": "string", "description": "For `process` and `engine`: where to start it." },
                    "format": { "type": "string", "enum": ["exit", "tap", "cargo", "pytest"], "default": "exit", "description": "For `engine`: how to read the suite's output. `exit` reads only the exit code; `tap` reads `ok 1 - name` / `not ok 2 - name`; `cargo` reads `test name ... ok`; `pytest` reads `path::name PASSED` and the `FAILED path::name - message` summary. A format that does not match reports no tests rather than a pass." },
                    "filter_args": { "type": "array", "items": { "type": "string" }, "description": "For `engine`: how to pass a test-name filter to the runner, with `{filter}` standing in for the name — `[\"{filter}\"]` for cargo, `[\"-k\", \"{filter}\"]` for pytest, `[\"--test-name-pattern\", \"{filter}\"]` for node. Without it, a filtered run is refused rather than silently running everything." },
                    "timeout_ms": { "type": "integer", "description": "For `engine`: how long a run may take before it is killed. Defaults to 300000, capped at 1800000." },
                    "host": { "type": "string", "enum": ["desktop", "hidden-desktop"], "default": "desktop", "description": "For `window`: `hidden-desktop` runs the target on a desktop of its own, so the user's screen, cursor and focus are untouched and the test can run while they work. Input there is posted window messages, so an app that reads raw input cannot be driven that way. `desktop` uses real hardware input but needs the window in front." },
                    "capture": { "type": "boolean", "default": false, "description": "Include a JPEG of the stage in the first look. The `window` backend has no capture and says so." }
                }
            }
        },
        {
            "name": "channel_look",
            "description": "What is on screen right now: a text digest, the refs you can act on, and the stage rectangle. Refs are held in the page, so a ref survives a layout shift between the look and the act.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "capture": { "type": "boolean", "default": false, "description": "Include a JPEG of the stage, base64." }
                },
                "required": ["id"]
            }
        },
        {
            "name": "channel_act",
            "description": "Run a sequence of actions and get every result plus everything that broke. Stops at the first failure, because the steps after a failed step were written assuming it worked. Action kinds: click, hold, drag, type, scroll, tick, wait, read, run. `run` is the `engine` backend's verb: it re-runs the project's suite and reports one outcome per test. A sequence that deletes and then confirms is refused unless you pass allow_destructive.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "actions": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "kind": { "type": "string", "enum": ["click", "hold", "drag", "type", "scroll", "tick", "wait", "read", "run"] },
                                "target": { "description": "A ref id from the last look, or {x, y} in CSS pixels." },
                                "from": { "description": "For drag: where it starts." },
                                "to": { "description": "For drag: where it ends." },
                                "button": { "type": "string", "enum": ["left", "right", "middle"], "default": "left" },
                                "count": { "type": "integer", "default": 1 },
                                "key": { "type": "string", "description": "For hold: ArrowRight, Space, KeyA, and so on." },
                                "frames": { "type": "integer", "description": "For hold and tick: how many animation frames." },
                                "ms": { "type": "integer", "description": "For hold and wait: milliseconds." },
                                "text": { "type": "string", "description": "For type." },
                                "dx": { "type": "number", "description": "For scroll." },
                                "dy": { "type": "number", "description": "For scroll." },
                                "filter": { "type": "string", "description": "For run: narrow the run to one test by name, passed through the channel's filter_args. Refused if the channel was opened without filter_args." }
                            },
                            "required": ["kind"]
                        }
                    },
                    "allow_destructive": { "type": "boolean", "default": false, "description": "Run a sequence that deletes and then confirms." }
                },
                "required": ["id", "actions"]
            }
        },
        {
            "name": "channel_expect",
            "description": "Assert that nothing broke since the last call. Fails on any new error-level issue unless allow_issues is set. Use this as the last step of a test rather than reading the issues yourself.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "allow_issues": { "type": "boolean", "default": false }
                },
                "required": ["id"]
            }
        },
        {
            "name": "channel_issues",
            "description": "Everything that broke since the last call: console errors, uncaught exceptions, failed requests, dialogs, non-zero exits. Deduplicated, with a count per entry.",
            "inputSchema": {
                "type": "object",
                "properties": { "id": { "type": "string" } },
                "required": ["id"]
            }
        },
        {
            "name": "channel_close",
            "description": "Close a channel and stop listening. A process channel kills its child.",
            "inputSchema": {
                "type": "object",
                "properties": { "id": { "type": "string" } },
                "required": ["id"]
            }
        }
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The agent parses `mcpServers` as a tagged union. An HTTP entry without
    /// `type` is answered with `-32602 Invalid params`, which fails
    /// `session/new` and leaves the app unable to open a session at all — so the
    /// discriminator is load-bearing, not decoration.
    #[test]
    fn the_channeler_entry_is_tagged_as_http() {
        let _ = PORT.set(41234);
        let _ = TOKEN.set("probe-token".into());
        let port = *PORT.get().expect("port was just set");
        let token = TOKEN.get().expect("token was just set");

        let entry = server_entry().expect("a listening server has an entry");

        assert_eq!(entry["type"], "http");
        assert_eq!(entry["url"], format!("http://127.0.0.1:{port}/mcp"));
        assert_eq!(entry["headers"][0]["name"], "Authorization");
        assert_eq!(entry["headers"][0]["value"], format!("Bearer {token}"));
        assert!(
            entry.get("command").is_none(),
            "an http entry carries a url, not a command"
        );
    }
}
