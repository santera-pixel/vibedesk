use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};
use tauri::AppHandle;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{oneshot, watch, Mutex};

use crate::acp::protocol::{
    client_capabilities, client_info, error_response, success_response, Incoming,
    OutgoingNotification, OutgoingRequest, RpcError,
};
use crate::acp::terminal::TerminalRegistry;
use crate::error::{AppError, AppResult};
use crate::paths;

pub const EVENT_NOTIFICATION: &str = "acp://notification";
pub const EVENT_REQUEST: &str = "acp://request";
pub const EVENT_STATUS: &str = "agent://status";
pub const EVENT_LOG: &str = "agent://log";
/// Emitted when the agent reports what a turn cost.
///
/// Two readings arrive per turn and they mean different things. The live
/// `usage_update` notification is a *session-cumulative* snapshot that repeats
/// every earlier turn, so it is a context gauge and not a cost. The
/// `session/prompt` result carries the same cumulative counters at the moment
/// the turn closed, plus the wall clock, and that is the reading a ledger folds
/// in — as a delta against the previous one, never as a sum. Both are emitted
/// here with `final` telling them apart, so the consumer never has to guess
/// which one it is holding.
pub const EVENT_USAGE: &str = "acp://usage";
/// Emitted when a turn the webview had stopped waiting for finally closes.
///
/// A `ready` status cannot carry this: the status for the turn that just ended
/// can arrive after the next one has already started, so the webview would tear
/// down a live turn.
pub const EVENT_TURN_ENDED: &str = "agent://turn-ended";

const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const INIT_TIMEOUT: Duration = Duration::from_secs(90);
const INTERACTION_TIMEOUT: Duration = Duration::from_secs(900);

/// How long the agent may be completely silent before a turn is considered
/// wedged.
///
/// A turn has no wall-clock deadline. The agent streams `session/update` while
/// it works, so a long turn is not a slow turn — it is a turn that is still
/// talking. Only silence is evidence of a problem, and 180s of it is well past
/// any gap a healthy agent produces between updates.
const TURN_IDLE_TIMEOUT: Duration = Duration::from_secs(180);

/// How long a stalled turn waits for its late reply before the process is killed.
///
/// The reaper exists so a reply that arrives after the idle window still ends the
/// turn instead of vanishing. It must not wait forever: a stalled turn whose
/// agent never answers keeps `busy` set, and the engine reads `busy` as "a turn
/// is in flight" and waits its 1800s turn ceiling instead of its 601s idle limit.
/// The sixth live run hung exactly that way — a worker process alive at 0.015s of
/// CPU, `busy` true, and the run showing `running` for half an hour while it
/// waited for a reply that was never coming.
///
/// Killing is the honest end of a stalled turn. The worktree and its commits
/// survive the process, and the engine respawns the member with the retry brief.
const STALL_GRACE: Duration = Duration::from_secs(120);

/// How long a turn may sit in "waiting on the user" before it is a hang.
///
/// `TurnSignals::waiting` is true while the agent has an unanswered request of
/// us, a pending permission, or an open tool call. Each of those is a real
/// reason to suspend the idle watchdog — a shell command that runs for four
/// minutes emits nothing at all, and calling that agent wedged is wrong.
///
/// But a signal that never clears disables the watchdog completely, and then
/// nothing bounds the turn. The thirteenth live run's `v-t1` sat at 0.015s of
/// CPU for twelve minutes with `busy` set: the engine could not nudge it
/// (`member_is_silent` reads `busy`), could not restart it (`respawn_dead` reads
/// `busy`), and would have waited its 1800s ceiling for a reply that was never
/// coming. A turn that has been "waiting" for longer than any real answer takes
/// is not waiting, it is hung.
///
/// Generous on purpose: the longest legitimate wait is a `terminal/wait_for_exit`
/// on a full test suite, and this repository's suite runs in seconds.
const WAITING_CEILING: Duration = Duration::from_secs(900);

/// The idle window in force.
///
/// `VIBEDESK_TURN_IDLE_SECS` overrides it: a model that thinks for minutes
/// between updates needs a longer window, and the watchdog itself is testable
/// without waiting three real minutes.
fn turn_idle_timeout() -> Duration {
    std::env::var("VIBEDESK_TURN_IDLE_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .map(Duration::from_secs)
        .unwrap_or(TURN_IDLE_TIMEOUT)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentStatus {
    pub state: String,
    pub pid: Option<u32>,
    pub version: Option<String>,
    pub cwd: Option<String>,
    pub session_id: Option<String>,
    pub model: Option<String>,
    pub agent: Option<String>,
    pub binary: Option<String>,
    pub error: Option<String>,
    pub started_at: Option<String>,
    pub restarts: u32,
}

impl Default for AgentStatus {
    fn default() -> Self {
        Self {
            state: "stopped".into(),
            pid: None,
            version: None,
            cwd: None,
            session_id: None,
            model: None,
            agent: None,
            binary: None,
            error: None,
            started_at: None,
            restarts: 0,
        }
    }
}

/// What the webview sends back for a pending agent request.
pub enum InteractionReply {
    Permission(Option<String>),
    Elicitation {
        action: String,
        content: Option<Value>,
    },
}

/// How a turn's reply channel resolved.
enum TurnWait {
    Settled(Result<Value, RpcError>),
    Closed,
    /// The agent went silent. The receiver is still live, so whoever holds it
    /// can clear the turn when the real answer eventually lands.
    Stalled(oneshot::Receiver<Result<Value, RpcError>>),
}

fn settle(
    outcome: Result<Result<Value, RpcError>, oneshot::error::RecvError>,
) -> TurnWait {
    match outcome {
        Ok(Ok(value)) => TurnWait::Settled(Ok(value)),
        Ok(Err(rpc)) => TurnWait::Settled(Err(rpc)),
        Err(_) => TurnWait::Closed,
    }
}

/// Wait for a turn's reply, restarting the idle clock on every sign of life.
///
/// Split out from `Inner` so the watchdog can be driven by a fake clock in
/// tests: `tokio::time::Instant` follows `tokio::time::pause`, `std::time`
/// does not.
/// Everything that can make agent silence legitimate.
///
/// The watchdog only fires when the agent is quiet *and* nothing explains why.
/// Each of these is a reason: an approval waiting on the user, a request the
/// agent has open against us, or a tool call it announced and has not finished.
struct TurnSignals<'a> {
    interactions: &'a Mutex<HashMap<String, oneshot::Sender<InteractionReply>>>,
    inbound: &'a AtomicU64,
    open_tools: &'a Mutex<HashSet<String>>,
}

impl TurnSignals<'_> {
    async fn waiting(&self) -> bool {
        self.inbound.load(Ordering::SeqCst) > 0
            || !self.interactions.lock().await.is_empty()
            || !self.open_tools.lock().await.is_empty()
    }
}

/// Track which tool calls the agent has announced and not yet finished.
///
/// A shell command that runs for four minutes emits nothing at all, so without
/// this the idle watchdog would call a working agent wedged.
async fn track_tool_activity(
    open: &Mutex<HashSet<String>>,
    method: &str,
    params: &Value,
) {
    if method != "session/update" {
        return;
    }
    let Some(update) = params.get("update") else {
        return;
    };
    let Some(kind) = update.get("sessionUpdate").and_then(Value::as_str) else {
        return;
    };
    if kind != "tool_call" && kind != "tool_call_update" {
        return;
    }
    let Some(id) = update.get("toolCallId").and_then(Value::as_str) else {
        return;
    };
    let status = update
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("in_progress");

    // The initial `tool_call` carries the title and the arguments; the updates
    // that follow carry only status. Recording here means one event per call.
    if kind == "tool_call" {
        record_mcp_tool_call(update);
    }

    let mut open = open.lock().await;
    if status == "completed" || status == "failed" {
        open.remove(id);
    } else {
        open.insert(id.to_string());
    }
}

/// Read the live usage snapshot out of a `usage_update` notification.
///
/// The counters live under `_meta`; `used` and `size` are the context window and
/// sit at the top level. A notification without `_meta` is the agent saying
/// nothing has been spent yet, which is a real reading and not an error.
fn usage_from_update(update: &Value) -> Option<Value> {
    if update.get("sessionUpdate").and_then(Value::as_str) != Some("usage_update") {
        return None;
    }
    let meta = update.get("_meta").cloned().unwrap_or(Value::Null);
    let count = |key: &str| meta.get(key).and_then(Value::as_u64).unwrap_or(0);
    Some(json!({
        "final": false,
        // The only reading that carries a context gauge. `used` and `size` are
        // the window; the end-of-turn reading has neither, and marking it here
        // is what stops that one from overwriting this one.
        "gauge": true,
        "input": count("promptTokens"),
        "output": count("completionTokens"),
        "cached": count("cachedTokens"),
        "contextUsed": update.get("used").and_then(Value::as_u64).unwrap_or(0),
        "contextSize": update.get("size").and_then(Value::as_u64).unwrap_or(0),
        "secs": 0.0,
    }))
}

/// Read the end-of-turn usage out of a `session/prompt` result.
///
/// `secs` is measured here rather than taken from the agent's
/// `lastTurnDuration`, which the live agent reports as `0.0` on every turn. A
/// clock the client owns is the only one it can trust.
///
/// No context gauge. `totalTokens` is the session's cumulative spend, not how
/// full the window is, and this reading used to publish it as `contextUsed`
/// beside a hardcoded `contextSize: 0` — so the panel showed a run at 3.6M of a
/// zero-token window while the session's own breakdown said 417K of 1M. The
/// gauge comes from `usage_update` and nowhere else.
fn usage_from_turn(result: &Value, secs: f64) -> Option<Value> {
    let usage = result.get("usage")?;
    let count = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
    Some(json!({
        "final": true,
        "input": count("inputTokens"),
        "output": count("outputTokens"),
        "cached": count("cachedReadTokens"),
        "secs": secs,
    }))
}

/// Record an MCP tool call in the capability ledger.
/// The panels answer "what has this server actually done", and the only place
/// that is observable is the transcript: the agent's tool calls arrive here as
/// `session/update` notifications. The servers' own tools are stateless — there
/// is no `history` call to make — so without this the web panel would have
/// nothing to show but a registration flag.
fn record_mcp_tool_call(update: &Value) {
    let Some(title) = update.get("title").and_then(Value::as_str) else {
        return;
    };
    let Some((capability, tool)) = mcp_tool(title) else {
        return;
    };

    crate::capabilities::registry().record(
        capability,
        crate::capabilities::CapabilityEvent {
            at: crate::capabilities::now(),
            label: tool.to_string(),
            detail: tool_detail(update.get("rawInput")),
            bytes_in: 0,
            bytes_out: 0,
            tokens_in: 0,
            tokens_out: 0,
        },
    );
}

/// Split `mcp_<server>.<tool>` into the capability it serves and the tool name.
fn mcp_tool(title: &str) -> Option<(crate::capabilities::CapabilityId, &str)> {
    use crate::capabilities::CapabilityId;

    let (server, tool) = title.strip_prefix("mcp_")?.split_once('.')?;
    let capability = match server {
        "codebase_memory" | "codebase-memory" => CapabilityId::CodeGraph,
        "donsetch" => CapabilityId::Web,
        _ => return None,
    };
    Some((capability, tool))
}

/// The one argument worth showing: what was searched, fetched or indexed.
fn tool_detail(input: Option<&Value>) -> String {
    let Some(input) = input.and_then(Value::as_object) else {
        return String::new();
    };
    for key in ["query", "url", "project", "path", "pattern", "command"] {
        if let Some(value) = input.get(key).and_then(Value::as_str) {
            if !value.trim().is_empty() {
                return format!("{key}: {value}");
            }
        }
    }
    String::new()
}

async fn await_turn(
    mut rx: oneshot::Receiver<Result<Value, RpcError>>,
    mut activity: watch::Receiver<tokio::time::Instant>,
    signals: &TurnSignals<'_>,
    idle: Duration,
) -> TurnWait {
    let mut waiting_since: Option<tokio::time::Instant> = None;
    let mut seen_activity: Option<tokio::time::Instant> = None;
    loop {
        let last = *activity.borrow_and_update();
        let elapsed = last.elapsed();

        // A new activity tick is the agent doing something, so the wait is not
        // stuck — it is a long command that keeps reporting. Only a wait that
        // produces nothing at all for the whole ceiling is a hang.
        if seen_activity != Some(last) {
            seen_activity = Some(last);
            waiting_since = None;
        }

        let waiting_on_user = signals.waiting().await;

        // A signal that never clears disables the watchdog entirely, so it needs
        // a ceiling of its own.
        if waiting_on_user {
            let since = *waiting_since.get_or_insert_with(tokio::time::Instant::now);
            if since.elapsed() >= WAITING_CEILING {
                return TurnWait::Stalled(rx);
            }
        } else {
            waiting_since = None;
        }

        if !waiting_on_user && elapsed >= idle {
            return TurnWait::Stalled(rx);
        }

        let wait = if waiting_on_user { idle } else { idle - elapsed };

        tokio::select! {
            biased;
            settled = &mut rx => return settle(settled),
            _ = tokio::time::sleep(wait) => {}
            changed = activity.changed() => {
                if changed.is_err() {
                    // Nothing will wake us again; let the sleep decide.
                    tokio::time::sleep(wait).await;
                }
            }
        }
    }
}

pub struct AcpBridge {
    inner: Arc<Inner>,
}

/// Where a bridge's events go.
///
/// The bridge used to hold an `AppHandle` directly, which meant nothing that
/// owned a bridge could be exercised without a Tauri app — including the whole
/// swarm engine. The only thing it ever did with the handle was `emit`, so that
/// is the entire interface: one method, and a test can supply its own.
pub trait EventSink: Send + Sync + 'static {
    fn emit(&self, event: &str, payload: Value);
}

impl EventSink for AppHandle {
    fn emit(&self, event: &str, payload: Value) {
        // Fully qualified: `AppHandle` has two `emit`s in scope now, and this is
        // the Tauri one.
        let _ = tauri::Emitter::emit(self, event, payload);
    }
}

/// A sink that drops everything.
///
/// For a bridge whose events nobody is listening to — a headless run, a test —
/// so that "no window" is a choice rather than a crash.
pub struct NullSink;

impl EventSink for NullSink {
    fn emit(&self, _event: &str, _payload: Value) {}
}

/// A sink that keeps what it was given, for tests that assert on events.
#[derive(Default)]
pub struct RecordingSink {
    events: std::sync::Mutex<Vec<(String, Value)>>,
}

impl RecordingSink {
    pub fn events(&self) -> Vec<(String, Value)> {
        self.events.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The payloads emitted on one event name, in order.
    pub fn on(&self, event: &str) -> Vec<Value> {
        self.events()
            .into_iter()
            .filter(|(name, _)| name == event)
            .map(|(_, payload)| payload)
            .collect()
    }
}

impl EventSink for RecordingSink {
    fn emit(&self, event: &str, payload: Value) {
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((event.to_string(), payload));
    }
}

struct Inner {
    app: Arc<dyn EventSink>,
    stdin: Mutex<Option<ChildStdin>>,
    child: Mutex<Option<Child>>,
    next_id: AtomicI64,
    pending: Mutex<HashMap<i64, oneshot::Sender<Result<Value, RpcError>>>>,
    interactions: Mutex<HashMap<String, oneshot::Sender<InteractionReply>>>,
    terminals: TerminalRegistry,
    status: Mutex<AgentStatus>,
    cwd: Mutex<Option<PathBuf>>,
    generation: AtomicU64,
    shutting_down: AtomicBool,
    stderr_tail: Mutex<Vec<String>>,
    /// Last moment the agent said anything. The idle watchdog measures silence
    /// from here, and every stdout line resets it.
    activity: watch::Sender<tokio::time::Instant>,
    /// True from the moment a `session/prompt` is written until the agent
    /// answers it. This — not the status string — is what decides whether a
    /// second prompt is allowed through.
    turn_active: AtomicBool,
    /// Requests the agent has open against us right now. A `terminal/wait_for_exit`
    /// can block for minutes, and during it the agent is waiting on a tool, not
    /// wedged — so the idle watchdog must not count that silence.
    inbound: AtomicU64,
    /// Tool calls the agent has announced but not finished. A 250s shell
    /// command produces no output at all, so this is the only thing that
    /// distinguishes "running a tool" from "wedged".
    open_tools: Mutex<HashSet<String>>,
    /// Who this bridge speaks for, when it is not the main session.
    ///
    /// A swarm member runs its own `vibe-acp` process. Without a scope its
    /// notifications would land on `acp://notification` and the main chat would
    /// render a worker's output as if the user had asked for it. The scope is
    /// the whole mechanism: the main window listens on the bare name, a member
    /// emits on `acp://notification/swarm/w-t1`, and the two never meet.
    scope: Option<String>,
    /// Whether this bridge speaks for the window, and so may move the folder the
    /// app remembers opening in.
    ///
    /// It used to be inferred from `scope.is_none()`, which was wrong twice. A
    /// swarm member is scoped, so it could not; a catalogue probe is unscoped
    /// and could — it runs in `%TEMP%ibedesk-catalog-probe`, and remembering
    /// that made every later launch open there, and every swarm run bind to a
    /// temp directory the user never named. The intent belongs to the caller,
    /// not to an absence of scope.
    remember: bool,
}

impl AcpBridge {
    pub fn new(app: AppHandle) -> Self {
        let mut bridge = Self::with_sink(Arc::new(app));
        // `with_sink` has not spawned anything yet, so the only handle to the
        // inner state is still unique here.
        if let Some(inner) = Arc::get_mut(&mut bridge.inner) {
            inner.remember = true;
        }
        bridge
    }

    /// A bridge that reports to an arbitrary sink.
    ///
    /// This is what makes the engine headless-testable: the same process
    /// handling, the same protocol, the same scoping, and no window.
    pub fn with_sink(app: Arc<dyn EventSink>) -> Self {
        Self {
            inner: Arc::new(Inner {
                app,
                stdin: Mutex::new(None),
                child: Mutex::new(None),
                next_id: AtomicI64::new(1),
                pending: Mutex::new(HashMap::new()),
                interactions: Mutex::new(HashMap::new()),
                terminals: TerminalRegistry::default(),
                status: Mutex::new(AgentStatus::default()),
                cwd: Mutex::new(None),
                generation: AtomicU64::new(0),
                shutting_down: AtomicBool::new(false),
                stderr_tail: Mutex::new(Vec::new()),
                activity: watch::channel(tokio::time::Instant::now()).0,
                turn_active: AtomicBool::new(false),
                inbound: AtomicU64::new(0),
                open_tools: Mutex::new(HashSet::new()),
                scope: None,
                remember: false,
            }),
        }
    }

    /// A bridge that speaks for one swarm member.
    ///
    /// Same process handling, same protocol, different event namespace. The
    /// alternative — one process with N sessions — would have needed the
    /// session id threaded through every emit site, and a member that crashed
    /// would have taken the run with it.
    ///
    /// A builder rather than a constructor, so it composes with [`with_sink`]:
    /// `AcpBridge::with_sink(sink).scoped("swarm/w-t1")`.
    pub fn scoped(mut self, scope: impl Into<String>) -> Self {
        // `Inner` is behind an `Arc` and shared with the reader tasks, so the
        // scope is set before `start` is ever called and never mutated after.
        // SAFETY-free by construction: nothing has been spawned yet.
        if let Some(inner) = Arc::get_mut(&mut self.inner) {
            inner.scope = Some(scope.into());
        }
        self
    }

    pub async fn status(&self) -> AgentStatus {
        self.inner.status.lock().await.clone()
    }

    pub async fn start(&self, cwd: Option<PathBuf>) -> AppResult<AgentStatus> {
        if self.inner.child.lock().await.is_some() {
            return Ok(self.status().await);
        }

        let resolution = paths::resolve_acp_binary()?;
        let workspace = cwd.unwrap_or_else(paths::default_workspace);
        if !workspace.is_dir() {
            return Err(AppError::message(format!(
                "working folder does not exist: {}",
                workspace.display()
            )));
        }

        self.inner.shutting_down.store(false, Ordering::SeqCst);
        let generation = self.inner.generation.fetch_add(1, Ordering::SeqCst) + 1;

        {
            let mut status = self.inner.status.lock().await;
            status.state = "starting".into();
            status.binary = Some(resolution.path.display().to_string());
            status.cwd = Some(workspace.display().to_string());
            status.error = None;
            status.version = None;
            status.session_id = None;
        }
        *self.inner.cwd.lock().await = Some(workspace.clone());
        self.inner.emit_status().await;

        let mut command = Command::new(&resolution.path);
        command
            .current_dir(&workspace)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .env("PYTHONUNBUFFERED", "1")
            .env("PYTHONIOENCODING", "utf-8");

        if let Some(home) = std::env::var_os("VIBE_HOME") {
            command.env("VIBE_HOME", home);
        }

        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }

        let mut child = command.spawn().map_err(|e| {
            AppError::message(format!(
                "could not start {}: {e}",
                resolution.path.display()
            ))
        })?;

        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let pid = child.id();

        // The engine can be killed rather than shut down, and then nothing here
        // runs to stop this child. The job object is the kernel's copy of that
        // promise: no member outlives the process that started it.
        if let Some(pid) = pid {
            super::job::adopt(pid);
        }

        *self.inner.stdin.lock().await = stdin;
        *self.inner.child.lock().await = Some(child);

        {
            let mut status = self.inner.status.lock().await;
            status.pid = pid;
            status.started_at = Some(chrono::Utc::now().to_rfc3339());
        }

        if let Some(stdout) = stdout {
            let inner = self.inner.clone();
            tokio::spawn(async move { read_stdout(inner, stdout, generation).await });
        }
        if let Some(stderr) = stderr {
            let inner = self.inner.clone();
            tokio::spawn(async move { read_stderr(inner, stderr).await });
        }

        // Handshake. A failure here means the binary is not an ACP agent.
        let init = self
            .inner
            .request(
                "initialize",
                json!({
                    "protocolVersion": 1,
                    "clientCapabilities": client_capabilities(),
                    "clientInfo": client_info(),
                }),
                INIT_TIMEOUT,
            )
            .await;

        match init {
            Ok(result) => {
                let version = result
                    .get("agentInfo")
                    .and_then(|v| v.get("version"))
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let mut status = self.inner.status.lock().await;
                status.state = "ready".into();
                status.version = version;
                status.error = None;
                drop(status);
                self.inner.emit_status().await;
                // Only the window's own bridge decides where the window opens.
                // A swarm member starts a bridge of its own in its worktree, and
                // remembering that path made the app reopen in a folder the run
                // deletes: after the thirteenth live run the header read
                // `...\.vibe\worktreesun-...-t2`, a verifier's checkout that
                // no longer existed. A scoped bridge is a member, not the user.
                //
                // A probe is neither: the model catalogue opens a session in a
                // scratch directory to read the levels of models the session is
                // not on, and remembering *that* folder moved the user's
                // workspace to `%TEMP%`. `remember` is set by the constructor
                // the window uses and by nothing else.
                if self.inner.remember {
                    crate::settings::remember_workspace(&workspace);
                }
                self.inner
                    .log("info", format!("agent ready · {}", resolution.path.display()))
                    .await;
                Ok(self.status().await)
            }
            Err(error) => {
                let message = error.to_string();
                self.inner.stop_process().await;
                let mut status = self.inner.status.lock().await;
                status.state = "failed".into();
                status.error = Some(message.clone());
                drop(status);
                self.inner.emit_status().await;
                Err(AppError::message(message))
            }
        }
    }

    pub async fn stop(&self) -> AppResult<()> {
        self.inner.shutting_down.store(true, Ordering::SeqCst);
        self.inner.stop_process().await;
        let mut status = self.inner.status.lock().await;
        status.state = "stopped".into();
        status.pid = None;
        status.session_id = None;
        drop(status);
        self.inner.emit_status().await;
        Ok(())
    }

    pub async fn restart(&self, cwd: Option<PathBuf>) -> AppResult<AgentStatus> {
        let previous = self.inner.cwd.lock().await.clone();
        self.stop().await?;
        {
            let mut status = self.inner.status.lock().await;
            status.restarts += 1;
        }
        self.start(cwd.or(previous)).await
    }

    pub async fn request(&self, method: &str, params: Value) -> AppResult<Value> {
        self.inner.request(method, params, REQUEST_TIMEOUT).await
    }

    pub async fn notify(&self, method: &str, params: Value) -> AppResult<()> {
        self.inner.notify(method, params).await
    }

    pub async fn set_session(&self, session_id: Option<String>) {
        let mut status = self.inner.status.lock().await;
        status.session_id = session_id;
        drop(status);
        self.inner.emit_status().await;
    }

    /// Whether a turn is in flight.
    ///
    /// The engine needs this to tell "the lead is thinking" from "the lead's turn
    /// ended and it wrote nothing". Both look like silence from the outside, and
    /// only one of them is worth a second prompt.
    pub fn busy(&self) -> bool {
        self.inner.turn_active.load(Ordering::SeqCst)
    }

    /// Ask the agent to abandon the turn in flight.
    ///
    /// ACP has no "stop after this tool call", so this is a request rather than a
    /// command: the agent finishes what it is doing and then ends the turn. That
    /// is exactly the granularity the swarm needs — a worker that has posted its
    /// evidence has nothing left to do, and letting it keep going spends tokens
    /// re-doing work a verifier is already reading.
    ///
    /// Best effort by design. A cancel that arrives after the turn ended is a
    /// no-op, and a bridge that is not running has nothing to cancel.
    pub async fn cancel(&self, session_id: &str) -> AppResult<()> {
        if !self.busy() {
            return Ok(());
        }
        self.inner
            .notify("session/cancel", serde_json::json!({ "sessionId": session_id }))
            .await
    }

    /// Run one agent turn.
    ///
    /// A turn is bounded by silence, not by a clock. When the idle watchdog
    /// fires the request is abandoned but the turn is not: the agent may still
    /// be working, so `busy` stays set and a reaper clears it when the real
    /// response finally arrives. Clearing `busy` on the timeout is what used to
    /// let the next prompt through to `-32603 A turn is already running`.
    pub async fn turn(&self, method: &str, params: Value) -> AppResult<Value> {
        if self.inner.turn_active.swap(true, Ordering::SeqCst) {
            return Err(AppError::TurnInProgress);
        }
        let idle = turn_idle_timeout();
        let started = std::time::Instant::now();
        self.inner.set_state("busy").await;
        self.inner.emit_status().await;

        match self.inner.request_turn(method, params).await {
            Ok(TurnWait::Settled(Ok(value))) => {
                self.finish_turn().await;
                if let Some(usage) = usage_from_turn(&value, started.elapsed().as_secs_f64()) {
                    self.inner.emit_usage(usage).await;
                }
                Ok(value)
            }
            Ok(TurnWait::Settled(Err(rpc))) => {
                self.finish_turn().await;
                Err(AppError::Rpc {
                    code: rpc.code,
                    message: rpc.message,
                })
            }
            Ok(TurnWait::Closed) => {
                self.finish_turn().await;
                Err(AppError::AgentExited(
                    "the agent closed the connection".into(),
                ))
            }
            Ok(TurnWait::Stalled(rx)) => {
                let inner = self.inner.clone();
                tokio::spawn(async move {
                    // A late reply still ends the turn. A reply that never comes
                    // is a dead turn, and waiting on it forever is what made the
                    // engine's 601s idle limit unreachable: `busy` stayed set, so
                    // the engine used its 1800s turn ceiling instead.
                    if tokio::time::timeout(STALL_GRACE, rx).await.is_err() {
                        inner.stop_process().await;
                    }
                    inner.turn_active.store(false, Ordering::SeqCst);
                    inner.set_state("ready").await;
                    inner.emit_status().await;
                    let event = scoped_name(inner.scope.as_deref(), EVENT_TURN_ENDED);
                    inner.app.emit(&event, json!({ "reason": "stalled" }));
                });
                self.inner
                    .log(
                        "warn",
                        format!(
                            "no agent output for {}s — the turn is still running",
                            idle.as_secs()
                        ),
                    )
                    .await;
                Err(AppError::TurnStalled(idle.as_secs()))
            }
            Err(error) => {
                self.finish_turn().await;
                Err(error)
            }
        }
    }

    /// A request that streams progress while it runs, so silence is the only
    /// failure signal. `session/load` replays history this way and can easily
    /// outlast a fixed deadline on a large chat.
    pub async fn request_streaming(&self, method: &str, params: Value) -> AppResult<Value> {
        match self.inner.request_turn(method, params).await {
            Ok(TurnWait::Settled(Ok(value))) => Ok(value),
            Ok(TurnWait::Settled(Err(rpc))) => Err(AppError::Rpc {
                code: rpc.code,
                message: rpc.message,
            }),
            Ok(TurnWait::Closed) => Err(AppError::AgentExited(
                "the agent closed the connection".into(),
            )),
            Ok(TurnWait::Stalled(_)) => Err(AppError::Timeout(turn_idle_timeout().as_secs())),
            Err(error) => Err(error),
        }
    }

    async fn finish_turn(&self) {
        self.inner.turn_active.store(false, Ordering::SeqCst);
        self.inner.set_state("ready").await;
        self.inner.emit_status().await;
    }

    pub async fn respond_permission(&self, request_id: &str, option_id: Option<String>) -> AppResult<()> {
        let sender = self.inner.interactions.lock().await.remove(request_id);
        match sender {
            Some(tx) => {
                // The agent was silent because it was waiting on the user. The
                // idle clock restarts from the answer, not from the question.
                self.inner.activity.send_replace(tokio::time::Instant::now());
                let _ = tx.send(InteractionReply::Permission(option_id));
                Ok(())
            }
            None => Err(AppError::message(format!(
                "no pending approval request {request_id}"
            ))),
        }
    }

    /// Register a pending interaction without an agent behind it.
    ///
    /// A real request is registered by the agent's own `session/request_permission`
    /// call, which needs a live `vibe-acp`. This lets a test observe *who answers*
    /// a request — the swarm's permission gate — without one.
    #[cfg(test)]
    pub async fn expect_interaction(
        &self,
        request_id: &str,
    ) -> tokio::sync::oneshot::Receiver<InteractionReply> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.inner
            .interactions
            .lock()
            .await
            .insert(request_id.to_string(), tx);
        rx
    }

    pub async fn respond_elicitation(
        &self,
        request_id: &str,
        action: String,
        content: Option<Value>,
    ) -> AppResult<()> {
        let sender = self.inner.interactions.lock().await.remove(request_id);
        match sender {
            Some(tx) => {
                self.inner.activity.send_replace(tokio::time::Instant::now());
                let _ = tx.send(InteractionReply::Elicitation { action, content });
                Ok(())
            }
            None => Err(AppError::message(format!(
                "no pending elicitation {request_id}"
            ))),
        }
    }

    pub async fn cwd(&self) -> Option<PathBuf> {
        self.inner.cwd.lock().await.clone()
    }

    /// Push a line into the bridge log the Status panel renders.
    pub async fn log(&self, level: &str, message: impl Into<String>) {
        self.inner.log(level, message).await;
    }

    /// Point the bridge at another folder without restarting the child.
    ///
    /// ACP carries the working folder per session, so switching workstations
    /// only needs the bridge to agree: the file index and fork target follow
    /// this value, while the agent resolves tool paths from the session cwd.
    pub async fn set_cwd(&self, cwd: PathBuf) {
        *self.inner.cwd.lock().await = Some(cwd);
    }

    pub async fn shutdown(&self) {
        let _ = self.stop().await;
    }
}

impl Inner {
    async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> AppResult<Value> {
        let (id, rx) = self.send(method, params).await?;

        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(Ok(value))) => Ok(value),
            Ok(Ok(Err(rpc))) => Err(AppError::Rpc {
                code: rpc.code,
                message: rpc.message,
            }),
            Ok(Err(_)) => Err(AppError::AgentExited(
                "the agent closed the connection".into(),
            )),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                Err(AppError::Timeout(timeout.as_secs()))
            }
        }
    }

    /// Write a request and hand back its reply channel.
    async fn send(
        &self,
        method: &str,
        params: Value,
    ) -> AppResult<(i64, oneshot::Receiver<Result<Value, RpcError>>)> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        let payload = serde_json::to_value(OutgoingRequest::new(id, method, params))?;
        if let Err(error) = self.write_value(payload).await {
            self.pending.lock().await.remove(&id);
            return Err(error);
        }

        Ok((id, rx))
    }

    /// Wait for a reply, extending the deadline every time the agent speaks.
    ///
    /// The pending entry is deliberately left in place when the watchdog
    /// fires: the agent has not answered yet, and dropping the sender would
    /// make its eventual reply vanish instead of ending the turn.
    async fn request_turn(&self, method: &str, params: Value) -> AppResult<TurnWait> {
        let (_id, rx) = self.send(method, params).await?;
        Ok(await_turn(
            rx,
            self.activity.subscribe(),
            &TurnSignals {
                interactions: &self.interactions,
                inbound: &self.inbound,
                open_tools: &self.open_tools,
            },
            turn_idle_timeout(),
        )
        .await)
    }

    async fn set_state(&self, state: &str) {
        let mut status = self.status.lock().await;
        if status.state == "ready" || status.state == "busy" {
            status.state = state.into();
        }
    }

    async fn notify(&self, method: &str, params: Value) -> AppResult<()> {
        let payload = serde_json::to_value(OutgoingNotification::new(method, params))?;
        self.write_value(payload).await
    }

    async fn write_value(&self, value: Value) -> AppResult<()> {
        let mut guard = self.stdin.lock().await;
        let stdin = guard.as_mut().ok_or(AppError::AgentNotRunning)?;
        let mut line = serde_json::to_string(&value)?;
        line.push('\n');
        stdin.write_all(line.as_bytes()).await?;
        stdin.flush().await?;
        Ok(())
    }

    async fn stop_process(&self) {
        let _ = self.terminals.kill_all().await;
        // Kill before taking the stdin lock: a write blocked on a full pipe
        // holds that lock, and the kill is what breaks the pipe and frees it.
        let child = self.child.lock().await.take();
        if let Some(mut child) = child {
            let _ = child.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
        }
        self.stdin.lock().await.take();
        self.pending.lock().await.clear();
        self.interactions.lock().await.clear();
        self.open_tools.lock().await.clear();
        // The process that owned the turn is gone, so the turn is over.
        self.turn_active.store(false, Ordering::SeqCst);
    }

    /// The event name for this bridge, scoped when it is not the main session.
    fn event(&self, name: &str) -> String {
        scoped_name(self.scope.as_deref(), name)
    }

    async fn emit_status(&self) {
        let status = self.status.lock().await.clone();
        self.app.emit(&self.event(EVENT_STATUS), json!(status));
    }

    pub async fn log(&self, level: &str, message: impl Into<String>) {
        self.app.emit(
            &self.event(EVENT_LOG),
            json!({
                "level": level,
                "message": message.into(),
                "at": chrono::Utc::now().to_rfc3339(),
            }),
        );
    }

    /// Track which tool calls the agent has announced and not yet finished.
    ///
    /// A shell command that runs for four minutes emits nothing at all, so
    /// without this the idle watchdog would call a working agent wedged.
    async fn track_tool_activity(&self, method: &str, params: &Value) {
        track_tool_activity(&self.open_tools, method, params).await;
    }

    async fn emit_notification(&self, method: &str, params: Value) {
        self.app.emit(
            &self.event(EVENT_NOTIFICATION),
            json!({ "method": method, "params": params }),
        );
    }

    /// Publish a usage reading on this bridge's scoped event.
    async fn emit_usage(&self, payload: Value) {
        self.app.emit(&self.event(EVENT_USAGE), payload);
    }

    async fn fail_pending(&self, reason: &str) {
        let mut pending = self.pending.lock().await;
        for (_, sender) in pending.drain() {
            let _ = sender.send(Err(RpcError::internal(reason.to_string())));
        }
    }
}

/// The event name a bridge emits on, given its scope.
///
/// One rule, one owner. The main session has no scope and keeps the bare
/// names; a swarm member appends its scope so its output cannot be mistaken
/// for the main session's. The reader tasks hold an `Arc<Inner>` rather than
/// an `AcpBridge`, so this is a free function rather than a method.
fn scoped_name(scope: Option<&str>, name: &str) -> String {
    match scope {
        Some(scope) => format!("{name}/{scope}"),
        None => name.to_string(),
    }
}

fn scoped_event(inner: &Inner, name: &str) -> String {
    scoped_name(inner.scope.as_deref(), name)
}

async fn read_stdout(inner: Arc<Inner>, stdout: ChildStdout, generation: u64) {
    let mut lines = BufReader::new(stdout).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                // Any byte from the agent is liveness, including a notification
                // that carries no reply.
                inner.activity.send_replace(tokio::time::Instant::now());
                dispatch_line(inner.clone(), trimmed).await;
            }
            Ok(None) => break,
            Err(error) => {
                inner.log("warn", format!("stdout read error: {error}")).await;
                break;
            }
        }
    }

    if inner.generation.load(Ordering::SeqCst) != generation {
        return;
    }
    if inner.shutting_down.load(Ordering::SeqCst) {
        return;
    }

    let tail = inner.stderr_tail.lock().await.clone();
    let detail = tail
        .iter()
        .rev()
        .find(|line| !line.trim().is_empty())
        .cloned()
        .unwrap_or_else(|| "the agent process exited".into());

    inner.fail_pending(&detail).await;
    inner.stop_process().await;
    {
        let mut status = inner.status.lock().await;
        status.state = "failed".into();
        status.pid = None;
        status.error = Some(detail.clone());
    }
    inner.emit_status().await;
    inner.log("error", detail).await;
}

async fn read_stderr(inner: Arc<Inner>, stderr: tokio::process::ChildStderr) {
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let trimmed = line.trim().to_string();
        if trimmed.is_empty() {
            continue;
        }
        {
            let mut tail = inner.stderr_tail.lock().await;
            tail.push(trimmed.clone());
            if tail.len() > 40 {
                tail.remove(0);
            }
        }
        inner.log("debug", trimmed).await;
    }
}

async fn dispatch_line(inner: Arc<Inner>, line: &str) {
    let message: Incoming = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(error) => {
            inner
                .log("warn", format!("unparseable agent output: {error}"))
                .await;
            return;
        }
    };

    match (message.id, message.method) {
        (Some(id), Some(method)) => {
            let params = message.params.unwrap_or(Value::Null);
            let inner = inner.clone();
            tokio::spawn(async move {
                handle_agent_request(inner, id, method, params).await;
            });
        }
        (Some(id), None) => {
            let key = id.as_i64();
            let sender = match key {
                Some(key) => inner.pending.lock().await.remove(&key),
                None => None,
            };
            if let Some(sender) = sender {
                let outcome = match message.error {
                    Some(error) => Err(error),
                    None => Ok(message.result.unwrap_or(Value::Null)),
                };
                let _ = sender.send(outcome);
            }
        }
        (None, Some(method)) => {
            let params = message.params.unwrap_or(Value::Null);
            inner.track_tool_activity(&method, &params).await;
            if let Some(update) = params.get("update") {
                if let Some(usage) = usage_from_update(update) {
                    inner.emit_usage(usage).await;
                }
            }
            inner.emit_notification(&method, params).await;
        }
        (None, None) => {}
    }
}

/// Counts an open agent request for as long as it is being served, and restarts
/// the idle clock when it closes.
struct InboundGuard(Arc<Inner>);

impl InboundGuard {
    fn open(inner: Arc<Inner>) -> Self {
        inner.inbound.fetch_add(1, Ordering::SeqCst);
        Self(inner)
    }
}

impl Drop for InboundGuard {
    fn drop(&mut self) {
        self.0.inbound.fetch_sub(1, Ordering::SeqCst);
        // The agent was quiet because it was waiting on this. The idle clock
        // restarts from the answer, not from the question.
        self.0.activity.send_replace(tokio::time::Instant::now());
    }
}

async fn handle_agent_request(inner: Arc<Inner>, id: Value, method: String, params: Value) {
    let _open = InboundGuard::open(inner.clone());

    // Every request the agent makes of us, when tracing is on. Whether the
    // agent actually uses the boundaries we advertise is the question the
    // capability layer lives or dies on, and it cannot be answered by reading
    // the code: a client that claims `fs` and `terminal` and is never asked for
    // either looks identical to one that is asked constantly.
    if std::env::var_os("VIBEDESK_TRACE_ACP").is_some() {
        eprintln!("acp: agent → client: {method} {}", summarise_params(&params));
    }

    let outcome: Result<Value, RpcError> = match method.as_str() {
        "fs/read_text_file" => read_text_file(&params).await,
        "fs/write_text_file" => write_text_file(&params).await,
        "terminal/create" => inner.terminals.create(&params).await.map_err(to_rpc),
        "terminal/output" => inner.terminals.output(&params).await.map_err(to_rpc),
        "terminal/wait_for_exit" => inner.terminals.wait_for_exit(&params).await.map_err(to_rpc),
        "terminal/kill" => inner.terminals.kill(&params).await.map_err(to_rpc),
        "terminal/release" => inner.terminals.release(&params).await.map_err(to_rpc),
        "session/request_permission" => request_permission(inner.clone(), &params).await,
        "elicitation/create" => create_elicitation(inner.clone(), &params).await,
        other => Err(RpcError::method_not_found(other)),
    };

    let response = match outcome {
        Ok(value) => success_response(&id, value),
        Err(error) => error_response(&id, error),
    };
    if let Err(error) = inner.write_value(response).await {
        inner
            .log("warn", format!("could not answer {method}: {error}"))
            .await;
    }
}

fn to_rpc(error: AppError) -> RpcError {
    RpcError::internal(error.to_string())
}

/// The parts of an agent request worth seeing in a trace, without the payload.
///
/// A read is interesting for its `path` and whether it carries `line`/`limit`,
/// because a paged read is the case the sparse-reading gate deliberately leaves
/// alone. A terminal is interesting for its command. Nothing else is printed:
/// a trace that dumps whole file contents is a trace nobody leaves on.
fn summarise_params(params: &Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    for key in ["path", "line", "limit", "command", "cwd"] {
        let Some(value) = params.get(key) else { continue };
        let text = match value {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        };
        let clipped: String = text.chars().take(120).collect();
        parts.push(format!("{key}={clipped}"));
    }
    if let Some(argv) = params.get("args").and_then(Value::as_array) {
        let joined: Vec<&str> = argv.iter().filter_map(Value::as_str).collect();
        parts.push(format!("args={}", joined.join(" ")));
    }
    parts.join(" ")
}

async fn read_text_file(params: &Value) -> Result<Value, RpcError> {
    let path = params
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params("fs/read_text_file requires a path"))?;

    let content = tokio::fs::read_to_string(path)
        .await
        .map_err(|e| RpcError::internal(format!("could not read {path}: {e}")))?;

    let line = params.get("line").and_then(Value::as_u64).unwrap_or(1).max(1) as usize;
    let limit = params.get("limit").and_then(Value::as_u64).map(|v| v as usize);

    // A request that already carries `line` or `limit` is the agent asking for
    // exact bytes. That is the whole point of the sparse-reading card, so it is
    // never second-guessed.
    let paging = params.get("line").is_some() || params.get("limit").is_some();

    if !paging {
        if crate::capabilities::registry()
            .is_enabled(crate::capabilities::CapabilityId::Reading)
        {
            let outcome = crate::capabilities::sparse::apply(Path::new(path), content, false);
            record_reading(path, &outcome);
            return Ok(json!({ "content": outcome.content }));
        }
        return Ok(json!({ "content": content }));
    }

    let selected: Vec<&str> = content
        .lines()
        .skip(line - 1)
        .take(limit.unwrap_or(usize::MAX))
        .collect();

    Ok(json!({ "content": selected.join("\n") }))
}

/// Report one read to the capability registry.
///
/// Pass-throughs are counted too: the panel's job is to show how often the gate
/// said no, which is most of the time and is the reason it can be trusted when
/// it says yes.
fn record_reading(path: &str, outcome: &crate::capabilities::sparse::ReadOutcome) {
    use crate::capabilities::compact::types::estimate_tokens_chars;
    use crate::capabilities::{now, registry, CapabilityEvent, CapabilityId};

    let registry = registry();
    if !outcome.intercepted {
        registry.bump(CapabilityId::Reading, "passed", 1);
        return;
    }
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

async fn write_text_file(params: &Value) -> Result<Value, RpcError> {
    let path = params
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params("fs/write_text_file requires a path"))?;
    let content = params
        .get("content")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params("fs/write_text_file requires content"))?;

    if let Some(parent) = Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| RpcError::internal(format!("could not create {}: {e}", parent.display())))?;
        }
    }

    tokio::fs::write(path, content)
        .await
        .map_err(|e| RpcError::internal(format!("could not write {path}: {e}")))?;

    Ok(Value::Null)
}

async fn request_permission(inner: Arc<Inner>, params: &Value) -> Result<Value, RpcError> {
    let request_id = format!(
        "perm-{}",
        inner.next_id.fetch_add(1, Ordering::SeqCst)
    );
    let (tx, rx) = oneshot::channel();
    inner
        .interactions
        .lock()
        .await
        .insert(request_id.clone(), tx);

    inner.app.emit(
        &scoped_event(&inner, EVENT_REQUEST),
        json!({
            "requestId": request_id,
            "method": "session/request_permission",
            "params": params,
        }),
    );

    match tokio::time::timeout(INTERACTION_TIMEOUT, rx).await {
        Ok(Ok(InteractionReply::Permission(Some(option_id)))) => Ok(json!({
            "outcome": { "outcome": "selected", "optionId": option_id }
        })),
        Ok(Ok(InteractionReply::Permission(None))) | Ok(Err(_)) | Err(_) => {
            inner.interactions.lock().await.remove(&request_id);
            Ok(json!({ "outcome": { "outcome": "cancelled" } }))
        }
        Ok(Ok(InteractionReply::Elicitation { .. })) => Ok(json!({
            "outcome": { "outcome": "cancelled" }
        })),
    }
}

async fn create_elicitation(inner: Arc<Inner>, params: &Value) -> Result<Value, RpcError> {
    let request_id = format!(
        "elicit-{}",
        inner.next_id.fetch_add(1, Ordering::SeqCst)
    );
    let (tx, rx) = oneshot::channel();
    inner
        .interactions
        .lock()
        .await
        .insert(request_id.clone(), tx);

    inner.app.emit(
        &scoped_event(&inner, EVENT_REQUEST),
        json!({
            "requestId": request_id,
            "method": "elicitation/create",
            "params": params,
        }),
    );

    match tokio::time::timeout(INTERACTION_TIMEOUT, rx).await {
        Ok(Ok(InteractionReply::Elicitation { action, content })) => {
            let mut payload = json!({ "action": action });
            if let Some(content) = content {
                payload["content"] = content;
            }
            Ok(payload)
        }
        Ok(Ok(InteractionReply::Permission(_))) | Ok(Err(_)) | Err(_) => {
            inner.interactions.lock().await.remove(&request_id);
            Ok(json!({ "action": "cancel" }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scoped bridge must not emit on the main session's event names.
    ///
    /// This is the whole isolation mechanism for a swarm member: the main
    /// window listens on `acp://notification`, so a member that emitted there
    /// would have its output rendered as if the user had asked for it.
    #[test]
    fn a_scoped_bridge_namespaces_every_event_it_emits() {
        let scope = Some("swarm/w-t1");
        assert_eq!(
            scoped_name(scope, EVENT_NOTIFICATION),
            "acp://notification/swarm/w-t1"
        );
        assert_eq!(scoped_name(scope, EVENT_REQUEST), "acp://request/swarm/w-t1");
        assert_eq!(scoped_name(scope, EVENT_STATUS), "agent://status/swarm/w-t1");
        assert_eq!(scoped_name(scope, EVENT_LOG), "agent://log/swarm/w-t1");
        assert_eq!(
            scoped_name(scope, EVENT_TURN_ENDED),
            "agent://turn-ended/swarm/w-t1"
        );
    }

    /// The main session keeps the bare names, so nothing about its behaviour
    /// changes when a swarm is running beside it.
    #[test]
    fn an_unscoped_bridge_keeps_the_bare_event_names() {
        assert_eq!(scoped_name(None, EVENT_NOTIFICATION), EVENT_NOTIFICATION);
        assert_eq!(scoped_name(None, EVENT_STATUS), EVENT_STATUS);
        assert_eq!(scoped_name(None, EVENT_TURN_ENDED), EVENT_TURN_ENDED);
    }

    /// Two members never collide, and neither collides with the main session.
    #[test]
    fn two_scopes_never_produce_the_same_event_name() {
        let a = scoped_name(Some("swarm/w-t1"), EVENT_NOTIFICATION);
        let b = scoped_name(Some("swarm/w-t2"), EVENT_NOTIFICATION);
        let main = scoped_name(None, EVENT_NOTIFICATION);
        assert_ne!(a, b);
        assert_ne!(a, main);
        assert_ne!(b, main);
    }

    #[test]
    fn an_mcp_tool_name_maps_to_the_capability_it_serves() {
        use crate::capabilities::CapabilityId;

        assert_eq!(
            mcp_tool("mcp_codebase_memory.index_repository"),
            Some((CapabilityId::CodeGraph, "index_repository"))
        );
        assert_eq!(
            mcp_tool("mcp_donsetch.web_search"),
            Some((CapabilityId::Web, "web_search"))
        );
    }

    #[test]
    fn a_native_tool_is_not_an_mcp_tool() {
        assert!(mcp_tool("read_file").is_none());
        assert!(mcp_tool("mcp_unknown.thing").is_none());
        assert!(mcp_tool("mcp_donsetch").is_none());
    }

    #[test]
    fn the_detail_names_what_was_searched_or_indexed() {
        assert_eq!(
            tool_detail(Some(&json!({ "query": "rust async" }))),
            "query: rust async"
        );
        assert_eq!(
            tool_detail(Some(&json!({ "url": "https://example.com" }))),
            "url: https://example.com"
        );
        assert_eq!(
            tool_detail(Some(&json!({ "project": "my-repo" }))),
            "project: my-repo"
        );
    }

    #[test]
    fn a_blank_or_missing_argument_is_an_empty_detail() {
        assert_eq!(tool_detail(None), "");
        assert_eq!(tool_detail(Some(&json!({}))), "");
        assert_eq!(tool_detail(Some(&json!({ "query": "   " }))), "");
    }

    fn clock() -> (
        watch::Sender<tokio::time::Instant>,
        watch::Receiver<tokio::time::Instant>,
    ) {
        watch::channel(tokio::time::Instant::now())
    }

    /// Owns the three signal sources so a test can hand out a borrowing view.
    #[derive(Default)]
    struct Signals {
        interactions: Mutex<HashMap<String, oneshot::Sender<InteractionReply>>>,
        inbound: AtomicU64,
        open_tools: Mutex<HashSet<String>>,
    }

    impl Signals {
        fn view(&self) -> TurnSignals<'_> {
            TurnSignals {
                interactions: &self.interactions,
                inbound: &self.inbound,
                open_tools: &self.open_tools,
            }
        }
    }

    /// The defect this module exists to fix: a turn that runs longer than the
    /// old 120s wall clock used to be abandoned mid-flight.
    #[tokio::test(start_paused = true)]
    async fn a_turn_that_keeps_talking_outlives_the_old_wall_clock() {
        let (reply_tx, reply_rx) = oneshot::channel();
        let (activity_tx, activity_rx) = clock();
        let signals = Signals::default();

        // Ten minutes of work, reporting every 60s — five times the old limit.
        tokio::spawn(async move {
            for _ in 0..10 {
                tokio::time::sleep(Duration::from_secs(60)).await;
                activity_tx.send_replace(tokio::time::Instant::now());
            }
            let _ = reply_tx.send(Ok(json!({ "stopReason": "end_turn" })));
        });

        let outcome = await_turn(reply_rx, activity_rx, &signals.view(), TURN_IDLE_TIMEOUT).await;

        match outcome {
            TurnWait::Settled(Ok(value)) => assert_eq!(value["stopReason"], "end_turn"),
            _ => panic!("a turn that keeps streaming must never be abandoned"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_silent_turn_stalls_instead_of_hanging_forever() {
        // The sender is held so the channel stays open: silence, not closure.
        let (_reply_tx, reply_rx) = oneshot::channel();
        let (_activity_tx, activity_rx) = clock();
        let signals = Signals::default();

        let outcome = await_turn(reply_rx, activity_rx, &signals.view(), TURN_IDLE_TIMEOUT).await;

        assert!(
            matches!(outcome, TurnWait::Stalled(_)),
            "silence past the idle window must surface as a stall"
        );
    }

    /// A "waiting on the user" signal that never clears must not disable the
    /// watchdog forever.
    ///
    /// The thirteenth live run's `v-t1` sat at 0.015s of CPU for twelve minutes
    /// with `busy` set. `member_is_silent` reads `busy`, so the engine could not
    /// nudge it; `respawn_dead` reads `busy`, so it could not restart it; and the
    /// run would have waited its 1800s ceiling for a reply that was never coming.
    #[tokio::test(start_paused = true)]
    async fn a_wait_that_never_clears_stalls_at_the_ceiling() {
        let (_reply_tx, reply_rx) = oneshot::channel();
        let (_activity_tx, activity_rx) = clock();
        let signals = Signals::default();
        // An open tool call: the agent announced a command and never finished it.
        signals.open_tools.lock().await.insert("call-1".into());

        let outcome = await_turn(reply_rx, activity_rx, &signals.view(), TURN_IDLE_TIMEOUT).await;

        assert!(
            matches!(outcome, TurnWait::Stalled(_)),
            "a wait that produces nothing for the whole ceiling is a hang, not a wait"
        );
    }

    /// A wait that keeps producing activity is not a hang, however long it runs.
    #[tokio::test(start_paused = true)]
    async fn a_wait_that_keeps_reporting_outlives_the_ceiling() {
        let (reply_tx, reply_rx) = oneshot::channel();
        let (activity_tx, activity_rx) = clock();
        let signals = Signals::default();
        signals.open_tools.lock().await.insert("call-1".into());

        // A four-minute command, reporting every 30s — well past the ceiling.
        tokio::spawn(async move {
            for _ in 0..20 {
                tokio::time::sleep(Duration::from_secs(30)).await;
                activity_tx.send_replace(tokio::time::Instant::now());
            }
            let _ = reply_tx.send(Ok(json!({ "stopReason": "end_turn" })));
        });

        let outcome = await_turn(reply_rx, activity_rx, &signals.view(), TURN_IDLE_TIMEOUT).await;

        match outcome {
            TurnWait::Settled(Ok(value)) => assert_eq!(value["stopReason"], "end_turn"),
            _ => panic!("a long command that keeps reporting is working, not hung"),
        }
    }

    /// A stalled turn hands its receiver back, so the reply that eventually
    /// arrives can still end the turn instead of vanishing.
    #[tokio::test(start_paused = true)]
    async fn a_stalled_turn_still_accepts_the_late_reply() {
        let (reply_tx, reply_rx) = oneshot::channel();
        let (_activity_tx, activity_rx) = clock();
        let signals = Signals::default();

        let outcome = await_turn(reply_rx, activity_rx, &signals.view(), TURN_IDLE_TIMEOUT).await;
        let TurnWait::Stalled(rx) = outcome else {
            panic!("expected a stall");
        };

        reply_tx
            .send(Ok(json!({ "stopReason": "cancelled" })))
            .expect("the receiver must still be live after a stall");

        match rx.await {
            Ok(Ok(value)) => assert_eq!(value["stopReason"], "cancelled"),
            _ => panic!("the late reply must reach the reaper"),
        }
    }

    /// An approval prompt is the agent waiting on the user, not the agent
    /// wedged. Ten minutes of human deliberation must not trip the watchdog.
    #[tokio::test(start_paused = true)]
    async fn a_pending_approval_is_not_silence() {
        let (reply_tx, reply_rx) = oneshot::channel();
        let (activity_tx, activity_rx) = clock();
        let signals = Arc::new(Signals::default());

        let (approval_tx, _approval_rx) = oneshot::channel();
        signals
            .interactions
            .lock()
            .await
            .insert("perm-1".into(), approval_tx);

        let pending = signals.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(600)).await;
            pending.interactions.lock().await.clear();
            activity_tx.send_replace(tokio::time::Instant::now());
            let _ = reply_tx.send(Ok(json!({ "stopReason": "end_turn" })));
        });

        let outcome = await_turn(reply_rx, activity_rx, &signals.view(), TURN_IDLE_TIMEOUT).await;

        match outcome {
            TurnWait::Settled(Ok(value)) => assert_eq!(value["stopReason"], "end_turn"),
            _ => panic!("a pending approval must suppress the idle watchdog"),
        }
    }

    /// `terminal/wait_for_exit` blocks for as long as the command runs.
    #[tokio::test(start_paused = true)]
    async fn an_open_tool_request_is_not_silence() {
        let (reply_tx, reply_rx) = oneshot::channel();
        let (activity_tx, activity_rx) = clock();
        let signals = Arc::new(Signals::default());
        signals.inbound.store(1, Ordering::SeqCst);

        let open = signals.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(600)).await;
            open.inbound.store(0, Ordering::SeqCst);
            activity_tx.send_replace(tokio::time::Instant::now());
            let _ = reply_tx.send(Ok(json!({ "stopReason": "end_turn" })));
        });

        let outcome = await_turn(reply_rx, activity_rx, &signals.view(), TURN_IDLE_TIMEOUT).await;

        match outcome {
            TurnWait::Settled(Ok(value)) => assert_eq!(value["stopReason"], "end_turn"),
            _ => panic!("an open tool request must suppress the idle watchdog"),
        }
    }

    /// A four-minute shell command announces itself once and then says nothing.
    #[tokio::test(start_paused = true)]
    async fn a_running_tool_call_is_not_silence() {
        let (reply_tx, reply_rx) = oneshot::channel();
        let (activity_tx, activity_rx) = clock();
        let signals = Arc::new(Signals::default());

        let running = signals.clone();
        tokio::spawn(async move {
            track_tool_activity(
                &running.open_tools,
                "session/update",
                &json!({ "update": { "sessionUpdate": "tool_call", "toolCallId": "t1", "status": "in_progress" } }),
            )
            .await;

            tokio::time::sleep(Duration::from_secs(600)).await;

            track_tool_activity(
                &running.open_tools,
                "session/update",
                &json!({ "update": { "sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "completed" } }),
            )
            .await;
            activity_tx.send_replace(tokio::time::Instant::now());
            let _ = reply_tx.send(Ok(json!({ "stopReason": "end_turn" })));
        });

        let outcome = await_turn(reply_rx, activity_rx, &signals.view(), TURN_IDLE_TIMEOUT).await;

        match outcome {
            TurnWait::Settled(Ok(value)) => assert_eq!(value["stopReason"], "end_turn"),
            _ => panic!("a running tool call must suppress the idle watchdog"),
        }
    }

    #[tokio::test]
    async fn a_finished_tool_call_stops_suppressing_the_watchdog() {
        let open = Mutex::new(HashSet::new());

        track_tool_activity(
            &open,
            "session/update",
            &json!({ "update": { "sessionUpdate": "tool_call", "toolCallId": "t1" } }),
        )
        .await;
        assert_eq!(open.lock().await.len(), 1, "a call with no status is running");

        track_tool_activity(
            &open,
            "session/update",
            &json!({ "update": { "sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "failed" } }),
        )
        .await;
        assert!(open.lock().await.is_empty(), "a failed call is finished");
    }

    #[tokio::test]
    async fn unrelated_notifications_do_not_touch_the_tool_set() {
        let open = Mutex::new(HashSet::new());

        track_tool_activity(
            &open,
            "session/update",
            &json!({ "update": { "sessionUpdate": "agent_message_chunk", "toolCallId": "t1" } }),
        )
        .await;
        track_tool_activity(&open, "_config/changed", &json!({ "toolCallId": "t1" })).await;

        assert!(open.lock().await.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_closed_channel_is_reported_as_closed() {
        let (reply_tx, reply_rx) = oneshot::channel();
        let (_activity_tx, activity_rx) = clock();
        let signals = Signals::default();

        drop(reply_tx);

        let outcome = await_turn(reply_rx, activity_rx, &signals.view(), TURN_IDLE_TIMEOUT).await;
        assert!(matches!(outcome, TurnWait::Closed));
    }

    #[tokio::test(start_paused = true)]
    async fn an_agent_error_settles_rather_than_stalling() {
        let (reply_tx, reply_rx) = oneshot::channel();
        let (_activity_tx, activity_rx) = clock();
        let signals = Signals::default();

        tokio::spawn(async move {
            let _ = reply_tx.send(Err(RpcError::internal("boom".to_string())));
        });

        let outcome = await_turn(reply_rx, activity_rx, &signals.view(), TURN_IDLE_TIMEOUT).await;
        match outcome {
            TurnWait::Settled(Err(error)) => assert_eq!(error.message, "boom"),
            _ => panic!("an agent error must settle the turn"),
        }
    }

    /// A child that stays alive and never reads its stdin, so the pipe fills.
    fn sleeper() -> Command {
        if cfg!(windows) {
            let mut command = Command::new("powershell");
            command.args(["-NoProfile", "-Command", "Start-Sleep -Seconds 60"]);
            command
        } else {
            let mut command = Command::new("sleep");
            command.arg("60");
            command
        }
    }

    /// `stop` must return even while a write is blocked on a full pipe.
    ///
    /// The deadlock this catches: `stop_process` used to take the stdin lock
    /// before killing the child, and a write blocked on a full pipe holds that
    /// lock, so the kill that would break the pipe waited behind it forever.
    /// The deadlock lives in the pipe, so the child is a real process.
    #[tokio::test]
    async fn stop_returns_while_a_write_is_blocked_on_a_full_pipe() {
        let bridge = Arc::new(AcpBridge::with_sink(Arc::new(NullSink)));

        let mut command = sleeper();
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = command.spawn().expect("the sleeper must spawn");
        let stdin = child.stdin.take().expect("stdin was piped");
        *bridge.inner.stdin.lock().await = Some(stdin);
        *bridge.inner.child.lock().await = Some(child);

        // Each chunk is far larger than the pipe buffer, so one write blocks
        // mid-`write_all` with the stdin lock held — the sleeper never reads.
        let writer = Arc::clone(&bridge);
        let blocked = tokio::spawn(async move {
            let chunk = json!({ "pad": "x".repeat(1024 * 1024) });
            while writer.inner.write_value(chunk.clone()).await.is_ok() {}
        });
        // Give the writer time to fill the pipe and block inside the write.
        tokio::time::sleep(Duration::from_millis(500)).await;

        tokio::time::timeout(Duration::from_secs(2), bridge.stop())
            .await
            .expect("stop must kill the child, not wait behind the blocked write")
            .expect("stop must succeed once the child is dead");
        blocked.abort();
    }
}
