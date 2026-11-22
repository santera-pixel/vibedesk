//! The capability layer.
//!
//! Six upstream projects become capabilities VibeDesk owns. Each one attaches at
//! a boundary that already belongs to this app, so the capability is not a
//! description of something the agent might do — it is code on the path the data
//! actually travels.
//!
//! | Capability | Boundary | Upstream |
//! | --- | --- | --- |
//! | terminal output compaction | `terminal/output` | tokenjuice |
//! | sparse reading | `fs/read_text_file` | SparseReading |
//! | context compression | the model request, via the proxy | billion-context |
//! | reflective memory | the transcript, and the proxy's system prompt | dream-memory |
//! | code graph | MCP registration | codebase-memory-mcp |
//! | web search and read | MCP registration | donsetch |
//!
//! The registry here is the single owner of "is it on", "what has it saved" and
//! "what did it decide". Every capability reports through [`Registry::record`],
//! so the panels read one shape and the totals add up across boundaries.

pub mod codegraph;
pub mod compact;
pub mod external;
pub mod mcp_client;
pub mod memory;
pub mod provider;
pub mod proxy;
pub mod settings;
pub mod sparse;
pub mod tokenjuice;

use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// Where a capability attaches. This is the honest answer to "what does it
/// actually intercept", and it is what the panel shows instead of a vague
/// description.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Boundary {
    /// `terminal/output` — VibeDesk runs the shell, so it sees the bytes first.
    /// Kept for the ACP fallback; no capability reports it as its boundary,
    /// because the agent runs its own shell and never asks for this one.
    TerminalOutput,
    /// `fs/read_text_file` — VibeDesk reads the file on the agent's behalf.
    /// Kept for the ACP fallback, for the same reason.
    FileRead,
    /// The model request itself, reached through the loopback proxy.
    ModelRequest,
    /// `session/update` notifications, which carry the whole transcript.
    Transcript,
    /// A server VibeDesk registers in the agent's `config.toml`.
    McpServer,
}

impl Boundary {
    pub fn label(self) -> &'static str {
        match self {
            Boundary::TerminalOutput => "terminal/output",
            Boundary::FileRead => "fs/read_text_file",
            Boundary::ModelRequest => "model request",
            Boundary::Transcript => "session/update",
            Boundary::McpServer => "mcp server",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CapabilityId {
    /// billion-context's `acp-kernel`.
    Compaction,
    /// tokenjuice's rule engine.
    Terminal,
    /// SparseReading's benefit gate and file card.
    Reading,
    /// dream-memory's consolidation pass.
    Memory,
    /// codebase-memory-mcp.
    CodeGraph,
    /// donsetch.
    Web,
    /// The channeler: a supervised test input channel.
    Channeler,
}

impl CapabilityId {
    pub const ALL: [CapabilityId; 7] = [
        CapabilityId::Compaction,
        CapabilityId::Terminal,
        CapabilityId::Reading,
        CapabilityId::Memory,
        CapabilityId::CodeGraph,
        CapabilityId::Web,
        CapabilityId::Channeler,
    ];

    pub fn key(self) -> &'static str {
        match self {
            CapabilityId::Compaction => "compaction",
            CapabilityId::Terminal => "terminal",
            CapabilityId::Reading => "reading",
            CapabilityId::Memory => "memory",
            CapabilityId::CodeGraph => "code-graph",
            CapabilityId::Web => "web",
            CapabilityId::Channeler => "channeler",
        }
    }

    pub fn from_key(value: &str) -> Option<Self> {
        CapabilityId::ALL
            .into_iter()
            .find(|id| id.key() == value.trim().to_ascii_lowercase())
    }

    pub fn name(self) -> &'static str {
        match self {
            CapabilityId::Compaction => "Context compaction",
            CapabilityId::Terminal => "Terminal compaction",
            CapabilityId::Reading => "Sparse reading",
            CapabilityId::Memory => "Reflective memory",
            CapabilityId::CodeGraph => "Code graph",
            CapabilityId::Web => "Web search and read",
            CapabilityId::Channeler => "Test channel",
        }
    }

    pub fn repo(self) -> &'static str {
        match self {
            CapabilityId::Compaction => "billion-context",
            CapabilityId::Terminal => "tokenjuice",
            CapabilityId::Reading => "SparseReading",
            CapabilityId::Memory => "dream-memory",
            CapabilityId::CodeGraph => "codebase-memory-mcp",
            CapabilityId::Web => "donsetch",
            CapabilityId::Channeler => "vibedesk",
        }
    }

    pub fn summary(self) -> &'static str {
        match self {
            CapabilityId::Compaction => {
                "Three-tier context compression. The model writes the summaries; \
                 the kernel decides when, what range, and what survives."
            }
            CapabilityId::Terminal => {
                "266 rules that shrink noisy command output before the agent reads it."
            }
            CapabilityId::Reading => {
                "A benefit gate and a file card, so a large file arrives as a map \
                 the agent can page instead of a dump it has to swallow."
            }
            CapabilityId::Memory => {
                "A consolidation pass over finished sessions that keeps what was new \
                 and drops what went stale."
            }
            CapabilityId::CodeGraph => {
                "A local knowledge graph of the repository, queried over MCP."
            }
            CapabilityId::Web => {
                "Search and page reads over MCP, without launching a browser."
            }
            CapabilityId::Channeler => {
                "A supervised input channel, so the agent can drive what it just                  built and learn what broke in the same breath."
            }
        }
    }

    pub fn boundary(self) -> Boundary {
        match self {
            // Reading and Terminal were built against `fs/read_text_file` and
            // `terminal/output`, and the agent never calls either: it uses its
            // own `read_file` and `bash`. They now run on the tool results
            // inside the model request, which is the boundary that sees every
            // token. The ACP handlers still answer if an agent does call them,
            // but that is the fallback, not where the work happens.
            CapabilityId::Compaction => Boundary::ModelRequest,
            CapabilityId::Terminal => Boundary::ModelRequest,
            CapabilityId::Reading => Boundary::ModelRequest,
            CapabilityId::Memory => Boundary::Transcript,
            CapabilityId::CodeGraph | CapabilityId::Web | CapabilityId::Channeler => {
                Boundary::McpServer
            }
        }
    }

    /// The panel that renders this capability.
    ///
    /// Every capability has one. A capability with no data surface of its own
    /// would be a capability the user cannot see, and the ledger exists so that
    /// "is it on, and what has it saved" has one answer for all of them.
    pub fn panel(self) -> &'static str {
        match self {
            CapabilityId::Compaction => "compaction",
            CapabilityId::Terminal => "terminal",
            CapabilityId::Reading => "reading",
            CapabilityId::Memory => "memory",
            CapabilityId::CodeGraph => "code-graph",
            CapabilityId::Web => "web",
            CapabilityId::Channeler => "channeler",
        }
    }

    /// Whether the capability is on unless the user turns it off.
    ///
    /// The two that rewrite what the agent sees on a boundary VibeDesk already
    /// owns — terminal output and file reads — default on, because they are
    /// lossless in the sense that matters: the raw bytes stay available and the
    /// agent can ask for them. Compaction defaults off because it changes the
    /// model request, which is a bigger promise to make on the user's behalf.
    pub fn default_enabled(self) -> bool {
        matches!(self, CapabilityId::Terminal | CapabilityId::Reading)
    }
}

/// One thing a capability did, kept for the panel.
///
/// Two units, because the capabilities do not all work in the same one. The
/// kernel folds tokens and knows the exact count; the terminal and the reading
/// gate measure characters. Reporting only bytes made the panel answer a
/// question nobody asked — "how many bytes did this save" — when the number
/// that matters is how much of the window it freed. `tokens_*` is zero when the
/// capability has no token figure to give, and the panel falls back to bytes
/// rather than inventing one.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityEvent {
    pub at: String,
    /// What happened, in the capability's own vocabulary: a rule id, a block id,
    /// a gate decision.
    pub label: String,
    /// The one-line reason, when there is one worth showing.
    pub detail: String,
    pub bytes_in: u64,
    pub bytes_out: u64,
    #[serde(default)]
    pub tokens_in: u64,
    #[serde(default)]
    pub tokens_out: u64,
}

impl CapabilityEvent {
    pub fn saved(&self) -> u64 {
        self.bytes_in.saturating_sub(self.bytes_out)
    }

    pub fn tokens_saved(&self) -> u64 {
        self.tokens_in.saturating_sub(self.tokens_out)
    }

    /// Whether this event carries a token figure at all.
    pub fn has_tokens(&self) -> bool {
        self.tokens_in > 0
    }
}

/// Running totals for one capability.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityStats {
    pub calls: u64,
    pub bytes_in: u64,
    pub bytes_out: u64,
    /// Zero when no event in this capability reported a token figure.
    #[serde(default)]
    pub tokens_in: u64,
    #[serde(default)]
    pub tokens_out: u64,
    /// Capability-specific counters — rules fired, blocks created, memories
    /// kept. Named rather than positional so the panel can label them.
    pub counters: BTreeMap<String, u64>,
    pub last_at: Option<String>,
    /// Newest first, capped at [`RECENT_LIMIT`].
    pub recent: VecDeque<CapabilityEvent>,
}

/// How many events a capability keeps for its panel.
///
/// Twenty is enough to see a pattern and small enough that the whole registry
/// serialises in a few kilobytes on every poll.
pub const RECENT_LIMIT: usize = 20;

impl CapabilityStats {
    pub fn saved(&self) -> u64 {
        self.bytes_in.saturating_sub(self.bytes_out)
    }

    pub fn tokens_saved(&self) -> u64 {
        self.tokens_in.saturating_sub(self.tokens_out)
    }

    /// Whether this capability reports tokens rather than only bytes.
    pub fn has_tokens(&self) -> bool {
        self.tokens_in > 0
    }

    pub fn bump(&mut self, counter: &str, by: u64) {
        *self.counters.entry(counter.to_string()).or_insert(0) += by;
    }
}

/// What the panel renders for one capability.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityRecord {
    pub id: String,
    pub name: String,
    pub repo: String,
    pub summary: String,
    pub boundary: String,
    pub panel: String,
    pub enabled: bool,
    /// Whether the capability can run at all right now — a missing binary, a
    /// proxy that is not listening. `None` means "no precondition to check".
    pub ready: Option<bool>,
    /// Why it is not ready, when it is not.
    pub blocked: Option<String>,
    pub stats: CapabilityStats,
}

/// The single owner of capability state.
///
/// One mutex over the whole map rather than one per capability: every write is
/// a handful of integer adds, and a single lock means a snapshot is consistent
/// across capabilities, which is what the totals in the panel need.
#[derive(Default)]
pub struct Registry {
    inner: Mutex<RegistryInner>,
}

#[derive(Default)]
struct RegistryInner {
    enabled: BTreeMap<CapabilityId, bool>,
    stats: BTreeMap<CapabilityId, CapabilityStats>,
    /// Set by the capability itself when it knows it cannot run: the proxy is
    /// not listening, the binary is not installed.
    blocked: BTreeMap<CapabilityId, String>,
    /// The name the agent gave its read tool, learned from a real request.
    ///
    /// The reading panel's probe shows "the card the agent would receive", and
    /// the card ends with the calls that fetch the bytes. Those calls have to
    /// name the agent's tool, not ours, so the probe needs the name the agent
    /// actually used. It is learned at the proxy boundary rather than guessed.
    read_tool: Option<String>,
}

impl Registry {
    pub fn new() -> Self {
        let mut inner = RegistryInner::default();
        for id in CapabilityId::ALL {
            inner.enabled.insert(id, id.default_enabled());
            inner.stats.insert(id, CapabilityStats::default());
        }
        Self {
            inner: Mutex::new(inner),
        }
    }

    /// Apply the persisted choices on top of the defaults.
    pub fn load(&self, saved: &settings::CapabilitySettings) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        for id in CapabilityId::ALL {
            let value = saved
                .enabled
                .get(id.key())
                .copied()
                .unwrap_or_else(|| id.default_enabled());
            inner.enabled.insert(id, value);
        }
    }

    pub fn is_enabled(&self, id: CapabilityId) -> bool {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.enabled.get(&id).copied().unwrap_or(false)
    }

    pub fn set_enabled(&self, id: CapabilityId, enabled: bool) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.enabled.insert(id, enabled);
        if enabled {
            // A capability that was blocked when it was switched off may have
            // been fixed since. Clearing here means the next snapshot re-checks
            // rather than showing a stale reason.
            inner.blocked.remove(&id);
        }
    }

    /// Remember the agent's read tool name, as seen on the wire.
    pub fn note_read_tool(&self, name: &str) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.read_tool.as_deref() != Some(name) {
            inner.read_tool = Some(name.to_string());
        }
    }

    /// The agent's read tool name, or `read_file` before one has been seen.
    pub fn read_tool(&self) -> String {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner
            .read_tool
            .clone()
            .unwrap_or_else(|| "read_file".to_string())
    }

    pub fn set_blocked(&self, id: CapabilityId, reason: Option<String>) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match reason {
            Some(reason) => {
                inner.blocked.insert(id, reason);
            }
            None => {
                inner.blocked.remove(&id);
            }
        }
    }

    /// Record one decision. Called from the boundary, on the hot path, so it
    /// does no allocation beyond the event itself and never fails.
    pub fn record(&self, id: CapabilityId, event: CapabilityEvent) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let stats = inner.stats.entry(id).or_default();
        stats.calls += 1;
        stats.bytes_in += event.bytes_in;
        stats.bytes_out += event.bytes_out;
        stats.tokens_in += event.tokens_in;
        stats.tokens_out += event.tokens_out;
        stats.last_at = Some(event.at.clone());
        stats.recent.push_front(event);
        while stats.recent.len() > RECENT_LIMIT {
            stats.recent.pop_back();
        }
    }

    /// Add to a named counter without recording an event. Used for things that
    /// happen many times per call — rules fired, lines dropped.
    pub fn bump(&self, id: CapabilityId, counter: &str, by: u64) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.stats.entry(id).or_default().bump(counter, by);
    }

    pub fn reset(&self, id: CapabilityId) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.stats.insert(id, CapabilityStats::default());
    }

    pub fn snapshot(&self) -> Vec<CapabilityRecord> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        CapabilityId::ALL
            .into_iter()
            .map(|id| {
                let blocked = inner.blocked.get(&id).cloned();
                CapabilityRecord {
                    id: id.key().to_string(),
                    name: id.name().to_string(),
                    repo: id.repo().to_string(),
                    summary: id.summary().to_string(),
                    boundary: id.boundary().label().to_string(),
                    panel: id.panel().to_string(),
                    enabled: inner.enabled.get(&id).copied().unwrap_or(false),
                    ready: blocked.as_ref().map(|_| false),
                    blocked,
                    stats: inner.stats.get(&id).cloned().unwrap_or_default(),
                }
            })
            .collect()
    }

    /// The enabled choices, for persisting.
    pub fn enabled_map(&self) -> BTreeMap<String, bool> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner
            .enabled
            .iter()
            .map(|(id, value)| (id.key().to_string(), *value))
            .collect()
    }
}

/// A timestamp in the shape the rest of the app uses.
pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// The process-wide registry.
///
/// One app has one set of capabilities, and the boundaries that report into it
/// — the ACP bridge, the loopback proxy, the MCP registrar — are reached from
/// places that share no handle. A `OnceLock` is the honest shape for that:
/// initialised once at startup, read from anywhere, never re-created.
static REGISTRY: std::sync::OnceLock<Registry> = std::sync::OnceLock::new();

pub fn registry() -> &'static Registry {
    REGISTRY.get_or_init(Registry::new)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_capability_has_a_distinct_key_and_repo() {
        let mut keys: Vec<&str> = CapabilityId::ALL.iter().map(|id| id.key()).collect();
        keys.sort_unstable();
        let before = keys.len();
        keys.dedup();
        assert_eq!(before, keys.len(), "two capabilities share a key");

        let mut repos: Vec<&str> = CapabilityId::ALL.iter().map(|id| id.repo()).collect();
        repos.sort_unstable();
        let before = repos.len();
        repos.dedup();
        assert_eq!(before, repos.len(), "two capabilities share a repo");
    }

    #[test]
    fn a_key_round_trips_through_from_key() {
        for id in CapabilityId::ALL {
            assert_eq!(CapabilityId::from_key(id.key()), Some(id));
        }
        assert_eq!(CapabilityId::from_key("nonsense"), None);
    }

    /// A capability the user cannot open is a capability they cannot judge. The
    /// ledger is the one place that has to be true for all of them.
    #[test]
    fn every_capability_has_a_panel_and_no_two_share_one() {
        let mut panels: Vec<&str> = CapabilityId::ALL.iter().map(|id| id.panel()).collect();
        panels.sort_unstable();
        let before = panels.len();
        panels.dedup();
        assert_eq!(before, panels.len(), "two capabilities share a panel");
    }

    #[test]
    fn compaction_is_off_until_the_user_asks_for_it() {
        assert!(!CapabilityId::Compaction.default_enabled());
        assert!(CapabilityId::Terminal.default_enabled());
        assert!(CapabilityId::Reading.default_enabled());
    }

    #[test]
    fn recording_an_event_moves_the_totals_and_keeps_newest_first() {
        let registry = Registry::new();
        for index in 0..(RECENT_LIMIT + 5) {
            registry.record(
                CapabilityId::Terminal,
                CapabilityEvent {
                    at: now(),
                    label: format!("rule-{index}"),
                    detail: String::new(),
                    bytes_in: 100,
                    bytes_out: 10,
                    tokens_in: 25,
                    tokens_out: 3,
                },
            );
        }
        let record = registry
            .snapshot()
            .into_iter()
            .find(|r| r.id == "terminal")
            .unwrap();
        assert_eq!(record.stats.calls, (RECENT_LIMIT + 5) as u64);
        assert_eq!(record.stats.bytes_in, 100 * (RECENT_LIMIT as u64 + 5));
        assert_eq!(record.stats.saved(), 90 * (RECENT_LIMIT as u64 + 5));
        assert_eq!(record.stats.tokens_in, 25 * (RECENT_LIMIT as u64 + 5));
        assert_eq!(record.stats.tokens_saved(), 22 * (RECENT_LIMIT as u64 + 5));
        assert!(record.stats.has_tokens());
        assert_eq!(record.stats.recent.len(), RECENT_LIMIT);
        assert_eq!(
            record.stats.recent.front().unwrap().label,
            format!("rule-{}", RECENT_LIMIT + 4)
        );
    }

    #[test]
    fn a_blocked_capability_reports_not_ready_and_says_why() {
        let registry = Registry::new();
        registry.set_blocked(CapabilityId::Web, Some("donsetch is not installed".into()));
        let record = registry
            .snapshot()
            .into_iter()
            .find(|r| r.id == "web")
            .unwrap();
        assert_eq!(record.ready, Some(false));
        assert_eq!(record.blocked.as_deref(), Some("donsetch is not installed"));

        registry.set_blocked(CapabilityId::Web, None);
        let record = registry
            .snapshot()
            .into_iter()
            .find(|r| r.id == "web")
            .unwrap();
        assert_eq!(record.ready, None);
        assert!(record.blocked.is_none());
    }

    #[test]
    fn saved_choices_override_the_defaults() {
        let registry = Registry::new();
        let mut saved = settings::CapabilitySettings::default();
        saved.enabled.insert("compaction".into(), true);
        saved.enabled.insert("terminal".into(), false);
        registry.load(&saved);
        assert!(registry.is_enabled(CapabilityId::Compaction));
        assert!(!registry.is_enabled(CapabilityId::Terminal));
        // A capability the file does not mention keeps its default.
        assert!(registry.is_enabled(CapabilityId::Reading));
    }
}
