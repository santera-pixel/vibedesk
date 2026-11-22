//! Reflective memory: a consolidation pass over finished sessions.
//!
//! Ported from dream-memory's `dream-core`. The shape is upstream's:
//!
//! ```text
//! sessions ──▶ Orient ──▶ Gather ──▶ Consolidate ──▶ Prune ──▶ memories/
//!                │           │            │             │
//!          read headers   transcripts   one model    delete what
//!          so updates     as text       call, JSON   is stale or
//!          beat creates                 actions      over the cap
//! ```
//!
//! Two things make this a capability rather than a library sitting next to one:
//!
//! * The **gate** is the store's own session log, so a pass runs when there is
//!   actually something new, not on a timer that fires on an idle machine.
//! * The **recall** text is injected into the proxy's transient system prompt,
//!   so a memory written in one session is in front of the model in the next
//!   one. That is the join between this capability and compaction, and it is the
//!   reason both are worth having.
//!
//! The model call goes to the provider directly rather than through the proxy.
//! A consolidation pass is not a conversation and must not be compacted.

pub mod prompts;
pub mod store;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config;
use crate::error::{AppError, AppResult};

use super::proxy::upstream;
use super::{now, registry, CapabilityEvent, CapabilityId};
use store::{free_id, Memory, MemoryType, Store};

/// One message of a session, as the consolidator sees it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}

/// A finished session to consolidate from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub messages: Vec<Message>,
}

/// What one pass did.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DreamResult {
    pub success: bool,
    pub phase: String,
    pub memories_scanned: usize,
    pub memories_created: usize,
    pub memories_updated: usize,
    pub memories_pruned: usize,
    pub duration_ms: u64,
    pub summary: String,
    pub error: Option<String>,
}

impl DreamResult {
    fn failed(phase: &str, error: impl ToString) -> Self {
        Self {
            success: false,
            phase: phase.to_string(),
            memories_scanned: 0,
            memories_created: 0,
            memories_updated: 0,
            memories_pruned: 0,
            duration_ms: 0,
            summary: format!("The pass stopped during {phase}."),
            error: Some(error.to_string()),
        }
    }
}

/// The actions a consolidation pass asks for.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct Actions {
    create: Vec<CreateAction>,
    update: Vec<UpdateAction>,
    delete: Vec<DeleteAction>,
    summary: String,
}

#[derive(Debug, Clone, Deserialize)]
struct CreateAction {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    content: String,
}

#[derive(Debug, Clone, Deserialize)]
struct UpdateAction {
    id: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    content: String,
}

#[derive(Debug, Clone, Deserialize)]
struct DeleteAction {
    id: String,
    #[serde(default)]
    reason: String,
}

/// Why a pass did not run.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Gate {
    pub ready: bool,
    pub reason: String,
    pub sessions_since: usize,
    pub hours_since: f64,
}

/// Whether a pass is worth running.
///
/// Both conditions must hold, which is upstream's rule: a pass needs new
/// material *and* enough distance from the last one, so a burst of short
/// sessions does not trigger a model call each time.
pub fn gate(store: &Store, min_hours: f64, min_sessions: usize) -> Gate {
    let last = store.last_consolidated_at();
    let sessions_since = store.count_sessions_since(last);
    let hours_since = if last == 0 {
        f64::INFINITY
    } else {
        (chrono::Utc::now().timestamp_millis() - last) as f64 / 3_600_000.0
    };

    if sessions_since < min_sessions {
        return Gate {
            ready: false,
            reason: format!(
                "{sessions_since} session(s) since the last pass; {min_sessions} needed"
            ),
            sessions_since,
            hours_since,
        };
    }
    if hours_since < min_hours {
        return Gate {
            ready: false,
            reason: format!("{hours_since:.1}h since the last pass; {min_hours}h needed"),
            sessions_since,
            hours_since,
        };
    }
    Gate {
        ready: true,
        reason: format!("{sessions_since} new session(s) to consolidate"),
        sessions_since,
        hours_since,
    }
}

/// A model client for the consolidation pass.
///
/// It talks to the provider directly, with the same base URL and key the agent
/// uses. The key is read from the provider's environment variable, falling back
/// to the CLI's own `.env`, and is never logged or returned.
struct Llm {
    base: String,
    key: String,
    model: String,
    scoring_model: String,
}

impl Llm {
    fn resolve() -> AppResult<Self> {
        let (document, issues) = config::load_effective(None);
        if let Some(issue) = issues.first() {
            return Err(AppError::message(issue.clone()));
        }
        let active = document
            .active_model
            .clone()
            .ok_or_else(|| AppError::message("no active model is configured"))?;
        let model = document
            .models
            .iter()
            .find(|entry| entry.alias == active || entry.name == active)
            .ok_or_else(|| {
                AppError::message(format!("the active model {active} is not in config.toml"))
            })?;
        let provider = document
            .providers
            .iter()
            .find(|entry| entry.name == model.provider);
        let base = upstream::base_for(&model.provider)?;
        let key = provider
            .map(|entry| entry.api_key_env_var.clone())
            .filter(|name| !name.is_empty())
            .and_then(|name| secret(&name))
            .ok_or_else(|| {
                AppError::message(format!(
                    "no API key is set for provider {}. Set {} in the environment or in \
                     ~/.vibe/.env",
                    model.provider,
                    provider
                        .map(|entry| entry.api_key_env_var.clone())
                        .filter(|name| !name.is_empty())
                        .unwrap_or_else(|| "the provider's key".to_string())
                ))
            })?;
        Ok(Self {
            base,
            key,
            model: model.name.clone(),
            scoring_model: model.name.clone(),
        })
    }

    async fn complete(&self, system: &str, user: &str) -> AppResult<String> {
        self.call(&self.model, system, user).await
    }

    async fn score(&self, user: &str) -> AppResult<String> {
        self.call(&self.scoring_model, "", user).await
    }

    async fn call(&self, model: &str, system: &str, user: &str) -> AppResult<String> {
        let mut messages = Vec::new();
        if !system.is_empty() {
            messages.push(serde_json::json!({ "role": "system", "content": system }));
        }
        messages.push(serde_json::json!({ "role": "user", "content": user }));

        let url = upstream::join_url(&self.base, "/v1/chat/completions");
        let response = reqwest::Client::new()
            .post(&url)
            .bearer_auth(&self.key)
            .json(&serde_json::json!({
                "model": model,
                "messages": messages,
                "temperature": 0.2,
                "stream": false,
            }))
            .timeout(std::time::Duration::from_secs(180))
            .send()
            .await
            .map_err(|error| AppError::message(format!("could not reach {url}: {error}")))?;

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|error| AppError::message(format!("could not read the reply: {error}")))?;
        if !status.is_success() {
            return Err(AppError::message(format!(
                "the provider answered {status}: {}",
                body.chars().take(400).collect::<String>()
            )));
        }
        let value: Value = serde_json::from_str(&body)
            .map_err(|error| AppError::message(format!("the reply was not JSON: {error}")))?;
        value
            .get("choices")
            .and_then(|choices| choices.get(0))
            .and_then(|choice| choice.get("message"))
            .and_then(|message| message.get("content"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| AppError::message("the reply had no message content"))
    }
}

/// A secret from the environment, falling back to the CLI's `.env`.
///
/// The value is returned to the caller and never logged. This is the same
/// resolution the CLI itself performs, which is why the app can make a request
/// on the user's behalf without asking them to paste a key.
fn secret(name: &str) -> Option<String> {
    if let Ok(value) = std::env::var(name) {
        if !value.trim().is_empty() {
            return Some(value);
        }
    }
    let path = crate::paths::vibe_home().join(".env");
    let raw = std::fs::read_to_string(path).ok()?;
    for line in raw.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != name {
            continue;
        }
        let value = value.trim().trim_matches('"').trim_matches('\'');
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

/// Run one consolidation pass.
pub async fn dream(store: &Store, sessions: &[Session], max_memories: usize) -> DreamResult {
    let started = std::time::Instant::now();
    let mut result = DreamResult {
        success: true,
        phase: "orient".into(),
        memories_scanned: 0,
        memories_created: 0,
        memories_updated: 0,
        memories_pruned: 0,
        duration_ms: 0,
        summary: String::new(),
        error: None,
    };

    // Phase 1 — Orient.
    let existing = store.list_headers();
    result.memories_scanned = existing.len();

    // Phase 2 — Gather.
    result.phase = "gather".into();
    let transcripts: Vec<String> = sessions
        .iter()
        .map(|session| {
            session
                .messages
                .iter()
                .map(|message| {
                    let prefix = match message.role.as_str() {
                        "user" => "Human",
                        "assistant" => "Assistant",
                        _ => "System",
                    };
                    format!("{prefix}: {}", message.content)
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .collect();

    if transcripts.is_empty() && existing.is_empty() {
        result.phase = "prune".into();
        result.summary = "Nothing to consolidate — no sessions or memories found.".into();
        result.duration_ms = started.elapsed().as_millis() as u64;
        return result;
    }

    // Phase 3 — Consolidate.
    result.phase = "consolidate".into();
    let llm = match Llm::resolve() {
        Ok(llm) => llm,
        Err(error) => {
            let mut failed = DreamResult::failed("consolidate", error);
            failed.memories_scanned = result.memories_scanned;
            failed.duration_ms = started.elapsed().as_millis() as u64;
            return failed;
        }
    };

    let user_prompt = prompts::consolidation_user(&existing, &transcripts, max_memories);
    let reply = match llm.complete(prompts::CONSOLIDATION_SYSTEM, &user_prompt).await {
        Ok(reply) => reply,
        Err(error) => {
            let mut failed = DreamResult::failed("consolidate", error);
            failed.memories_scanned = result.memories_scanned;
            failed.duration_ms = started.elapsed().as_millis() as u64;
            return failed;
        }
    };
    let actions = parse_actions(&reply);

    for action in &actions.create {
        let kind = MemoryType::parse(&action.kind).unwrap_or(MemoryType::Project);
        let id = free_id(store, &action.name);
        let stamp = chrono::Utc::now().timestamp_millis();
        let memory = Memory {
            id,
            name: action.name.clone(),
            description: action.description.clone(),
            content: action.content.clone(),
            kind,
            created_at: stamp,
            updated_at: stamp,
            source: sessions.first().map(|session| session.id.clone()),
        };
        if store.set(&memory).is_ok() {
            result.memories_created += 1;
        }
    }

    for action in &actions.update {
        let Some(existing) = store.get(&action.id) else {
            continue;
        };
        let updated = Memory {
            description: action
                .description
                .clone()
                .filter(|text| !text.trim().is_empty())
                .unwrap_or(existing.description),
            content: if action.content.trim().is_empty() {
                existing.content
            } else {
                action.content.clone()
            },
            updated_at: chrono::Utc::now().timestamp_millis(),
            ..existing
        };
        if store.set(&updated).is_ok() {
            result.memories_updated += 1;
        }
    }

    // Phase 4 — Prune.
    result.phase = "prune".into();
    // The prompt asks the model for a reason per deletion, and that reason is the
    // only account of why a memory went. It was deserialised and dropped, so the
    // pass reported a count with nothing behind it.
    let mut pruned: Vec<String> = Vec::new();
    for action in &actions.delete {
        if store.delete(&action.id).is_ok() {
            result.memories_pruned += 1;
            let reason = action.reason.trim();
            if !reason.is_empty() {
                pruned.push(format!("{} — {reason}", action.id));
            }
        }
    }

    // The cap is enforced here rather than trusted to the model: a pass that
    // keeps creating memories would otherwise grow without bound.
    let after = store.list_headers();
    if after.len() > max_memories {
        for header in after.iter().skip(max_memories) {
            if store.delete(&header.id).is_ok() {
                result.memories_pruned += 1;
            }
        }
    }

    let _ = store.set_last_consolidated_at(chrono::Utc::now().timestamp_millis());
    result.summary = if actions.summary.trim().is_empty() {
        "Consolidation complete.".to_string()
    } else {
        actions.summary.clone()
    };
    // Bounded: a pass that prunes twenty memories should not turn its summary
    // into a list. The count is already in `memories_pruned`.
    if !pruned.is_empty() {
        let shown = pruned.iter().take(3).cloned().collect::<Vec<_>>().join("; ");
        let more = pruned.len().saturating_sub(3);
        result.summary = if more > 0 {
            format!("{} Pruned: {shown}; and {more} more.", result.summary)
        } else {
            format!("{} Pruned: {shown}.", result.summary)
        };
    }
    result.duration_ms = started.elapsed().as_millis() as u64;

    registry().record(
        CapabilityId::Memory,
        CapabilityEvent {
            at: now(),
            label: "dream".into(),
            detail: result.summary.clone(),
            bytes_in: (result.memories_scanned * 200) as u64,
            bytes_out: ((result.memories_created + result.memories_updated) * 400) as u64,
            // A dream pass reads memories and writes a few; it has no token
            // figure of its own, and inventing one from the byte estimate
            // would be a number the panel could not stand behind.
            tokens_in: 0,
            tokens_out: 0,
        },
    );
    registry().bump(CapabilityId::Memory, "created", result.memories_created as u64);
    registry().bump(CapabilityId::Memory, "pruned", result.memories_pruned as u64);

    result
}

/// Parse the model's JSON, tolerating the code fences it likes to add.
fn parse_actions(reply: &str) -> Actions {
    let cleaned = reply
        .replace("```json\n", "")
        .replace("```json", "")
        .replace("```\n", "")
        .replace("```", "");
    let trimmed = cleaned.trim();
    // A model that wraps the object in prose still gets read: take the outermost
    // braces rather than giving up on the whole pass.
    let candidate = match (trimmed.find('{'), trimmed.rfind('}')) {
        (Some(start), Some(end)) if end > start => &trimmed[start..=end],
        _ => trimmed,
    };
    serde_json::from_str::<Actions>(candidate).unwrap_or_default()
}

/// The memories that help with this query, chosen by the cheap model.
pub async fn find_relevant(store: &Store, query: &str, limit: usize) -> Vec<Memory> {
    let headers = store.list_headers();
    if headers.is_empty() || query.trim().is_empty() {
        return Vec::new();
    }
    let Ok(llm) = Llm::resolve() else {
        return Vec::new();
    };
    let prompt = prompts::relevance_scoring(query, &headers);
    let Ok(reply) = llm.score(&prompt).await else {
        return Vec::new();
    };
    let ids = parse_ids(&reply);
    ids.into_iter()
        .take(limit)
        .filter_map(|id| store.get(&id))
        .collect()
}

fn parse_ids(reply: &str) -> Vec<String> {
    let trimmed = reply.trim();
    let candidate = match (trimmed.find('['), trimmed.rfind(']')) {
        (Some(start), Some(end)) if end > start => &trimmed[start..=end],
        _ => return Vec::new(),
    };
    serde_json::from_str::<Vec<String>>(candidate).unwrap_or_default()
}

/// The recall block injected into the proxy's system prompt.
///
/// Upstream's `recallAsContext`, kept in shape: a heading, then one section per
/// memory with its type as the label. Empty when nothing is relevant, so a turn
/// with no memories costs nothing.
pub async fn recall_as_context(store: &Store, query: &str, limit: usize) -> String {
    let memories = find_relevant(store, query, limit).await;
    if memories.is_empty() {
        return String::new();
    }
    let mut lines = vec![
        "## Relevant memories from previous sessions".to_string(),
        String::new(),
    ];
    for memory in &memories {
        lines.push(format!("### {}: {}", memory.kind.label(), memory.name));
        lines.push(memory.content.clone());
        lines.push(String::new());
    }
    registry().bump(CapabilityId::Memory, "recalls", 1);
    lines.join("\n")
}

/// The panel's view of the store.
pub fn describe(store: &Store, min_hours: f64, min_sessions: usize) -> Value {
    let headers = store.list_headers();
    let mut by_type: BTreeMap<&str, usize> = BTreeMap::new();
    for header in &headers {
        *by_type.entry(header.kind.as_str()).or_insert(0) += 1;
    }
    let gate = gate(store, min_hours, min_sessions);
    serde_json::json!({
        "root": store.root().display().to_string(),
        "count": headers.len(),
        "byType": by_type,
        "gate": gate,
        "memories": headers.iter().take(50).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> Store {
        Store::new(std::env::temp_dir().join(format!(
            "vibedesk-dream-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        )))
    }

    #[test]
    fn a_fenced_json_reply_is_read() {
        let actions = parse_actions(
            "```json\n{\"create\":[{\"name\":\"A\",\"description\":\"d\",\"type\":\"project\",\"content\":\"c\"}],\"update\":[],\"delete\":[],\"summary\":\"done\"}\n```",
        );
        assert_eq!(actions.create.len(), 1);
        assert_eq!(actions.create[0].name, "A");
        assert_eq!(actions.summary, "done");
    }

    #[test]
    fn a_reply_wrapped_in_prose_is_still_read() {
        let actions = parse_actions(
            "Here is what I did:\n{\"create\":[],\"update\":[],\"delete\":[],\"summary\":\"nothing\"}\nHope that helps.",
        );
        assert_eq!(actions.summary, "nothing");
    }

    #[test]
    fn an_unreadable_reply_asks_for_nothing_rather_than_guessing() {
        let actions = parse_actions("I could not do that.");
        assert!(actions.create.is_empty());
        assert!(actions.update.is_empty());
        assert!(actions.delete.is_empty());
    }

    #[test]
    fn an_id_array_is_read_out_of_a_fenced_reply() {
        assert_eq!(
            parse_ids("```json\n[\"a\", \"b\"]\n```"),
            vec!["a".to_string(), "b".to_string()]
        );
        assert!(parse_ids("none").is_empty());
    }

    #[test]
    fn the_gate_needs_both_new_sessions_and_distance() {
        let store = temp_store();
        let first = gate(&store, 24.0, 2);
        assert!(!first.ready);
        assert!(first.reason.contains("0 session(s)"));

        store.log_session("s1").unwrap();
        store.log_session("s2").unwrap();
        let second = gate(&store, 24.0, 2);
        assert!(second.ready, "{}", second.reason);

        // A pass just ran, so the distance rule now holds it back.
        store
            .set_last_consolidated_at(chrono::Utc::now().timestamp_millis())
            .unwrap();
        let third = gate(&store, 24.0, 2);
        assert!(!third.ready);
        assert!(third.reason.contains("session(s) since"));
        store.clear().unwrap();
    }

    #[test]
    fn a_store_with_no_lock_is_ready_as_soon_as_it_has_sessions() {
        let store = temp_store();
        store.log_session("s1").unwrap();
        let ready = gate(&store, 24.0, 1);
        assert!(ready.ready);
        assert!(ready.hours_since.is_infinite());
        store.clear().unwrap();
    }

    #[test]
    fn the_panel_view_counts_memories_by_type() {
        let store = temp_store();
        let stamp = chrono::Utc::now().timestamp_millis();
        for (id, kind) in [
            ("a", MemoryType::User),
            ("b", MemoryType::Project),
            ("c", MemoryType::Project),
        ] {
            store
                .set(&Memory {
                    id: id.into(),
                    name: id.into(),
                    description: "d".into(),
                    content: "c".into(),
                    kind,
                    created_at: stamp,
                    updated_at: stamp,
                    source: None,
                })
                .unwrap();
        }
        let view = describe(&store, 24.0, 2);
        assert_eq!(view["count"], 3);
        assert_eq!(view["byType"]["project"], 2);
        assert_eq!(view["byType"]["user"], 1);
        store.clear().unwrap();
    }

    #[test]
    fn a_secret_is_never_returned_when_it_is_not_set() {
        assert!(secret("VIBEDESK_DEFINITELY_NOT_SET_ANYWHERE").is_none());
    }
}
