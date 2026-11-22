//! The loopback proxy: where compaction actually happens.
//!
//! VibeDesk is an ACP client, and the agent owns its own context. There is no
//! ACP method that says "compress your history", so a client-side compaction
//! feature has exactly one honest implementation: stand between the agent and
//! the provider, and rewrite the request. That is this module.
//!
//! The shape of a turn:
//!
//! ```text
//! agent ──POST /v1/chat/completions──▶ proxy ──▶ kernel.process_turn
//!                                        │        (fold, measure, nudge)
//!                                        ▼
//!                                     upstream ──▶ stream back
//!                                        │
//!                          text ─────────┴──▶ agent, as it arrives
//!                          proxy tool call ──▶ executed here, never forwarded
//!                                        │
//!                                        └──▶ re-request with the result
//! ```
//!
//! Two rules make the relay safe, and both are upstream's:
//!
//! * A proxy tool call is never forwarded. The agent has no such tool, so
//!   forwarding one would fail the turn.
//! * A round that produced a client tool call ends the relay. The client's work
//!   is more important than the proxy's, so the proxy's result is dropped and
//!   the client gets its turn. Nothing is lost: the next turn nudges again.
//!
//! The proxy is off unless the user turns compaction on, and turning it on adds
//! a provider and a model to `config.toml` rather than editing the user's own
//! entries. See [`super::provider`].

pub mod sse;
pub mod tools;
pub mod upstream;

use std::collections::{BTreeMap, HashSet};
use std::convert::Infallible;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use bytes::Bytes;
use futures_util::StreamExt;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};

use super::compact::nudge::Nudge;
use super::compact::types::{CompressionState, ContentType, CoreMessage, Role};
use super::compact::{rules, wire, Kernel};
use super::{now, registry, CapabilityEvent, CapabilityId};
use crate::error::{AppError, AppResult};

/// How many rounds one turn may take before the proxy gives up and completes.
///
/// Upstream uses ten. The limit exists so a model that keeps calling
/// `acp_retrieve` cannot spin forever; hitting it ends the turn gracefully
/// rather than discarding it.
const MAX_ROUNDS: usize = 10;

/// How many conversations keep their compression state.
///
/// The proxy sees requests, not sessions, so it keys state by a fingerprint of
/// the conversation's opening. Eight is enough for the tabs a person actually
/// has open, and the oldest is evicted rather than growing without bound.
const MAX_SESSIONS: usize = 8;

type ProxyBody = BoxBody<Bytes, std::io::Error>;
type Sender = mpsc::Sender<Result<Frame<Bytes>, std::io::Error>>;

/// The proxy's own state. One per process.
pub struct Proxy {
    client: reqwest::Client,
    sessions: Mutex<BTreeMap<u64, Arc<Session>>>,
}

/// One conversation's compression state.
struct Session {
    inner: Mutex<SessionInner>,
    last_used: Mutex<i64>,
}

struct SessionInner {
    kernel: Kernel,
    state: CompressionState,
    /// The conversation's size after the last turn, in tokens.
    ///
    /// The panel's usage bar is about how full the window is, so it needs the
    /// live size. `stats.tokens_compressed` is a running total of everything
    /// ever folded, which is a different number and can exceed the window.
    last_tokens: u64,
}

impl Session {
    fn new(config: super::compact::types::Config) -> Self {
        Self {
            inner: Mutex::new(SessionInner {
                kernel: Kernel::new(config),
                state: CompressionState::default(),
                last_tokens: 0,
            }),
            last_used: Mutex::new(chrono::Utc::now().timestamp_millis()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, SessionInner> {
        self.inner.lock().unwrap_or_else(|error| error.into_inner())
    }

    fn touch(&self) {
        let mut last = self.last_used.lock().unwrap_or_else(|e| e.into_inner());
        *last = chrono::Utc::now().timestamp_millis();
    }
}

impl Proxy {
    fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                // A model can take minutes. The proxy is not the thing that
                // should decide a turn is over.
                .timeout(Duration::from_secs(600))
                .build()
                .unwrap_or_default(),
            sessions: Mutex::new(BTreeMap::new()),
        }
    }

    fn session(&self, key: u64, config: super::compact::types::Config) -> Arc<Session> {
        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(session) = sessions.get(&key) {
            session.touch();
            // Settings are live. The panel's gates are read from disk on every
            // request, so a conversation that is already open must pick up a
            // changed `compress at` or `distil at` on its next turn rather than
            // on the next launch. The kernel reads its config per turn, so
            // replacing it here is enough.
            session.lock().kernel.config = config;
            return session.clone();
        }
        if sessions.len() >= MAX_SESSIONS {
            let oldest = sessions
                .iter()
                .min_by_key(|(_, session)| {
                    *session.last_used.lock().unwrap_or_else(|e| e.into_inner())
                })
                .map(|(key, _)| *key);
            if let Some(oldest) = oldest {
                sessions.remove(&oldest);
            }
        }
        let session = Arc::new(Session::new(config));
        sessions.insert(key, session.clone());
        session
    }

    /// Every conversation's status, for the panel.
    pub fn statuses(&self) -> Vec<Value> {
        let sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        sessions
            .iter()
            .map(|(key, session)| {
                let inner = session.lock();
                json!({
                    "session": format!("{key:016x}"),
                    "status": inner.kernel.status(&inner.state, inner.last_tokens),
                })
            })
            .collect()
    }
}

/// The port the proxy is listening on, once it has started.
static PORT: std::sync::OnceLock<u16> = std::sync::OnceLock::new();

/// The running proxy, so the panel can read its sessions.
static RUNNING: std::sync::OnceLock<Arc<Proxy>> = std::sync::OnceLock::new();

/// Every conversation the kernel is holding, for the panel.
pub fn statuses() -> Vec<Value> {
    RUNNING
        .get()
        .map(|proxy| proxy.statuses())
        .unwrap_or_default()
}

/// The port the proxy is on, or `None` when it has never started.
pub fn port() -> Option<u16> {
    PORT.get().copied()
}

/// Start the proxy if it is not already running, and return its port.
///
/// The port is remembered for the life of the process: a second call returns the
/// same one rather than binding a second listener, because the provider in
/// `config.toml` can only point at one.
pub async fn ensure_started() -> AppResult<u16> {
    if let Some(port) = port() {
        return Ok(port);
    }
    let preferred = super::settings::load().compaction.proxy_port;
    let port = start(preferred).await?;
    let _ = PORT.set(port);
    Ok(port)
}

/// Whether any capability that lives at this boundary is switched on.
///
/// The proxy is the interception point for three of them, not one: compaction
/// rewrites the conversation, reading and terminal rewrite tool results inside
/// it. Starting it only for compaction is what left the other two unreachable —
/// they were wired to ACP methods the agent never calls, and the one boundary
/// that does see every token was switched off unless compaction was on.
pub fn needed() -> bool {
    let registry = registry();
    registry.is_enabled(CapabilityId::Compaction)
        || registry.is_enabled(CapabilityId::Reading)
        || registry.is_enabled(CapabilityId::Terminal)
}

/// Bind the proxy and start serving. Returns the port it actually got.
///
/// `preferred` is the port recorded in the user's config. If it is taken — a
/// second VibeDesk, a stale process — the proxy takes any free port and the
/// caller rewrites the provider to match. A proxy that refuses to start because
/// of a port collision would be a worse outcome than a changed port.
pub async fn start(preferred: u16) -> AppResult<u16> {
    let listener = match TcpListener::bind(("127.0.0.1", preferred)).await {
        Ok(listener) => listener,
        Err(_) => TcpListener::bind(("127.0.0.1", 0)).await.map_err(|error| {
            AppError::message(format!(
                "the compaction proxy could not bind a loopback port: {error}"
            ))
        })?,
    };
    let port = listener
        .local_addr()
        .map_err(|error| AppError::message(format!("the proxy has no local address: {error}")))?
        .port();

    let proxy = Arc::new(Proxy::new());
    let _ = RUNNING.set(proxy.clone());
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                continue;
            };
            let proxy = proxy.clone();
            tokio::spawn(async move {
                let io = TokioIo::new(stream);
                let service = service_fn(move |request: Request<Incoming>| {
                    let proxy = proxy.clone();
                    async move { Ok::<_, Infallible>(proxy.handle(request).await) }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(io, service)
                    .await;
            });
        }
    });

    Ok(port)
}

/// What one turn needs to carry through its rounds.
struct LoopCtx {
    path: String,
    headers: hyper::HeaderMap,
    document: Value,
    dialect: wire::Dialect,
    messages: Vec<CoreMessage>,
    /// The transient system prompt, applied to the outbound copy of the
    /// conversation rather than to `messages` itself. See `run_loop`.
    system: String,
    /// The clock note, appended to the tail of the outbound copy. It changes on
    /// every request, so it must not sit in the system prompt. See `with_clock`.
    clock: Option<String>,
    session: Arc<Session>,
    streaming: bool,
}

/// What the first upstream request did, reported before the response is built.
///
/// The proxy used to answer `200` the moment it had a channel, which meant an
/// upstream refusal arrived as a successful response carrying an error document.
/// The client then failed to unmarshal it and reported a schema error — the
/// status and the reason both lost. Waiting for the first answer costs nothing
/// the client was not already waiting for, and it lets a refusal keep its own
/// status.
enum Ready {
    /// Upstream accepted the request; the answer follows on the channel.
    Accepted,
    /// Upstream refused it. Nothing was written to the channel.
    Refused(StatusCode, String),
    /// Upstream could not be reached at all.
    Unreachable(String),
}

impl Proxy {
    async fn handle(self: Arc<Self>, request: Request<Incoming>) -> Response<ProxyBody> {
        let method = request.method().clone();
        let path = request.uri().path().to_string();
        // The query string is part of the request the CLI made. Dropping it
        // turns a filtered or paged call into a different one, so the target
        // that goes upstream keeps it even though routing uses the bare path.
        let target = request
            .uri()
            .path_and_query()
            .map(|value| value.as_str().to_string())
            .unwrap_or_else(|| path.clone());
        let headers = request.headers().clone();
        let body = match request.into_body().collect().await {
            Ok(collected) => collected.to_bytes(),
            Err(error) => {
                return text_response(
                    StatusCode::BAD_REQUEST,
                    &format!("the proxy could not read the request body: {error}"),
                )
            }
        };

        if !is_model_path(&path) {
            return self.passthrough(&method, &target, &headers, body).await;
        }

        // Three capabilities live at this boundary, and they are independent.
        // Compaction rewrites the conversation; Reading and Terminal rewrite
        // tool results inside it. A user who wants the second without the first
        // still needs the proxy, so the gate is "any of them", not "compaction".
        let compacting = registry().is_enabled(CapabilityId::Compaction);
        let intercepting = registry().is_enabled(CapabilityId::Reading)
            || registry().is_enabled(CapabilityId::Terminal);
        if !compacting && !intercepting {
            return self.passthrough(&method, &target, &headers, body).await;
        }

        let Ok(mut document) = serde_json::from_slice::<Value>(&body) else {
            return self.passthrough(&method, &target, &headers, body).await;
        };
        let dialect = wire::dialect_for_path(&path);

        // Tool results are rewritten first, on the body itself, so the kernel
        // below measures the payload that actually goes upstream.
        let intercepted = tools::intercept(dialect, &mut document);

        if !compacting {
            if !intercepted.touched() {
                return self.passthrough(&method, &target, &headers, body).await;
            }
            let rewritten = Bytes::from(document.to_string());
            return self.passthrough(&method, &target, &headers, rewritten).await;
        }

        let mut messages = wire::decode(dialect, &document);
        if messages.is_empty() {
            return self.passthrough(&method, &target, &headers, body).await;
        }

        if std::env::var_os("VIBEDESK_TRACE_ACP").is_some() {
            trace_request(&document, &messages, &intercepted);
        }

        let session = self.session(fingerprint(&messages), kernel_config());
        let (tokens_before, tokens_after, nudge, clock) = {
            let mut inner = session.lock();
            let SessionInner { kernel, state, .. } = &mut *inner;
            // Stamped before the fold so the gap measured is the agent's own
            // work, not the kernel's.
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            let clock = rules::time_note(
                state.first_seen_ms,
                state.last_request_ms,
                state.requests,
                now_ms,
            );
            state.stamp_request(now_ms);
            let turn = kernel.process_turn(&mut messages, state);
            inner.last_tokens = turn.tokens_after;
            (turn.tokens_before, turn.tokens_after, turn.nudge, clock)
        };

        // The philosophy prompt is transient: it is rebuilt for every request
        // and never enters the conversation, so it cannot accumulate across
        // rounds the way an injected message would.
        let mut system = transient_system(nudge.as_ref());

        // The method reminder is the one part of this block that is a standing
        // requirement rather than a compression rule, so it is the one part
        // worth being able to see in a trace. A session that lost it looks
        // exactly like a session that ignored it.
        if std::env::var_os("VIBEDESK_TRACE_ACP").is_some() {
            eprintln!(
                "proxy: transient system {} chars, method reminder {}",
                system.chars().count(),
                if system.contains("using-aegis") { "present" } else { "MISSING" }
            );
        }

        // Recall is the join between memory and compaction: a memory written in
        // an earlier session arrives here, in the same transient block, so it
        // costs nothing in the conversation and cannot be folded away.
        if let Some(recall) = recall_for(&messages).await {
            system.push_str("

");
            system.push_str(&recall);
        }
        registry().record(
            CapabilityId::Compaction,
            CapabilityEvent {
                at: now(),
                label: if nudge.is_some() { "nudge" } else { "turn" }.to_string(),
                detail: format!("{tokens_before} → {tokens_after} tokens"),
                bytes_in: tokens_before * 4,
                bytes_out: tokens_after * 4,
                // The kernel works in tokens, so these are the real figures
                // rather than an estimate derived from bytes.
                tokens_in: tokens_before,
                tokens_out: tokens_after,
            },
        );

        let streaming = document
            .get("stream")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let (tx, rx) = mpsc::channel(64);
        let ctx = LoopCtx {
            path,
            headers,
            document,
            dialect,
            messages,
            system,
            clock,
            session,
            streaming,
        };
        let this = self.clone();
        let (ready_tx, ready_rx) = oneshot::channel();
        tokio::spawn(async move { this.run_loop(ctx, tx, ready_tx).await });

        // The status is upstream's to give. Committing `200` before the first
        // answer turned every refusal into a successful response carrying an
        // error document, which the client reported as a schema error.
        match ready_rx.await {
            Ok(Ready::Accepted) => {}
            Ok(Ready::Refused(status, body)) => return raw_response(status, body),
            Ok(Ready::Unreachable(error)) => {
                return text_response(StatusCode::BAD_GATEWAY, &error)
            }
            Err(_) => {
                return text_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "the proxy stopped before it reached upstream",
                )
            }
        }

        let content_type = if streaming {
            "text/event-stream"
        } else {
            "application/json"
        };
        Response::builder()
            .status(StatusCode::OK)
            .header("content-type", content_type)
            .header("cache-control", "no-cache")
            .body(channel_body(rx))
            .unwrap_or_else(|_| {
                text_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "the proxy could not build a response",
                )
            })
    }

    /// Forward a request the proxy does not rewrite, streaming the answer back.
    async fn passthrough(
        &self,
        method: &Method,
        target: &str,
        headers: &hyper::HeaderMap,
        body: Bytes,
    ) -> Response<ProxyBody> {
        let response = match self.forward(method, target, headers, body).await {
            Ok(response) => response,
            Err(error) => return text_response(StatusCode::BAD_GATEWAY, &error),
        };
        let status = response.status();
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("application/json")
            .to_string();

        let (tx, rx) = mpsc::channel(64);
        tokio::spawn(async move {
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let Ok(chunk) = chunk else { break };
                if tx.send(Ok(Frame::data(chunk))).await.is_err() {
                    break;
                }
            }
        });

        Response::builder()
            .status(status)
            .header("content-type", content_type)
            .body(channel_body(rx))
            .unwrap_or_else(|_| {
                text_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "the proxy could not build a response",
                )
            })
    }

    /// Headers that belong to this hop and must not be forwarded.
    ///
    /// `host` and `content-length` describe the connection the caller opened,
    /// not the one the proxy opens. `accept-encoding` is the interesting one:
    /// the proxy reads and rewrites bodies, so it has to negotiate identity.
    /// Forwarding the caller's value asks upstream for a gzip body this client
    /// cannot decode, and the bytes then reach the CLI with no
    /// `content-encoding` to explain them — which surfaced as "Failed to load
    /// connectors: UnicodeDecodeError".
    fn is_hop_header(name: &hyper::header::HeaderName) -> bool {
        name == hyper::header::HOST
            || name == hyper::header::CONTENT_LENGTH
            || name == hyper::header::ACCEPT_ENCODING
    }

    /// Send one request upstream, keeping the method the caller used.
    ///
    /// This used to be `post`, and it always sent POST. The proxy sits in front
    /// of the whole provider base URL, not just the chat route, so every other
    /// call the CLI makes through it — the connector manifest among them — was
    /// turned into a POST and came back 405. The CLI reported it as "Failed to
    /// load connectors", which is how a proxy bug surfaced as the agent losing
    /// its tools.
    async fn forward(
        &self,
        method: &Method,
        target: &str,
        headers: &hyper::HeaderMap,
        body: impl Into<reqwest::Body>,
    ) -> Result<reqwest::Response, String> {
        let provider = super::settings::load()
            .compaction
            .upstream_provider;
        let upstream = upstream::resolve(&provider).map_err(|error| error.to_string())?;
        let url = upstream::join_url(&upstream.base, target);

        let method = reqwest::Method::from_bytes(method.as_str().as_bytes())
            .map_err(|error| format!("the proxy cannot forward {method}: {error}"))?;
        let mut request = self.client.request(method, &url).body(body);
        for (name, value) in headers.iter() {
            if Self::is_hop_header(name) {
                continue;
            }
            request = request.header(name.as_str(), value.as_bytes());
        }
        request
            .send()
            .await
            .map_err(|error| format!("the proxy could not reach {url}: {error}"))
    }

    /// Run the relay: request, read, decide, maybe request again.
    async fn run_loop(
        self: Arc<Self>,
        ctx: LoopCtx,
        tx: Sender,
        ready: oneshot::Sender<Ready>,
    ) {
        let LoopCtx {
            path,
            headers,
            document,
            dialect,
            mut messages,
            system,
            clock,
            session,
            streaming,
        } = ctx;
        let mut emitter = sse::Emitter::new(dialect);
        let mut round = 0usize;
        // Taken by the first round only: after the response is committed there
        // is no status left to change, so later rounds report on the channel.
        let mut ready = Some(ready);

        loop {
            round += 1;
            emitter.begin_round();

            // The transient prompt goes on a copy. Appending it to `messages`
            // would change the first system message's text, and the kernel
            // identifies a message by a hash of its text — so every ref the
            // nudge had just handed the model would stop resolving, and every
            // compression the model asked for would be rejected as unknown.
            let mut outbound = messages.clone();
            with_system(&mut outbound, &system);
            with_clock(&mut outbound, clock.as_deref());
            let mut body = wire::encode(dialect, &document, &outbound);
            strip_model_prefix(&mut body);
            inject_tools(dialect, &mut body, &tools_for(dialect, &session));
            let payload = match serde_json::to_vec(&body) {
                Ok(payload) => payload,
                Err(error) => {
                    send(&tx, error_event(dialect, &format!("could not encode the request: {error}"))).await;
                    break;
                }
            };

            let response = match self.forward(&Method::POST, &path, &headers, payload).await {
                Ok(response) => response,
                Err(error) => {
                    if let Some(ready) = ready.take() {
                        let _ = ready.send(Ready::Unreachable(error));
                    } else {
                        send(&tx, error_event(dialect, &error)).await;
                    }
                    break;
                }
            };

            if !response.status().is_success() {
                let status = response.status();
                let text = response.text().await.unwrap_or_default();
                // The body is the only thing that says *why* upstream refused,
                // and it used to be dropped: the panel recorded the status alone
                // and the client reported a schema error, so a 400 arrived as
                // "body.data.id Field required" with the cause nowhere.
                if std::env::var_os("VIBEDESK_TRACE_ACP").is_some() {
                    eprintln!("proxy: upstream answered {status}: {text}");
                }
                registry().set_blocked(
                    CapabilityId::Compaction,
                    Some(format!(
                        "upstream answered {status}: {}",
                        first_chars(&text, 400)
                    )),
                );
                // The first round still owns the response, so the refusal keeps
                // its own status and body. A later round has already committed
                // one, and there the answer has to stay in the stream's shape.
                if let Some(ready) = ready.take() {
                    let _ = ready.send(Ready::Refused(status, text));
                } else if streaming {
                    send(&tx, error_event(dialect, &format!("upstream answered {status}: {text}")))
                        .await;
                } else {
                    send(&tx, Bytes::from(text.clone())).await;
                }
                break;
            }
            registry().set_blocked(CapabilityId::Compaction, None);
            if let Some(ready) = ready.take() {
                let _ = ready.send(Ready::Accepted);
            }

            let (calls, real_calls, held) = if streaming {
                pump_stream(response, dialect, &mut emitter, &tx).await
            } else {
                pump_json(response, dialect, &tx).await
            };

            let proxy_calls: Vec<sse::PartialCall> = calls
                .into_iter()
                .filter(|call| is_proxy_tool(&call.name))
                .collect();

            // The client's work outranks the proxy's. If the model asked for a
            // real tool, the turn belongs to the client and the proxy's result
            // is dropped — the next turn nudges again.
            if proxy_calls.is_empty() || real_calls > 0 || round >= MAX_ROUNDS {
                for raw in held {
                    send(&tx, raw).await;
                }
                break;
            }

            for call in &proxy_calls {
                let result = self.execute(call, &session, &messages);
                messages.push(tool_call_message(call));
                messages.push(tool_result_message(call, &result));
            }

            // Re-run the kernel so the next round sees the blocks it just made.
            {
                let mut inner = session.lock();
                let SessionInner { kernel, state, last_tokens } = &mut *inner;
                let turn = kernel.process_turn(&mut messages, state);
                *last_tokens = turn.tokens_after;
            }
        }

        let inner = session.lock();
        let status = inner.kernel.status(&inner.state, 0);
        registry().bump(CapabilityId::Compaction, "blocks", status.active_blocks as u64);
        registry().bump(CapabilityId::Compaction, "rounds", round as u64);
    }

    /// Run one of the proxy's own tools and return the text the model sees.
    fn execute(&self, call: &sse::PartialCall, session: &Session, messages: &[CoreMessage]) -> String {
        let mut inner = session.lock();
        let SessionInner { kernel, state, .. } = &mut *inner;
        let input = call.input();

        match call.name.as_str() {
            wire::COMPRESS_TOOL => {
                let ranges = wire::parse_compress_input(&input);
                if ranges.is_empty() {
                    return "No ranges were given. Write one `mNNNNN-mNNNNN topic` header per \
                            range, then the summary under it."
                        .to_string();
                }
                match kernel.apply_compression(&ranges, messages, state) {
                    Ok(blocks) => {
                        registry().bump(CapabilityId::Compaction, "compressions", blocks.len() as u64);
                        format!("Compressed {} range(s): {}.", blocks.len(), blocks.join(", "))
                    }
                    Err(error) => format!("Compression rejected: {}.", error.message()),
                }
            }
            wire::DECOMPRESS_TOOL => {
                let block_id = input
                    .get("blockId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                match kernel.decompress(&block_id, state) {
                    Some(block) => format!(
                        "Restored {}: {} messages are visible again.",
                        block.block_id,
                        block.direct_message_ids.len()
                    ),
                    None => format!("No block named {block_id}."),
                }
            }
            wire::RETRIEVE_TOOL => {
                let reference = input
                    .get("ref")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                match kernel.retrieve(&reference, state) {
                    Some(stored) => stored.text,
                    None => format!("No stored content named {reference}."),
                }
            }
            other => format!("The proxy has no tool named {other}."),
        }
    }
}

/// Read a streamed response, forwarding what belongs to the client.
async fn pump_stream(
    response: reqwest::Response,
    dialect: wire::Dialect,
    emitter: &mut sse::Emitter,
    tx: &Sender,
) -> (Vec<sse::PartialCall>, usize, Vec<Bytes>) {
    let mut reader = sse::StreamReader::new(dialect);
    let mut forwarded: HashSet<usize> = HashSet::new();
    let mut real_calls = 0usize;
    let mut held: Vec<Bytes> = Vec::new();
    let mut stream = response.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let Ok(chunk) = chunk else { break };
        for item in reader.push(&chunk) {
            relay_item(item, emitter, &mut forwarded, &mut real_calls, &mut held, tx).await;
        }
    }
    for item in reader.finish() {
        relay_item(item, emitter, &mut forwarded, &mut real_calls, &mut held, tx).await;
    }

    (reader.calls().to_vec(), real_calls, held)
}

/// Read a non-streamed response. There is nothing to forward early, so the whole
/// body is held until the loop decides what to do with it.
async fn pump_json(
    response: reqwest::Response,
    dialect: wire::Dialect,
    tx: &Sender,
) -> (Vec<sse::PartialCall>, usize, Vec<Bytes>) {
    let body = response.bytes().await.unwrap_or_default();
    let Ok(value) = serde_json::from_slice::<Value>(&body) else {
        return (Vec::new(), 1, vec![body]);
    };
    let calls = sse::calls_in_body(dialect, &value);
    let real_calls = calls.iter().filter(|call| !is_proxy_tool(&call.name)).count();
    let _ = tx;
    (calls, real_calls, vec![body])
}

async fn relay_item(
    item: sse::Item,
    emitter: &mut sse::Emitter,
    forwarded: &mut HashSet<usize>,
    real_calls: &mut usize,
    held: &mut Vec<Bytes>,
    tx: &Sender,
) {
    match item {
        sse::Item::Forward { raw, kind } => {
            for bytes in emitter.forward(&raw, kind) {
                send(tx, bytes).await;
            }
        }
        sse::Item::ToolCallNamed { index, name, raw } => {
            if is_proxy_tool(&name) {
                // Never forwarded: the client has no such tool.
                return;
            }
            forwarded.insert(index);
            *real_calls += 1;
            send(tx, raw).await;
        }
        sse::Item::ToolCallArgs { index, raw } | sse::Item::ToolCallEnd { index, raw } => {
            if forwarded.contains(&index) {
                send(tx, raw).await;
            }
        }
        sse::Item::Done(raw) => held.push(raw),
    }
}

/// Put the model name back to the one upstream knows.
///
/// The client asks for `vibedesk/<name>` because that is the mirrored model it
/// was given. Upstream has never heard of the prefix, and answers
/// `Invalid model` if it sees it, so it is stripped on the way out.
fn strip_model_prefix(body: &mut Value) {
    let Some(model) = body.get("model").and_then(Value::as_str) else {
        return;
    };
    let Some(rest) = model.strip_prefix(super::provider::PROXY_MODEL_PREFIX) else {
        return;
    };
    let rest = rest.to_string();
    if let Some(object) = body.as_object_mut() {
        object.insert("model".into(), Value::String(rest));
    }
}

fn is_proxy_tool(name: &str) -> bool {
    name == wire::COMPRESS_TOOL || name == wire::DECOMPRESS_TOOL || name == wire::RETRIEVE_TOOL
}

fn is_model_path(path: &str) -> bool {
    path.ends_with("/chat/completions") || path.ends_with("/messages")
}

/// The tools the proxy offers this turn.
///
/// `compress` is always offered: the model is the only thing that can write a
/// summary, and it should be able to do so before the window is full.
/// `decompress` and `acp_retrieve` are offered only when there is something to
/// bring back, so the tool list never advertises a call that would fail.
fn tools_for(dialect: wire::Dialect, session: &Session) -> Vec<Value> {
    let inner = session.lock();
    let has_blocks = inner.state.active_blocks().next().is_some();
    let has_stored = !inner.state.store.is_empty();
    let mut tools = Vec::new();
    match dialect {
        wire::Dialect::OpenAi => {
            tools.push(wire::compress_tool_openai());
            if has_blocks {
                tools.push(wire::decompress_tool_openai());
            }
            if has_stored {
                tools.push(wire::retrieve_tool_openai());
            }
        }
        wire::Dialect::Anthropic => {
            tools.push(wire::compress_tool_anthropic());
            if has_blocks {
                tools.push(wire::decompress_tool_anthropic());
            }
            if has_stored {
                tools.push(wire::retrieve_tool_anthropic());
            }
        }
    }
    tools
}

fn inject_tools(dialect: wire::Dialect, body: &mut Value, tools: &[Value]) {
    if tools.is_empty() {
        return;
    }
    let _ = dialect;
    match body.get_mut("tools").and_then(Value::as_array_mut) {
        Some(list) => list.extend(tools.iter().cloned()),
        None => body["tools"] = Value::Array(tools.to_vec()),
    }
}

/// Append the transient system prompt to the conversation's own.
///
/// It goes on the existing system message rather than into a new one so the
/// provider sees one system turn, which is what both dialects expect.
fn with_system(messages: &mut Vec<CoreMessage>, system: &str) {
    if system.trim().is_empty() {
        return;
    }
    if let Some(first) = messages.iter_mut().find(|message| message.role == Role::System) {
        first.text = format!("{}\n\n{system}", first.text);
        // `text` no longer matches the blocks, and a system turn has none to
        // keep anyway.
        first.blocks = None;
    } else {
        messages.insert(0, CoreMessage::text(Role::System, system));
    }
}

/// The memories worth putting in front of the model for this turn.
///
/// The query is the newest user message, which is what the person is actually
/// asking about. A turn with no memories, or a store that cannot be read, adds
/// nothing rather than an empty heading.
async fn recall_for(messages: &[CoreMessage]) -> Option<String> {
    if !registry().is_enabled(CapabilityId::Memory) {
        return None;
    }
    let settings = super::settings::load().memory;
    if !settings.inject_recall {
        return None;
    }
    let query = messages
        .iter()
        .rev()
        .find(|message| message.role == Role::User)
        .map(|message| message.text.clone())?;
    let store = super::memory::store::Store::default();
    let recall = super::memory::recall_as_context(&store, &query, 5).await;
    if recall.trim().is_empty() {
        None
    } else {
        Some(recall)
    }
}

fn transient_system(nudge: Option<&Nudge>) -> String {
    let mut parts = vec![
        rules::METHOD_REMINDER.to_string(),
        rules::COMPRESS_PHILOSOPHY.to_string(),
        rules::HOW_TO_COMPRESS_RULES.to_string(),
        rules::LANGUAGE_PRESERVATION_RULE.to_string(),
        wire::ACP_TAGS_SECTION.to_string(),
    ];
    if let Some(nudge) = nudge {
        parts.push(nudge.text.clone());
    }
    parts.join("\n\n")
}

/// Put the clock on the tail of the request instead of the head.
///
/// The clock names the elapsed time, the request number and the gap since the
/// last request, so its text is different on every single request. It used to
/// ride in the transient system prompt, which is the first token of the
/// request — and a provider caches a request by its prefix. A system prompt
/// that changes every request therefore invalidates the cache from token zero,
/// and the whole conversation is billed as fresh input on every round. That is
/// a real regression: the hit rate fell from the high nineties to about half.
///
/// A trailing message costs nothing, because the tail of a request is new
/// content anyway. Everything before it — the system prompt and every message
/// the conversation has accumulated — stays byte-identical between requests, so
/// the prefix stays cached and only the clock itself is fresh.
///
/// It is appended to the outbound copy, never to the kernel's own array: the
/// kernel identifies a message by a hash of its text, so a message it did not
/// write would not resolve as a ref.
fn with_clock(messages: &mut Vec<CoreMessage>, clock: Option<&str>) {
    let Some(clock) = clock else {
        return;
    };
    if clock.trim().is_empty() {
        return;
    }
    messages.push(CoreMessage::text(Role::User, clock));
}

fn tool_call_message(call: &sse::PartialCall) -> CoreMessage {
    CoreMessage {
        id: String::new(),
        role: Role::Assistant,
        content_type: ContentType::ToolCall,
        text: call.arguments.clone(),
        tool_name: Some(call.name.clone()),
        tool_call_id: Some(call.id.clone()),
        blocks: None,
    }
}

fn tool_result_message(call: &sse::PartialCall, result: &str) -> CoreMessage {
    CoreMessage {
        id: String::new(),
        role: Role::Tool,
        content_type: ContentType::ToolResult,
        text: result.to_string(),
        tool_name: Some(call.name.clone()),
        tool_call_id: Some(call.id.clone()),
        blocks: None,
    }
}

/// A stable key for a conversation.
///
/// The proxy sees requests, not sessions, so it has to recognise a conversation
/// from its own bytes. The opening system prompt plus the first user message is
/// stable across turns and distinct between tabs.
fn fingerprint(messages: &[CoreMessage]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for message in messages.iter().take(2) {
        for byte in message.text.as_bytes() {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash ^= 0xff;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Print what the agent actually sent, when `VIBEDESK_TRACE_ACP` is set.
///
/// The first `limit` characters of a body, for a message a panel has to render.
///
/// An upstream error body is the only description of why a request was refused,
/// and it can be long. The panel shows one line, so this keeps the beginning —
/// which is where the message is — and says how much it left out.
fn first_chars(text: &str, limit: usize) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= limit {
        return collapsed;
    }
    let mut out: String = collapsed.chars().take(limit).collect();
    out.push('…');
    out
}

/// The capability layer has to know two things it cannot guess: the names of the
/// tools the agent declares, and the names it stamps on the results it sends
/// back. Both are the agent's choice, not ours, so the only honest way to learn
/// them is to look at a real request.
fn trace_request(document: &Value, messages: &[CoreMessage], intercepted: &tools::Report) {
    let declared: Vec<&str> = document
        .get("tools")
        .and_then(Value::as_array)
        .map(|tools| {
            tools
                .iter()
                .filter_map(|tool| {
                    tool.get("function")
                        .and_then(|f| f.get("name"))
                        .or_else(|| tool.get("name"))
                        .and_then(Value::as_str)
                })
                .collect()
        })
        .unwrap_or_default();
    eprintln!("proxy: tools declared: {declared:?}");
    if intercepted.touched() || intercepted.reads_passed > 0 || intercepted.commands_passed > 0 {
        eprintln!(
            "proxy: intercepted reads={}/{} commands={}/{} plain={} saved={}",
            intercepted.reads_intercepted,
            intercepted.reads_intercepted + intercepted.reads_passed,
            intercepted.commands_compacted,
            intercepted.commands_compacted + intercepted.commands_passed,
            intercepted.plain_clamped,
            intercepted.saved_chars
        );
    }
    for message in messages {
        if message.role == Role::Tool || message.content_type == ContentType::ToolCall {
            let clipped: String = message.text.chars().take(160).collect();
            eprintln!(
                "proxy: {} {:?} name={:?} id={:?} len={} text={clipped:?}",
                message.role.as_str(),
                message.content_type,
                message.tool_name,
                message.tool_call_id,
                message.text.len()
            );
        }
    }
}

/// The kernel's configuration, from the user's capability settings.
fn kernel_config() -> super::compact::types::Config {
    let settings = super::settings::load().compaction;
    let mut config = super::compact::types::Config::default();
    if settings.context_limit > 0 {
        config.model_context_limit = settings.context_limit;
    }
    config.min_context_limit_pct = settings.nudge_threshold as f32;
    config.emergency_threshold_pct = settings.truncate_threshold as f32;
    config.preserve_recent_messages = settings.preserve_recent as usize;
    config.ccr_enabled = settings.ccr;
    config.ccr_min_tool_tokens = settings.ccr_min_tokens;
    config.min_compress_range = settings.min_compress_range;
    config.min_distill_range = settings.min_distill_range;
    config
}

async fn send(tx: &Sender, bytes: Bytes) {
    let _ = tx.send(Ok(Frame::data(bytes))).await;
}

fn error_event(dialect: wire::Dialect, message: &str) -> Bytes {
    let escaped = serde_json::to_string(message).unwrap_or_else(|_| "\"error\"".to_string());
    match dialect {
        wire::Dialect::OpenAi => Bytes::from(format!(
            "data: {{\"error\":{{\"message\":{escaped}}}}}\n\ndata: [DONE]\n\n"
        )),
        wire::Dialect::Anthropic => Bytes::from(format!(
            "event: error\ndata: {{\"type\":\"error\",\"error\":{{\"type\":\"api_error\",\"message\":{escaped}}}}}\n\n"
        )),
    }
}

fn channel_body(mut rx: mpsc::Receiver<Result<Frame<Bytes>, std::io::Error>>) -> ProxyBody {
    let stream = futures_util::stream::poll_fn(move |cx| rx.poll_recv(cx));
    BodyExt::boxed(StreamBody::new(stream))
}

fn text_response(status: StatusCode, message: &str) -> Response<ProxyBody> {
    let body = json!({ "error": { "message": message } }).to_string();
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(body)).map_err(|never| match never {}).boxed())
        .unwrap_or_else(|_| Response::new(Full::new(Bytes::new()).map_err(|never| match never {}).boxed()))
}

/// Upstream's own refusal, passed through with its own status.
///
/// The body is already an error document in the provider's shape, so rewriting
/// it would only lose the detail the client needs to report the cause.
fn raw_response(status: StatusCode, body: String) -> Response<ProxyBody> {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(body)).map_err(|never| match never {}).boxed())
        .unwrap_or_else(|_| Response::new(Full::new(Bytes::new()).map_err(|never| match never {}).boxed()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conversation() -> Vec<CoreMessage> {
        vec![
            CoreMessage::text(Role::System, "you are a coding agent"),
            CoreMessage::text(Role::User, "fix the parser"),
            CoreMessage::text(Role::Assistant, "on it"),
        ]
    }

    #[test]
    fn the_same_conversation_gets_the_same_key() {
        assert_eq!(fingerprint(&conversation()), fingerprint(&conversation()));
    }

    #[test]
    fn a_different_opening_gets_a_different_key() {
        let mut other = conversation();
        other[1].text = "fix the lexer".into();
        assert_ne!(fingerprint(&conversation()), fingerprint(&other));
    }

    #[test]
    fn a_live_session_picks_up_a_changed_gate() {
        // The panel writes settings to disk and the proxy reads them per
        // request. A conversation that is already open must see the new gate
        // on its next turn; otherwise the fields in the panel are decoration
        // until the app restarts.
        let proxy = Proxy::new();
        let mut first = super::super::compact::types::Config::default();
        first.min_compress_range = 5_000;
        let session = proxy.session(7, first);
        assert_eq!(session.lock().kernel.config.min_compress_range, 5_000);

        let mut second = super::super::compact::types::Config::default();
        second.min_compress_range = 1_200;
        let again = proxy.session(7, second);
        assert_eq!(again.lock().kernel.config.min_compress_range, 1_200);
        assert!(Arc::ptr_eq(&session, &again), "the state must survive");
    }

    #[test]
    fn the_transient_prompt_carries_the_rules_and_the_nudge() {
        let nudge = Nudge {
            voice: super::super::compact::nudge::Voice::Pressure,
            text: "compress now".into(),
            pending_tokens: 12_000,
        };
        let system = transient_system(Some(&nudge));
        assert!(system.contains("Compression Philosophy"));
        assert!(system.contains("compress now"));
        assert!(system.contains("ACP TAGS"));
    }

    #[test]
    fn a_quiet_turn_still_gets_the_rules_but_no_nudge() {
        let system = transient_system(None);
        assert!(system.contains("Compression Philosophy"));
        assert!(!system.contains("compress now"));
    }

    /// The clock names the elapsed time, the request number and the gap since
    /// the last request, so its text differs on every request. It used to ride
    /// in the transient system prompt, which is the first token of the request
    /// — and a provider caches a request by its prefix, so a system prompt that
    /// changes every round invalidates the cache from token zero and the whole
    /// conversation is billed as fresh input. The hit rate fell from the high
    /// nineties to about half. It belongs on the tail, where the request is new
    /// content anyway and everything before it stays byte-identical.
    #[test]
    fn the_clock_rides_on_the_tail_not_the_system_prompt() {
        let clock = "VibeDesk clock: this conversation has been open 26m";
        assert!(
            !transient_system(None).contains("VibeDesk clock"),
            "the clock must not be in the system prompt, or the prefix cache dies"
        );

        let mut messages = conversation();
        let before = messages.clone();
        with_clock(&mut messages, Some(clock));

        assert_eq!(messages.len(), before.len() + 1);
        let tail = messages.last().unwrap();
        assert_eq!(tail.role, Role::User);
        assert_eq!(tail.text, clock);
        // Everything the provider already cached is untouched.
        for (index, message) in before.iter().enumerate() {
            assert_eq!(messages[index].text, message.text);
            assert_eq!(messages[index].role, message.role);
        }
    }

    #[test]
    fn a_missing_clock_adds_nothing() {
        let mut messages = conversation();
        let len = messages.len();
        with_clock(&mut messages, None);
        with_clock(&mut messages, Some("   "));
        assert_eq!(messages.len(), len);
    }

    /// The full method statement rides on the first user turn, which compaction
    /// is free to fold away. This block is rebuilt for every request and never
    /// enters the conversation, so it is the only place the requirement outlives
    /// a long session — and a long session is exactly where an agent stops
    /// loading skills and starts working from memory.
    #[test]
    fn the_method_reminder_is_rebuilt_on_every_turn() {
        let nudge = Nudge {
            voice: super::super::compact::nudge::Voice::Pressure,
            text: "compress now".into(),
            pending_tokens: 12_000,
        };
        for system in [transient_system(None), transient_system(Some(&nudge))] {
            assert!(system.contains("using-aegis"), "the route must be named");
            assert!(
                system.contains("mcp_mastery.load"),
                "the load call must be named, not just the idea of skills"
            );
        }
    }

    #[test]
    fn the_system_prompt_joins_the_existing_one_rather_than_adding_a_turn() {        let mut messages = conversation();
        with_system(&mut messages, "extra");
        assert_eq!(messages.len(), 3);
        assert!(messages[0].text.ends_with("extra"));
        assert!(messages[0].text.starts_with("you are a coding agent"));
    }

    #[test]
    fn a_conversation_with_no_system_message_gets_one_at_the_front() {
        let mut messages = vec![CoreMessage::text(Role::User, "hi")];
        with_system(&mut messages, "extra");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, Role::System);
    }

    /// The proxy sits in front of the whole provider base URL, so it forwards
    /// requests that are not chat completions. Three headers cannot survive that
    /// hop, and `accept-encoding` is the one that broke: the CLI's connector
    /// fetch came back 405 when every request was a POST, and then
    /// `UnicodeDecodeError` once the method was fixed, because the body was gzip
    /// the proxy had no way to decode.
    #[test]
    fn the_headers_that_cannot_survive_the_hop_are_named() {
        use hyper::header::{HeaderName, ACCEPT_ENCODING, AUTHORIZATION, CONTENT_LENGTH, HOST};
        for name in [HOST, CONTENT_LENGTH, ACCEPT_ENCODING] {
            assert!(Proxy::is_hop_header(&name), "{name} must not be forwarded");
        }
        for name in [AUTHORIZATION, HeaderName::from_static("content-type")] {
            assert!(!Proxy::is_hop_header(&name), "{name} must be forwarded");
        }
    }

    /// The kernel identifies a message by a hash of its text, so appending the
    /// transient prompt to the system message after refs are assigned changes
    /// that message's identity. Every ref the nudge had just handed the model
    /// then stops resolving, and every compression comes back "ref m00001 does
    /// not exist in this session" — which is exactly what happened against a
    /// live upstream before the prompt was moved onto the outbound copy.
    #[test]
    fn the_transient_prompt_must_not_touch_the_kernels_message_array() {
        use super::super::compact::types::CompressionState;
        use super::super::compact::{CompressRange, Kernel};

        let mut messages = vec![CoreMessage::text(Role::System, "you are a coding agent")];
        for index in 0..12 {
            let role = if index % 2 == 0 { Role::User } else { Role::Assistant };
            messages.push(CoreMessage::text(role, format!("turn {index}")));
        }
        let mut state = CompressionState::default();
        let mut config = super::super::compact::types::Config::default();
        config.model_context_limit = 10_000_000;
        config.preserve_recent_messages = 1;
        config.preserve_recent_tokens = 1;
        let kernel = Kernel::new(config);
        kernel.process_turn(&mut messages, &mut state);

        let range = CompressRange {
            start_ref: "m00001".into(),
            end_ref: "m00003".into(),
            summary: "A summary long enough to pass the minimum length check.".into(),
            topic: None,
        };

        // A copy carrying the prompt no longer resolves the ref.
        let mut mutated = messages.clone();
        with_system(&mut mutated, "a transient prompt");
        assert!(kernel
            .apply_compression(&[range.clone()], &mutated, &mut state)
            .is_err());

        // The kernel's own array still does, which is why the prompt goes on
        // the outbound copy instead.
        let applied = kernel.apply_compression(&[range], &messages, &mut state);
        assert!(applied.is_ok(), "{:?}", applied.err().map(|e| e.message()));
    }

    #[test]
    fn the_proxy_recognises_its_own_tools_and_nobody_elses() {
        assert!(is_proxy_tool(wire::COMPRESS_TOOL));
        assert!(is_proxy_tool(wire::DECOMPRESS_TOOL));
        assert!(is_proxy_tool(wire::RETRIEVE_TOOL));
        assert!(!is_proxy_tool("exec"));
        assert!(!is_proxy_tool("read_file"));
    }

    #[test]
    fn only_the_two_model_paths_are_rewritten() {
        assert!(is_model_path("/v1/chat/completions"));
        assert!(is_model_path("/v1/messages"));
        assert!(!is_model_path("/v1/models"));
        assert!(!is_model_path("/health"));
    }

    #[test]
    fn the_mirrored_model_name_is_stripped_before_it_goes_upstream() {
        // Upstream has never heard of the prefix and answers `Invalid model`.
        let mut body = json!({ "model": "vibedesk/deepseek-v4-1-flash-1017" });
        strip_model_prefix(&mut body);
        assert_eq!(
            body.get("model").and_then(Value::as_str),
            Some("deepseek-v4-1-flash-1017")
        );
    }

    #[test]
    fn a_model_name_without_the_prefix_is_left_alone() {
        let mut body = json!({ "model": "deepseek" });
        strip_model_prefix(&mut body);
        assert_eq!(body.get("model").and_then(Value::as_str), Some("deepseek"));
    }

    #[test]
    fn a_body_with_no_model_is_left_alone() {
        let mut body = json!({ "messages": [] });
        strip_model_prefix(&mut body);
        assert!(body.get("model").is_none());
    }

    #[test]
    fn tools_are_appended_to_the_clients_own_list() {
        let mut body = json!({ "tools": [{ "type": "function", "function": { "name": "exec" } }] });
        inject_tools(wire::Dialect::OpenAi, &mut body, &[wire::compress_tool_openai()]);
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0]["function"]["name"], "exec");
        assert_eq!(tools[1]["function"]["name"], "compress");
    }

    #[test]
    fn a_body_with_no_tools_gets_a_list() {
        let mut body = json!({ "model": "x" });
        inject_tools(wire::Dialect::Anthropic, &mut body, &[wire::compress_tool_anthropic()]);
        assert_eq!(body["tools"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn an_error_is_spelled_in_the_dialect_the_client_speaks() {
        let openai = String::from_utf8_lossy(&error_event(wire::Dialect::OpenAi, "boom")).to_string();
        assert!(openai.contains("\"error\""));
        assert!(openai.contains("[DONE]"));

        let anthropic =
            String::from_utf8_lossy(&error_event(wire::Dialect::Anthropic, "boom")).to_string();
        assert!(anthropic.contains("event: error"));
        assert!(anthropic.contains("api_error"));
    }

    #[test]
    fn a_proxy_tool_call_becomes_a_pair_of_messages_the_kernel_can_read() {
        let call = sse::PartialCall {
            index: 0,
            id: "call_1".into(),
            name: wire::COMPRESS_TOOL.into(),
            arguments: "{\"content\":\"m1-m2 x\"}".into(),
        };
        let call_message = tool_call_message(&call);
        assert_eq!(call_message.content_type, ContentType::ToolCall);
        assert_eq!(call_message.tool_name.as_deref(), Some("compress"));

        let result_message = tool_result_message(&call, "done");
        assert_eq!(result_message.content_type, ContentType::ToolResult);
        assert_eq!(result_message.tool_call_id.as_deref(), Some("call_1"));
    }
}
