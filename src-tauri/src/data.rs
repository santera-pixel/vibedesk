//! The data plane: where a panel's bound values come from.
//!
//! A panel declares `sources`, and a binding reads a path into the data model
//! those sources produce. Two families, and the split is the security boundary:
//!
//! * **Built-in** — VibeDesk's own state. No network, no approval friction.
//!   Resolved in the frontend, because that is where the state already lives;
//!   routing it through IPC would be a round trip and a second copy of the
//!   truth. See `src/lib/data.ts`.
//! * **External** — a loopback HTTP endpoint, polled or streamed. Fetched
//!   **here**, in Rust, because the webview's CSP is `connect-src 'self' ipc:`
//!   and it has no network reach at all. Loosening that would hand every
//!   agent-authored panel the ability to reach any host the machine can reach.
//!   Fetching in Rust keeps the webview's capability unchanged and confines the
//!   new capability to code the user approved.
//!
//! # The load-bearing rule
//!
//! A `panels.json` source may only address loopback. Non-loopback is not
//! expressible in the file at all — it would need a `settings.json` entry the
//! user edits by hand. The agent may propose a panel; it may not propose a wider
//! network capability. Everything else in this module is defence in depth
//! behind that one property.
//!
//! # Backpressure
//!
//! Coalesce, never queue. A tick that fires while a fetch is in flight is
//! skipped: the poll loop sleeps *after* the fetch completes, so a slow endpoint
//! naturally lowers its own rate, and a global semaphore is taken with
//! `try_acquire` so a burst of sources cannot pile up behind four permits. The
//! previous value stands until a new one arrives, which is the correct thing for
//! a panel to show.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter};

/// Emitted once per subscriber per tick.
pub const EVENT_DATA: &str = "panel://data";

/// Most sources one panel may declare. A panel is a sidebar, not a dashboard.
pub const MAX_SOURCES_PER_PANEL: usize = 8;

/// Largest response body accepted, in bytes. Reading stops at the cap rather
/// than buffering and then checking, so a hostile endpoint cannot make the app
/// allocate before it is refused.
pub const MAX_BODY_BYTES: usize = 256 * 1024;

/// Bounds on a parsed response. A 256 KiB body can still be a million nodes.
pub const MAX_JSON_DEPTH: usize = 32;
pub const MAX_JSON_NODES: usize = 10_000;

/// Fetches allowed in flight across every source. A tick that cannot get a
/// permit is skipped, not queued.
pub const MAX_CONCURRENT: usize = 4;

/// Inclusive bounds for `intervalMs` and `timeoutMs`.
pub const INTERVAL_MS: (u64, u64) = (1_000, 60_000);
pub const TIMEOUT_MS: (u64, u64) = (500, 15_000);

pub const MAX_URL: usize = 2048;

/// How long to wait before reconnecting a dropped SSE stream.
const SSE_RECONNECT_MS: u64 = 1_000;

fn default_interval() -> u64 {
    2_000
}

fn default_timeout() -> u64 {
    5_000
}

/// One declared input.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Source {
    /// The path root. `/ci/queue/depth` reads `data.ci.queue.depth`.
    pub id: String,
    #[serde(flatten)]
    pub kind: SourceKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum SourceKind {
    /// VibeDesk's own state. `name` defaults to the id.
    Builtin {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<BuiltinName>,
    },
    /// A loopback HTTP endpoint.
    ///
    /// `rename_all` on the enum renames the *variants*, not the fields, so the
    /// fields need their own. Without it `intervalMs` is silently ignored and
    /// the default applies — a panel that polls at the wrong rate with no error.
    #[serde(rename_all = "camelCase")]
    Http {
        url: String,
        #[serde(default)]
        mode: HttpMode,
        #[serde(default = "default_interval")]
        interval_ms: u64,
        #[serde(default = "default_timeout")]
        timeout_ms: u64,
    },
}

/// The closed set of built-in feeds. A panel cannot invent a seventh.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BuiltinName {
    Status,
    Usage,
    Session,
    Chats,
    Plugins,
    Clock,
}

impl BuiltinName {
    pub const ALL: [&'static str; 6] = ["status", "usage", "session", "chats", "plugins", "clock"];

    pub fn parse(token: &str) -> Option<Self> {
        Some(match token {
            "status" => BuiltinName::Status,
            "usage" => BuiltinName::Usage,
            "session" => BuiltinName::Session,
            "chats" => BuiltinName::Chats,
            "plugins" => BuiltinName::Plugins,
            "clock" => BuiltinName::Clock,
            _ => return None,
        })
    }

    pub fn token(self) -> &'static str {
        match self {
            BuiltinName::Status => "status",
            BuiltinName::Usage => "usage",
            BuiltinName::Session => "session",
            BuiltinName::Chats => "chats",
            BuiltinName::Plugins => "plugins",
            BuiltinName::Clock => "clock",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HttpMode {
    /// Fetch on an interval.
    #[default]
    Poll,
    /// Hold the connection open and read `text/event-stream` frames.
    Sse,
}

impl HttpMode {
    pub const ALL: [&'static str; 2] = ["poll", "sse"];
}

impl Source {
    /// The built-in this source resolves to, if it is one.
    pub fn builtin(&self) -> Option<BuiltinName> {
        match &self.kind {
            SourceKind::Builtin { name } => name.or_else(|| BuiltinName::parse(&self.id)),
            SourceKind::Http { .. } => None,
        }
    }

    /// The fetch spec, if this source needs the network.
    pub fn http(&self) -> Option<HttpSpec> {
        match &self.kind {
            SourceKind::Http {
                url,
                mode,
                interval_ms,
                timeout_ms,
            } => Some(HttpSpec {
                url: url.clone(),
                mode: *mode,
                interval_ms: *interval_ms,
                timeout_ms: *timeout_ms,
            }),
            SourceKind::Builtin { .. } => None,
        }
    }
}

/// What to fetch, and how often. Two panels declaring the same spec share one
/// task — that is what coalescing means here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpSpec {
    pub url: String,
    pub mode: HttpMode,
    pub interval_ms: u64,
    pub timeout_ms: u64,
}

impl HttpSpec {
    fn key(&self) -> String {
        format!(
            "{}|{}|{}|{}",
            self.url,
            match self.mode {
                HttpMode::Poll => "poll",
                HttpMode::Sse => "sse",
            },
            self.interval_ms,
            self.timeout_ms
        )
    }

    /// The origin this spec reads, which is the unit consent is granted in.
    ///
    /// Falls back to the URL when it cannot be parsed. That cannot happen for a
    /// spec that passed `check_source`, and the fallback is deliberately a value
    /// no ledger entry will ever match — so an unparseable spec is refused
    /// rather than allowed.
    fn origin(&self) -> String {
        crate::consent::origin_of(&self.url).unwrap_or_else(|_| self.url.clone())
    }
}

/* ------------------------------------------------------------------ *
 * Validation
 * ------------------------------------------------------------------ */

pub fn check_source(source: &Source) -> Result<(), String> {
    if !is_id(&source.id) {
        return Err(format!(
            "source id `{}` must be 1-32 lowercase letters, digits or dashes",
            source.id
        ));
    }

    match &source.kind {
        SourceKind::Builtin { name } => {
            if name.is_none() && BuiltinName::parse(&source.id).is_none() {
                return Err(format!(
                    "builtin source `{}` must be one of {} — or set `name` to one of them",
                    source.id,
                    BuiltinName::ALL.join(", ")
                ));
            }
            Ok(())
        }
        SourceKind::Http {
            url,
            interval_ms,
            timeout_ms,
            ..
        } => {
            check_loopback_url(url)?;
            if *interval_ms < INTERVAL_MS.0 || *interval_ms > INTERVAL_MS.1 {
                return Err(format!(
                    "intervalMs {interval_ms} is outside {}..{}",
                    INTERVAL_MS.0, INTERVAL_MS.1
                ));
            }
            if *timeout_ms < TIMEOUT_MS.0 || *timeout_ms > TIMEOUT_MS.1 {
                return Err(format!(
                    "timeoutMs {timeout_ms} is outside {}..{}",
                    TIMEOUT_MS.0, TIMEOUT_MS.1
                ));
            }
            Ok(())
        }
    }
}

/// The one rule that matters: a source may only address this machine.
///
/// `https` is refused rather than allowed, because loopback TLS needs a
/// certificate the app cannot verify, so accepting it would mean either
/// disabling verification or shipping a CA — both worse than plain `http` on an
/// interface that never leaves the host.
pub fn check_loopback_url(url: &str) -> Result<(), String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err("url is empty".into());
    }
    if trimmed.len() > MAX_URL {
        return Err(format!("url is longer than {MAX_URL} characters"));
    }
    if trimmed.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("url contains whitespace or control characters".into());
    }
    if trimmed.contains('#') {
        return Err("url must not have a fragment".into());
    }

    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("https://") {
        return Err(
            "a source url must use http://, not https:// — loopback TLS needs a certificate the app cannot verify"
                .into(),
        );
    }
    if !lower.starts_with("http://") {
        return Err("a source url must start with http://".into());
    }

    let rest = &trimmed["http://".len()..];
    let authority = rest.split(['/', '?']).next().unwrap_or("");
    if authority.is_empty() {
        return Err("url has no host".into());
    }
    if authority.contains('@') {
        return Err("url must not embed credentials".into());
    }

    let host = if let Some(stripped) = authority.strip_prefix('[') {
        stripped.split(']').next().unwrap_or("")
    } else {
        authority.split(':').next().unwrap_or("")
    };
    let host = host.to_ascii_lowercase();

    if !matches!(host.as_str(), "127.0.0.1" | "::1" | "localhost") {
        return Err(format!(
            "source host `{host}` is not loopback; a panel may only read 127.0.0.1, ::1 or localhost"
        ));
    }
    Ok(())
}

/// Bounds on a parsed response, walked iteratively so a deep body cannot blow
/// the stack while being measured.
pub fn check_json(value: &Value) -> Result<(), String> {
    let mut stack: Vec<(&Value, usize)> = vec![(value, 1)];
    let mut nodes = 0usize;
    while let Some((node, depth)) = stack.pop() {
        nodes += 1;
        if nodes > MAX_JSON_NODES {
            return Err(format!(
                "response has more than {MAX_JSON_NODES} values"
            ));
        }
        if depth > MAX_JSON_DEPTH {
            return Err(format!(
                "response nests deeper than {MAX_JSON_DEPTH} levels"
            ));
        }
        match node {
            Value::Array(items) => {
                for item in items {
                    stack.push((item, depth + 1));
                }
            }
            Value::Object(map) => {
                for item in map.values() {
                    stack.push((item, depth + 1));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn is_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 32
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/* ------------------------------------------------------------------ *
 * Events
 * ------------------------------------------------------------------ */

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DataEvent {
    pub panel_id: String,
    pub source_id: String,
    /// `None` when the fetch failed; the previous value stands in the store.
    pub value: Option<Value>,
    pub at: String,
    pub error: Option<String>,
}

/// What the Status panel lists. Every active source, where it points, and
/// whether it is running — so "why is this panel empty" has an answer.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceStatus {
    pub panel_id: String,
    pub source_id: String,
    pub kind: String,
    pub target: String,
    /// The origin consent is granted for. `target` is the URL; this is the unit
    /// the user actually decides about.
    pub origin: String,
    pub mode: Option<String>,
    pub interval_ms: Option<u64>,
    pub running: bool,
    /// `running`, `stopped`, `awaiting-consent` or `denied`. `running` is the
    /// boolean; this is the reason, which is what the UI needs to say something
    /// useful.
    pub state: String,
}

/* ------------------------------------------------------------------ *
 * The plane
 * ------------------------------------------------------------------ */

struct Running {
    handle: tauri::async_runtime::JoinHandle<()>,
    subscribers: Arc<Mutex<Vec<(String, String)>>>,
}

#[derive(Default)]
struct Inner {
    running: HashMap<String, Running>,
    specs: HashMap<String, HttpSpec>,
    /// `key -> [(panelId, sourceId)]`. Recorded whether or not the task runs, so
    /// the Status panel can still say what is declared while the plane is
    /// paused. "Nothing is running" and "nothing is declared" are different
    /// answers and the user has to be able to tell them apart.
    declared: HashMap<String, Vec<(String, String)>>,
    /// Origins that are declared but not allowed to run, keyed by origin.
    ///
    /// Kept apart from `specs` because these never get a task: the whole point
    /// of consent is that an unapproved origin is never contacted. They are
    /// still listed so the user can see what is asking and decide.
    pending: BTreeMap<String, PendingOrigin>,
    /// Set by the kill switch. While paused, `reconcile` starts nothing.
    paused: bool,
}

/// One origin that is waiting on a decision, with everything that wants it.
struct PendingOrigin {
    /// `None` means the user has not been asked; `Some(Deny)` means refused.
    decision: Option<crate::consent::Decision>,
    /// One entry per distinct spec on this origin, with its subscribers.
    specs: Vec<(HttpSpec, Vec<(String, String)>)>,
}

pub struct DataPlane {
    inner: Mutex<Inner>,
    permits: Arc<tokio::sync::Semaphore>,
}

/// What the panels declare, in the shape the plane runs it.
///
/// Two panels pointing at the same URL share one task, so the spec map is keyed
/// by URL and the subscriber map records every `(panelId, sourceId)` that reads
/// it. Pure, so the sharing rule is testable without an `AppHandle`.
fn declared_sets(
    panels: &[crate::panels::CustomPanel],
) -> (
    HashMap<String, HttpSpec>,
    HashMap<String, Vec<(String, String)>>,
) {
    let mut specs: HashMap<String, HttpSpec> = HashMap::new();
    let mut wanted: HashMap<(String, String), String> = HashMap::new();

    for panel in panels {
        // A widget may declare its own sources. They run under the panel's id,
        // so the model the widget receives already contains them — which is the
        // point: one edit, not two.
        let widget_sources = panel.blocks.iter().filter_map(|block| {
            match &block.body {
                crate::panels::PanelBlock::Widget { sources, .. } => Some(sources),
                _ => None,
            }
        });
        for source in panel.sources.iter().chain(widget_sources.flatten()) {
            let Some(spec) = source.http() else { continue };
            let key = spec.key();
            wanted.insert((panel.id.clone(), source.id.clone()), key.clone());
            specs.insert(key, spec);
        }
    }

    let mut declared: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for key in specs.keys() {
        let mut subscribers: Vec<(String, String)> = wanted
            .iter()
            .filter(|(_, wanted_key)| *wanted_key == key)
            .map(|(pair, _)| pair.clone())
            .collect();
        // HashMap order is not stable, and a status list that reshuffles on
        // every read is unreadable.
        subscribers.sort();
        declared.insert(key.clone(), subscribers);
    }

    (specs, declared)
}

/// Split what the panels declare into what may run and what is waiting on the
/// user.
///
/// This is the gate. A spec whose origin is not explicitly allowed lands in
/// `pending` and never gets a task, so an unapproved origin is never contacted —
/// not once, not to "check if it is up". Pure, so the rule is testable without
/// an `AppHandle` and without a network.
fn split_by_consent(
    specs: HashMap<String, HttpSpec>,
    declared: &HashMap<String, Vec<(String, String)>>,
    ledger: &crate::consent::Ledger,
) -> (
    HashMap<String, HttpSpec>,
    BTreeMap<String, PendingOrigin>,
) {
    let mut allowed: HashMap<String, HttpSpec> = HashMap::new();
    let mut pending: BTreeMap<String, PendingOrigin> = BTreeMap::new();

    for (key, spec) in specs {
        let origin = spec.origin();
        let decision = ledger.decision(&origin);
        if ledger.allows(&origin) {
            allowed.insert(key, spec);
            continue;
        }
        let subscribers = declared.get(&key).cloned().unwrap_or_default();
        let entry = pending.entry(origin).or_insert_with(|| PendingOrigin {
            decision,
            specs: Vec::new(),
        });
        entry.specs.push((spec, subscribers));
    }

    (allowed, pending)
}

impl DataPlane {
    pub fn new() -> Self {
        DataPlane {
            inner: Mutex::new(Inner::default()),
            permits: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT)),
        }
    }

    /// Bring the running tasks in line with what the panels declare.
    ///
    /// Idempotent, and safe to call on every config change: a spec that is
    /// already running only has its subscriber set updated, so a panel edit does
    /// not restart a stream that is working.
    pub fn reconcile(&self, app: &AppHandle, panels: &[crate::panels::CustomPanel]) {
        let (declared_specs, declared) = declared_sets(panels);
        let (specs, pending) = split_by_consent(declared_specs, &declared, &crate::consent::load());

        let mut inner = self.inner.lock().unwrap();

        // Stop what nobody wants any more, before the declared set is replaced.
        let stale: Vec<String> = inner
            .running
            .keys()
            .filter(|key| !specs.contains_key(*key))
            .cloned()
            .collect();
        for key in stale {
            if let Some(running) = inner.running.remove(&key) {
                running.handle.abort();
            }
        }

        inner.specs = specs;
        inner.declared = declared;
        inner.pending = pending;

        // Safe mode and the kill switch both mean "declare it, run nothing".
        if crate::customize::safe_mode() || inner.paused {
            for (_, running) in inner.running.drain() {
                running.handle.abort();
            }
            return;
        }

        // Taken out of `inner` so the loop can hold `running` mutably.
        let specs = std::mem::take(&mut inner.specs);
        let declared = std::mem::take(&mut inner.declared);

        let mut to_start: Vec<(String, HttpSpec, Vec<(String, String)>)> = Vec::new();
        for (key, spec) in &specs {
            let subscribers = declared.get(key).cloned().unwrap_or_default();
            match inner.running.get_mut(key) {
                Some(running) => {
                    *running.subscribers.lock().unwrap() = subscribers;
                }
                None => to_start.push((key.clone(), spec.clone(), subscribers)),
            }
        }

        for (key, spec, subscribers) in to_start {
            let subscribers = Arc::new(Mutex::new(subscribers));
            let handle = spawn_task(
                app.clone(),
                spec.clone(),
                subscribers.clone(),
                self.permits.clone(),
            );
            inner.running.insert(
                key,
                Running {
                    handle,
                    subscribers,
                },
            );
        }

        inner.specs = specs;
        inner.declared = declared;
    }

    /// Abort every task. The declared set is kept: stopping is not forgetting.
    pub fn stop_all(&self) {
        let mut inner = self.inner.lock().unwrap();
        for (_, running) in inner.running.drain() {
            running.handle.abort();
        }
    }

    /// The kill switch. Pausing stops every task and refuses to start new ones
    /// until it is lifted, so a panel cannot quietly resume after the user has
    /// said stop.
    pub fn set_paused(&self, app: &AppHandle, paused: bool, panels: &[crate::panels::CustomPanel]) {
        {
            let mut inner = self.inner.lock().unwrap();
            inner.paused = paused;
        }
        if paused {
            self.stop_all();
        } else {
            self.reconcile(app, panels);
        }
    }

    pub fn paused(&self) -> bool {
        self.inner.lock().unwrap().paused
    }

    pub fn status(&self) -> Vec<SourceStatus> {
        let inner = self.inner.lock().unwrap();
        let mut out: Vec<SourceStatus> = Vec::new();

        for (key, spec) in &inner.specs {
            let running = inner.running.contains_key(key);
            let subscribers = inner.declared.get(key).cloned().unwrap_or_default();
            for (panel_id, source_id) in subscribers {
                out.push(SourceStatus {
                    panel_id,
                    source_id,
                    kind: "http".into(),
                    target: spec.url.clone(),
                    origin: spec.origin(),
                    mode: Some(mode_token(spec.mode).into()),
                    interval_ms: Some(spec.interval_ms),
                    running,
                    state: if running { "running" } else { "stopped" }.into(),
                });
            }
        }

        // A source waiting on the user is not running and never will be until
        // they say so. Listing it is what makes the prompt discoverable.
        for (origin, pending) in &inner.pending {
            let state = match pending.decision {
                None => "awaiting-consent",
                Some(crate::consent::Decision::Deny) => "denied",
                // An allow that has not been reconciled yet; the next pass
                // starts it. Reporting it as running would be a lie.
                Some(crate::consent::Decision::Allow) => "stopped",
            };
            for (spec, subscribers) in &pending.specs {
                for (panel_id, source_id) in subscribers {
                    out.push(SourceStatus {
                        panel_id: panel_id.clone(),
                        source_id: source_id.clone(),
                        kind: "http".into(),
                        target: spec.url.clone(),
                        origin: origin.clone(),
                        mode: Some(mode_token(spec.mode).into()),
                        interval_ms: Some(spec.interval_ms),
                        running: false,
                        state: state.into(),
                    });
                }
            }
        }

        out.sort_by(|a, b| {
            a.panel_id
                .cmp(&b.panel_id)
                .then_with(|| a.source_id.cmp(&b.source_id))
        });
        out
    }
}

fn mode_token(mode: HttpMode) -> &'static str {
    match mode {
        HttpMode::Poll => "poll",
        HttpMode::Sse => "sse",
    }
}

fn spawn_task(
    app: AppHandle,
    spec: HttpSpec,
    subscribers: Arc<Mutex<Vec<(String, String)>>>,
    permits: Arc<tokio::sync::Semaphore>,
) -> tauri::async_runtime::JoinHandle<()> {
    tauri::async_runtime::spawn(async move {
        match spec.mode {
            HttpMode::Poll => poll_loop(app, spec, subscribers, permits).await,
            HttpMode::Sse => sse_loop(app, spec, subscribers, permits).await,
        }
    })
}

fn client(spec: &HttpSpec) -> Result<reqwest::Client, String> {
    let builder = reqwest::Client::builder()
        // A redirect is how a loopback service bounces a request to the
        // internet. Refusing them outright is simpler and stricter than
        // checking where they landed.
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("VibeDesk/", env!("CARGO_PKG_VERSION")));

    // `timeout` is a deadline for the whole request, body included. That is
    // right for a poll and wrong for a stream: an event-stream is meant to stay
    // open, so a total deadline would cut it every `timeoutMs` and reconnect,
    // dropping whatever was sent in the gap. A stream gets a connect deadline
    // instead, and the frame cap plus the reconnect loop bound it from the
    // other side.
    let builder = match spec.mode {
        HttpMode::Poll => builder.timeout(Duration::from_millis(spec.timeout_ms)),
        HttpMode::Sse => builder.connect_timeout(Duration::from_millis(spec.timeout_ms)),
    };

    builder
        .build()
        .map_err(|e| format!("could not build an HTTP client: {e}"))
}

async fn poll_loop(
    app: AppHandle,
    spec: HttpSpec,
    subscribers: Arc<Mutex<Vec<(String, String)>>>,
    permits: Arc<tokio::sync::Semaphore>,
) {
    let client = match client(&spec) {
        Ok(client) => client,
        Err(error) => {
            emit(&app, &subscribers, Err(error));
            return;
        }
    };

    loop {
        // Skip, never queue. If every permit is taken, this tick is dropped and
        // the previous value stands.
        let permit = permits.clone().try_acquire_owned();
        match permit {
            Ok(_permit) => {
                let started = Instant::now();
                let outcome = fetch_once(&client, &spec).await;
                emit(&app, &subscribers, outcome);
                // Sleep the remainder of the interval, so a slow endpoint
                // lowers its own rate instead of accumulating a backlog.
                let wait = Duration::from_millis(spec.interval_ms).saturating_sub(started.elapsed());
                tokio::time::sleep(wait).await;
            }
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(spec.interval_ms)).await;
            }
        }
    }
}

async fn fetch_once(client: &reqwest::Client, spec: &HttpSpec) -> Result<Value, String> {
    let response = client
        .get(&spec.url)
        .send()
        .await
        .map_err(|e| format!("could not reach {}: {e}", spec.url))?;

    if !response.status().is_success() {
        return Err(format!("{} returned HTTP {}", spec.url, response.status()));
    }

    // Defence in depth: redirects are already refused, but if a future change
    // relaxes that, this still catches a hop off loopback.
    if let Err(reason) = check_loopback_url(response.url().as_str()) {
        return Err(format!("response came from off loopback: {reason}"));
    }

    let body = read_capped(response).await?;
    let value: Value = serde_json::from_slice(&body)
        .map_err(|e| format!("{} did not return JSON: {e}", spec.url))?;
    check_json(&value)?;
    Ok(value)
}

/// Read at most `MAX_BODY_BYTES`, stopping as soon as the cap is crossed.
async fn read_capped(mut response: reqwest::Response) -> Result<Vec<u8>, String> {
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| format!("could not read the response: {e}"))?
    {
        if body.len() + chunk.len() > MAX_BODY_BYTES {
            return Err(format!(
                "response is larger than {MAX_BODY_BYTES} bytes"
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

async fn sse_loop(
    app: AppHandle,
    spec: HttpSpec,
    subscribers: Arc<Mutex<Vec<(String, String)>>>,
    permits: Arc<tokio::sync::Semaphore>,
) {
    let client = match client(&spec) {
        Ok(client) => client,
        Err(error) => {
            emit(&app, &subscribers, Err(error));
            return;
        }
    };

    loop {
        let permit = permits.clone().try_acquire_owned();
        match permit {
            Ok(_permit) => {
                if let Err(error) = sse_once(&client, &spec, &app, &subscribers).await {
                    emit(&app, &subscribers, Err(error));
                }
            }
            Err(_) => {}
        }
        tokio::time::sleep(Duration::from_millis(SSE_RECONNECT_MS)).await;
    }
}

async fn sse_once(
    client: &reqwest::Client,
    spec: &HttpSpec,
    app: &AppHandle,
    subscribers: &Arc<Mutex<Vec<(String, String)>>>,
) -> Result<(), String> {
    let mut response = client
        .get(&spec.url)
        .header("accept", "text/event-stream")
        .send()
        .await
        .map_err(|e| format!("could not reach {}: {e}", spec.url))?;

    if !response.status().is_success() {
        return Err(format!("{} returned HTTP {}", spec.url, response.status()));
    }
    if let Err(reason) = check_loopback_url(response.url().as_str()) {
        return Err(format!("stream came from off loopback: {reason}"));
    }

    let mut buffer = String::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| format!("the stream ended: {e}"))?
    {
        buffer.push_str(&String::from_utf8_lossy(&chunk));

        // A frame larger than the cap is a broken producer, not a big value.
        // Drop what we have and say so once, rather than growing without bound.
        if buffer.len() > MAX_BODY_BYTES {
            buffer.clear();
            emit(
                app,
                subscribers,
                Err(format!(
                    "an event-stream frame was larger than {MAX_BODY_BYTES} bytes and was dropped"
                )),
            );
            continue;
        }

        while let Some(index) = buffer.find("\n\n") {
            let frame: String = buffer.drain(..index + 2).collect();
            let Some(data) = sse_data(&frame) else { continue };
            let Ok(value) = serde_json::from_str::<Value>(&data) else {
                continue;
            };
            if check_json(&value).is_ok() {
                emit(app, subscribers, Ok(value));
            }
        }
    }
    Ok(())
}

/// The `data:` payload of one SSE frame, with multi-line data joined by newline
/// as the format specifies.
fn sse_data(frame: &str) -> Option<String> {
    let mut lines: Vec<&str> = Vec::new();
    for line in frame.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(rest) = line.strip_prefix("data:") {
            lines.push(rest.strip_prefix(' ').unwrap_or(rest));
        }
    }
    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    }
}

fn emit(
    app: &AppHandle,
    subscribers: &Arc<Mutex<Vec<(String, String)>>>,
    outcome: Result<Value, String>,
) {
    let at = chrono::Utc::now().to_rfc3339();
    let subscribers = subscribers.lock().unwrap().clone();
    for (panel_id, source_id) in subscribers {
        let event = match &outcome {
            Ok(value) => DataEvent {
                panel_id: panel_id.clone(),
                source_id: source_id.clone(),
                value: Some(value.clone()),
                at: at.clone(),
                error: None,
            },
            Err(error) => DataEvent {
                panel_id: panel_id.clone(),
                source_id: source_id.clone(),
                value: None,
                at: at.clone(),
                error: Some(error.clone()),
            },
        };
        let _ = app.emit(EVENT_DATA, event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn http(url: &str) -> Source {
        Source {
            id: "ci".into(),
            kind: SourceKind::Http {
                url: url.into(),
                mode: HttpMode::Poll,
                interval_ms: 2_000,
                timeout_ms: 5_000,
            },
        }
    }

    fn builtin(id: &str, name: Option<BuiltinName>) -> Source {
        Source {
            id: id.into(),
            kind: SourceKind::Builtin { name },
        }
    }

    // ---- the loopback rule ------------------------------------------------

    #[test]
    fn every_loopback_spelling_is_accepted() {
        for url in [
            "http://127.0.0.1:8787/status",
            "http://127.0.0.1/",
            "http://localhost:3000/api/v1/state",
            "http://LOCALHOST/status",
            "http://[::1]:8787/status",
            "http://127.0.0.1:8787/status?since=1",
        ] {
            check_loopback_url(url).unwrap_or_else(|e| panic!("{url}: {e}"));
        }
    }

    #[test]
    fn a_non_loopback_host_is_refused() {
        for url in [
            "http://example.com/status",
            "http://10.0.0.1/status",
            "http://192.168.1.1/status",
            "http://0.0.0.0/status",
            "http://[::]/status",
            // The lookalikes, which a substring check would have let through.
            "http://127.0.0.1.evil.com/status",
            "http://localhost.evil.com/status",
            "http://notlocalhost/status",
        ] {
            let error = check_loopback_url(url).unwrap_err();
            assert!(error.contains("not loopback"), "{url}: {error}");
        }
    }

    #[test]
    fn https_is_refused_with_a_reason_the_agent_can_act_on() {
        let error = check_loopback_url("https://127.0.0.1:8787/status").unwrap_err();
        assert!(error.contains("http://"), "{error}");
        assert!(error.contains("certificate"), "{error}");
    }

    #[test]
    fn a_url_may_not_carry_credentials() {
        let error = check_loopback_url("http://user:pass@127.0.0.1/status").unwrap_err();
        assert!(error.contains("credentials"), "{error}");
    }

    #[test]
    fn a_url_may_not_carry_a_fragment() {
        let error = check_loopback_url("http://127.0.0.1/status#x").unwrap_err();
        assert!(error.contains("fragment"), "{error}");
    }

    #[test]
    fn a_url_may_not_carry_whitespace_or_a_control_character() {
        assert!(check_loopback_url("http://127.0.0.1/a b").is_err());
        assert!(check_loopback_url("http://127.0.0.1/a\nb").is_err());
    }

    #[test]
    fn a_url_must_have_a_scheme_and_a_host() {
        assert!(check_loopback_url("127.0.0.1/status").is_err());
        assert!(check_loopback_url("http:///status").is_err());
        assert!(check_loopback_url("").is_err());
    }

    #[test]
    fn a_url_may_not_be_absurdly_long() {
        let long = format!("http://127.0.0.1/{}", "a".repeat(MAX_URL));
        let error = check_loopback_url(&long).unwrap_err();
        assert!(error.contains("longer than"), "{error}");
    }

    // ---- source declarations ---------------------------------------------

    #[test]
    fn a_valid_http_source_passes() {
        check_source(&http("http://127.0.0.1:8787/status")).unwrap();
    }

    #[test]
    fn an_interval_below_the_floor_is_refused() {
        let mut source = http("http://127.0.0.1:8787/status");
        if let SourceKind::Http { interval_ms, .. } = &mut source.kind {
            *interval_ms = 999;
        }
        let error = check_source(&source).unwrap_err();
        assert!(error.contains("intervalMs"), "{error}");
    }

    #[test]
    fn an_interval_above_the_ceiling_is_refused() {
        let mut source = http("http://127.0.0.1:8787/status");
        if let SourceKind::Http { interval_ms, .. } = &mut source.kind {
            *interval_ms = 60_001;
        }
        assert!(check_source(&source).is_err());
    }

    #[test]
    fn a_timeout_outside_the_range_is_refused() {
        for value in [499, 15_001] {
            let mut source = http("http://127.0.0.1:8787/status");
            if let SourceKind::Http { timeout_ms, .. } = &mut source.kind {
                *timeout_ms = value;
            }
            let error = check_source(&source).unwrap_err();
            assert!(error.contains("timeoutMs"), "{value}: {error}");
        }
    }

    #[test]
    fn a_builtin_source_must_name_a_known_feed() {
        let error = check_source(&builtin("nope", None)).unwrap_err();
        assert!(error.contains("must be one of"), "{error}");
        for name in BuiltinName::ALL {
            assert!(error.contains(name), "{name} is missing from the message");
        }
    }

    #[test]
    fn a_builtin_source_may_take_its_name_from_its_id() {
        for name in BuiltinName::ALL {
            let source = builtin(name, None);
            check_source(&source).unwrap();
            assert_eq!(source.builtin().map(BuiltinName::token), Some(name));
        }
    }

    #[test]
    fn a_builtin_source_may_alias_a_feed() {
        let source = builtin("now", Some(BuiltinName::Clock));
        check_source(&source).unwrap();
        assert_eq!(source.builtin(), Some(BuiltinName::Clock));
    }

    #[test]
    fn a_source_id_must_be_an_id() {
        for bad in ["", "CI", "a b", "a_b", "a.b"] {
            let mut source = http("http://127.0.0.1:8787/status");
            source.id = bad.into();
            assert!(check_source(&source).is_err(), "`{bad}` was accepted");
        }
    }

    #[test]
    fn a_source_round_trips_through_json() {
        let raw = json!({
            "id": "ci",
            "type": "http",
            "url": "http://127.0.0.1:8787/status",
            "mode": "sse",
            "intervalMs": 3000,
            "timeoutMs": 4000
        });
        let source: Source = serde_json::from_value(raw.clone()).unwrap();
        check_source(&source).unwrap();
        assert_eq!(serde_json::to_value(&source).unwrap(), raw);

        let builtin_raw = json!({ "id": "status", "type": "builtin" });
        let source: Source = serde_json::from_value(builtin_raw.clone()).unwrap();
        check_source(&source).unwrap();
        assert_eq!(serde_json::to_value(&source).unwrap(), builtin_raw);
    }

    #[test]
    fn an_http_source_defaults_its_interval_and_timeout() {
        let source: Source =
            serde_json::from_value(json!({ "id": "ci", "type": "http", "url": "http://127.0.0.1/x" }))
                .unwrap();
        let spec = source.http().unwrap();
        assert_eq!(spec.interval_ms, 2_000);
        assert_eq!(spec.timeout_ms, 5_000);
        assert_eq!(spec.mode, HttpMode::Poll);
    }

    #[test]
    fn two_panels_declaring_the_same_spec_share_a_key() {
        let a = http("http://127.0.0.1:8787/status").http().unwrap();
        let b = http("http://127.0.0.1:8787/status").http().unwrap();
        assert_eq!(a.key(), b.key());

        let mut c = http("http://127.0.0.1:8787/status").http().unwrap();
        c.interval_ms = 3_000;
        assert_ne!(a.key(), c.key());
    }

    // ---- response caps ----------------------------------------------------

    #[test]
    fn a_normal_response_passes_the_caps() {
        check_json(&json!({ "queue": { "depth": 3 }, "runs": [1, 2, 3] })).unwrap();
    }

    #[test]
    fn a_deep_response_is_refused() {
        let mut value = json!(1);
        for _ in 0..=MAX_JSON_DEPTH {
            value = json!([value]);
        }
        let error = check_json(&value).unwrap_err();
        assert!(error.contains("nests deeper"), "{error}");
    }

    #[test]
    fn a_wide_response_is_refused() {
        let value = json!((0..MAX_JSON_NODES).collect::<Vec<_>>());
        let error = check_json(&value).unwrap_err();
        assert!(error.contains("more than"), "{error}");
    }

    // ---- SSE framing ------------------------------------------------------

    #[test]
    fn sse_data_reads_a_single_line_payload() {
        assert_eq!(
            sse_data("event: tick\ndata: {\"a\":1}\n\n").as_deref(),
            Some("{\"a\":1}")
        );
    }

    #[test]
    fn sse_data_joins_a_multi_line_payload() {
        assert_eq!(
            sse_data("data: {\"a\":\ndata: 1}\n\n").as_deref(),
            Some("{\"a\":\n1}")
        );
    }

    #[test]
    fn sse_data_ignores_comments_and_other_fields() {
        assert_eq!(sse_data(": keep-alive\n\n"), None);
        assert_eq!(sse_data("event: tick\nid: 7\n\n"), None);
    }

    #[test]
    fn sse_data_tolerates_crlf() {
        assert_eq!(
            sse_data("data: {\"a\":1}\r\n\r\n").as_deref(),
            Some("{\"a\":1}")
        );
    }

    // ---- a real loopback fetch -------------------------------------------

    /// Serve one canned HTTP response on a loopback port and return its URL.
    async fn serve_once(response: Vec<u8>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            if let Ok((mut socket, _)) = listener.accept().await {
                let mut buffer = [0u8; 2048];
                let _ = socket.read(&mut buffer).await;
                let _ = socket.write_all(&response).await;
            }
        });
        format!("http://{addr}/status")
    }

    fn response(status: &str, body: &str) -> Vec<u8> {
        format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }

    fn spec_for(url: &str) -> HttpSpec {
        HttpSpec {
            url: url.into(),
            mode: HttpMode::Poll,
            interval_ms: 2_000,
            timeout_ms: 5_000,
        }
    }

    #[tokio::test]
    async fn a_loopback_response_is_fetched_and_parsed() {
        let url = serve_once(response("200 OK", r#"{"queue":{"depth":3}}"#)).await;
        let client = client(&spec_for(&url)).unwrap();
        let value = fetch_once(&client, &spec_for(&url)).await.unwrap();
        assert_eq!(value["queue"]["depth"], 3);
    }

    #[tokio::test]
    async fn a_non_success_status_becomes_an_error_not_a_panic() {
        let url = serve_once(response("500 Internal Server Error", "{}")).await;
        let client = client(&spec_for(&url)).unwrap();
        let error = fetch_once(&client, &spec_for(&url)).await.unwrap_err();
        assert!(error.contains("HTTP 500"), "{error}");
    }

    #[tokio::test]
    async fn a_redirect_is_refused_rather_than_followed() {
        let url = serve_once(
            b"HTTP/1.1 302 Found\r\nlocation: http://example.com/\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                .to_vec(),
        )
        .await;
        let client = client(&spec_for(&url)).unwrap();
        let error = fetch_once(&client, &spec_for(&url)).await.unwrap_err();
        assert!(error.contains("HTTP 302"), "{error}");
    }

    #[tokio::test]
    async fn a_body_over_the_cap_is_refused() {
        let big = format!("{{\"pad\":\"{}\"}}", "x".repeat(MAX_BODY_BYTES));
        let url = serve_once(response("200 OK", &big)).await;
        let client = client(&spec_for(&url)).unwrap();
        let error = fetch_once(&client, &spec_for(&url)).await.unwrap_err();
        assert!(error.contains("larger than"), "{error}");
    }

    #[tokio::test]
    async fn a_non_json_body_is_refused() {
        let url = serve_once(response("200 OK", "not json at all")).await;
        let client = client(&spec_for(&url)).unwrap();
        let error = fetch_once(&client, &spec_for(&url)).await.unwrap_err();
        assert!(error.contains("did not return JSON"), "{error}");
    }

    #[tokio::test]
    async fn a_deep_body_is_refused_after_parsing() {
        let mut value = json!(1);
        for _ in 0..=MAX_JSON_DEPTH {
            value = json!([value]);
        }
        let url = serve_once(response("200 OK", &value.to_string())).await;
        let client = client(&spec_for(&url)).unwrap();
        let error = fetch_once(&client, &spec_for(&url)).await.unwrap_err();
        assert!(error.contains("nests deeper"), "{error}");
    }

    #[tokio::test]
    async fn an_unreachable_port_becomes_an_error_not_a_panic() {
        // Bind and drop, so the port is almost certainly closed.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let url = format!("http://{addr}/status");
        let client = client(&spec_for(&url)).unwrap();
        let error = fetch_once(&client, &spec_for(&url)).await.unwrap_err();
        assert!(error.contains("could not reach"), "{error}");
    }

    // ---- the declared set ------------------------------------------------

    fn panel(id: &str, sources: Vec<Source>) -> crate::panels::CustomPanel {
        crate::panels::CustomPanel {
            id: id.into(),
            title: id.into(),
            icon: None,
            width: 280,
            layout: None,
            blocks: Vec::new(),
            sources,
            plugin: None,
        }
    }

    #[test]
    fn two_panels_on_one_url_share_a_single_task() {
        let panels = vec![
            panel("a", vec![http("http://127.0.0.1:8787/status")]),
            panel("b", vec![http("http://127.0.0.1:8787/status")]),
        ];
        let (specs, declared) = declared_sets(&panels);
        assert_eq!(specs.len(), 1, "one URL is one task");
        let subscribers = declared.values().next().unwrap();
        assert_eq!(subscribers.len(), 2, "both panels read the same task");
    }

    #[test]
    fn a_builtin_source_declares_no_task() {
        let panels = vec![panel("a", vec![builtin("usage", Some(BuiltinName::Usage))])];
        let (specs, declared) = declared_sets(&panels);
        assert!(specs.is_empty(), "a builtin is resolved in the frontend");
        assert!(declared.is_empty());
    }

    #[test]
    fn the_declared_set_survives_stopping() {
        // The bug this guards: `status()` used to read the subscriber list off
        // the running task, so pausing emptied the list and the user could no
        // longer see what they had paused.
        let plane = DataPlane::new();
        let panels = vec![panel("a", vec![http("http://127.0.0.1:8787/status")])];
        let (specs, declared) = declared_sets(&panels);
        {
            let mut inner = plane.inner.lock().unwrap();
            inner.specs = specs;
            inner.declared = declared;
        }

        plane.stop_all();

        let status = plane.status();
        assert_eq!(status.len(), 1, "stopping is not forgetting");
        assert_eq!(status[0].panel_id, "a");
        assert_eq!(status[0].source_id, "ci");
        assert!(!status[0].running, "nothing is running after a stop");
        assert_eq!(status[0].target, "http://127.0.0.1:8787/status");
    }

    #[test]
    fn the_status_list_is_ordered() {
        let panels = vec![
            panel("b", vec![http("http://127.0.0.1:8787/two")]),
            panel("a", vec![http("http://127.0.0.1:8787/one")]),
        ];
        let plane = DataPlane::new();
        let (specs, declared) = declared_sets(&panels);
        {
            let mut inner = plane.inner.lock().unwrap();
            inner.specs = specs;
            inner.declared = declared;
        }
        let status = plane.status();
        let ids: Vec<&str> = status.iter().map(|s| s.panel_id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b"], "a status list must not reshuffle");
    }

    // ---- streaming -------------------------------------------------------

    /// Serve an event-stream that emits `count` frames, one every `gap_ms`, and
    /// keep the connection open for the whole run.
    async fn serve_stream(count: usize, gap_ms: u64) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            if let Ok((mut socket, _)) = listener.accept().await {
                let mut buffer = [0u8; 2048];
                let _ = socket.read(&mut buffer).await;
                let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: keep-alive\r\n\r\n";
                if socket.write_all(head.as_bytes()).await.is_err() {
                    return;
                }
                for index in 0..count {
                    let frame = format!("data: {{\"tick\":{index}}}\n\n");
                    if socket.write_all(frame.as_bytes()).await.is_err() {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(gap_ms)).await;
                }
                // Hold the connection open so a total deadline would be the only
                // thing that ends it.
                tokio::time::sleep(Duration::from_millis(2_000)).await;
            }
        });
        format!("http://{addr}/events")
    }

    #[tokio::test]
    async fn a_stream_outlives_its_timeout() {
        // The bug this guards: `Client::timeout` is a deadline for the whole
        // request, so a stream was cut every `timeoutMs` and reconnected,
        // dropping whatever was sent in the gap. Four frames 200ms apart is
        // 800ms of stream against a 500ms timeout.
        let url = serve_stream(4, 200).await;
        let spec = HttpSpec {
            url: url.clone(),
            mode: HttpMode::Sse,
            interval_ms: 1_000,
            timeout_ms: 500,
        };
        let client = client(&spec).unwrap();

        let mut response = client
            .get(&url)
            .header("accept", "text/event-stream")
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success());

        let mut buffer = String::new();
        let mut ticks: Vec<i64> = Vec::new();
        while let Some(chunk) = response.chunk().await.unwrap() {
            buffer.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(index) = buffer.find("\n\n") {
                let frame: String = buffer.drain(..index + 2).collect();
                if let Some(data) = sse_data(&frame) {
                    let value: serde_json::Value = serde_json::from_str(&data).unwrap();
                    ticks.push(value["tick"].as_i64().unwrap());
                }
            }
            if ticks.len() == 4 {
                break;
            }
        }

        assert_eq!(ticks, vec![0, 1, 2, 3], "the stream must not be cut short");
    }

    #[tokio::test]
    async fn a_poll_still_has_a_total_deadline() {
        // The other half of the same decision: a poll that never answers must
        // still fail rather than hang a permit forever.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            if let Ok((mut socket, _)) = listener.accept().await {
                let mut buffer = [0u8; 2048];
                let _ = socket.read(&mut buffer).await;
                tokio::time::sleep(Duration::from_millis(3_000)).await;
            }
        });
        let url = format!("http://{addr}/slow");
        let spec = HttpSpec {
            url: url.clone(),
            mode: HttpMode::Poll,
            interval_ms: 1_000,
            timeout_ms: 500,
        };
        let client = client(&spec).unwrap();
        let error = fetch_once(&client, &spec).await.unwrap_err();
        assert!(error.contains("could not reach"), "{error}");
    }
}
