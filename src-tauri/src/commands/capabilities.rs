//! The capability layer as the webview sees it.
//!
//! One snapshot command rather than one per capability: the panel renders a
//! ledger, and a ledger that arrives in six round trips can disagree with
//! itself. Everything else here is an action on that ledger.

use serde_json::{json, Value};

use crate::capabilities::{
    self, codegraph, external, memory, provider, proxy, settings, CapabilityId, CapabilityRecord,
};
use crate::error::{AppError, AppResult};

/// What the code graph holds, for the panel.
///
/// The query spawns the server and speaks MCP to it, which is blocking work, so
/// it runs off the async runtime. `refresh` bypasses the short cache the panel
/// polls through.
#[tauri::command]
pub async fn capabilities_codegraph(
    state: tauri::State<'_, crate::AppState>,
    refresh: Option<bool>,
) -> AppResult<codegraph::CodeGraphState> {
    let cwd = state.bridge.cwd().await;
    let refresh = refresh.unwrap_or(false);

    match tauri::async_runtime::spawn_blocking(move || {
        codegraph::cached_state(cwd.as_deref(), refresh)
    })
    .await
    {
        Ok(state) => Ok(state),
        Err(error) => Ok(codegraph::CodeGraphState::unavailable(
            format!("the code graph query failed: {error}"),
            None,
        )),
    }
}

/// One project's whole graph, for the renderer.
///
/// Reading it is a dozen paged round trips to the server, so it is cached apart
/// from the counts and refreshed on demand. An unreachable server is an error
/// here rather than an empty graph: an empty canvas and a missing server look
/// identical and mean opposite things.
#[tauri::command]
pub async fn capabilities_codegraph_graph(
    state: tauri::State<'_, crate::AppState>,
    project: String,
    refresh: Option<bool>,
) -> AppResult<codegraph::CodeGraphGraph> {
    let cwd = state.bridge.cwd().await;
    let refresh = refresh.unwrap_or(false);

    tauri::async_runtime::spawn_blocking(move || {
        codegraph::cached_graph(&project, cwd.as_deref(), refresh)
    })
    .await
    .map_err(|error| AppError::message(format!("the code graph query failed: {error}")))?
}

/// The whole ledger: what each capability is, whether it is on, and what it has
/// saved.
#[tauri::command]
pub fn capabilities_snapshot() -> Vec<CapabilityRecord> {
    capabilities::registry().snapshot()
}

/// Whether the channeler is listening, and what it has open.
///
/// The channeler is the app's own loopback server rather than an external
/// binary, so it has no path, no version and nothing to register — the panel
/// asks a different question of it than it asks of the others.
#[tauri::command]
pub fn capabilities_channel_status() -> Value {
    crate::channel::mcp::status()
}

/// Turn a capability on or off.
///
/// Three capabilities live at the proxy boundary — compaction, reading and
/// terminal — so the proxy and the provider it registers are managed together
/// and follow "is any of them on", not "is compaction on". The rest are a flag
/// the boundary reads.
#[tauri::command]
pub async fn capabilities_set_enabled(id: String, enabled: bool) -> AppResult<Vec<CapabilityRecord>> {
    let capability = CapabilityId::from_key(&id)
        .ok_or_else(|| AppError::message(format!("{id} is not a capability")))?;

    capabilities::registry().set_enabled(capability, enabled);
    let mut current = settings::load();
    current.enabled.insert(id, enabled);
    settings::save(&current).map_err(AppError::message)?;

    if proxy::needed() {
        let port = proxy::ensure_started().await?;
        let registration = provider::enable(port)?;
        let mut current = settings::load();
        current.compaction.upstream_provider = registration.upstream_provider;
        current.compaction.proxy_port = port;
        settings::save(&current).map_err(AppError::message)?;
    } else {
        provider::disable()?;
    }

    Ok(capabilities::registry().snapshot())
}

#[tauri::command]
pub fn capabilities_settings_get() -> settings::CapabilitySettings {
    settings::load()
}

#[tauri::command]
pub fn capabilities_settings_set(
    next: settings::CapabilitySettings,
) -> AppResult<settings::CapabilitySettings> {
    settings::save(&next).map_err(AppError::message)?;
    capabilities::registry().load(&next);
    Ok(settings::load())
}

#[tauri::command]
pub fn capabilities_reset(id: String) -> AppResult<Vec<CapabilityRecord>> {
    let capability = CapabilityId::from_key(&id)
        .ok_or_else(|| AppError::message(format!("{id} is not a capability")))?;
    capabilities::registry().reset(capability);
    Ok(capabilities::registry().snapshot())
}

/// What the compaction panel needs: the proxy, the provider, and every
/// conversation the kernel is holding.
#[tauri::command]
pub fn capabilities_compaction_status() -> Value {
    let settings = settings::load().compaction;
    json!({
        "port": proxy::port(),
        "listening": proxy::port().is_some(),
        "upstreamProvider": settings.upstream_provider,
        "registration": provider::describe(),
        "sessions": proxy::statuses(),
        "config": {
            "contextLimit": settings.context_limit,
            "nudgeThreshold": settings.nudge_threshold,
            "truncateThreshold": settings.truncate_threshold,
            "preserveRecent": settings.preserve_recent,
            "ccr": settings.ccr,
            "ccrMinTokens": settings.ccr_min_tokens,
            "minCompressRange": settings.min_compress_range,
            "minDistillRange": settings.min_distill_range,
        },
    })
}

/// What the memory panel needs: the store, the gate, and the memories.
#[tauri::command]
pub fn capabilities_memory_status() -> Value {
    let settings = settings::load().memory;
    let store = memory::store::Store::default();
    memory::describe(&store, 24.0, settings.min_sessions as usize)
}

/// Run a consolidation pass now, over the sessions the caller hands in.
#[tauri::command]
pub async fn capabilities_memory_dream(sessions: Vec<memory::Session>) -> memory::DreamResult {
    let settings = settings::load().memory;
    let store = memory::store::Store::default();
    for session in &sessions {
        let _ = store.log_session(&session.id);
    }
    memory::dream(&store, &sessions, settings.max_memories as usize).await
}

#[tauri::command]
pub fn capabilities_memory_forget(id: String) -> AppResult<Value> {
    let store = memory::store::Store::default();
    store.delete(&id)?;
    Ok(capabilities_memory_status())
}

/// The two native servers, with whether they are installed and registered.
#[tauri::command]
pub fn capabilities_external_status() -> Vec<external::ServerStatus> {
    external::status()
}

#[tauri::command]
pub fn capabilities_external_register(name: String) -> AppResult<Vec<external::ServerStatus>> {
    external::register(&name)
}

#[tauri::command]
pub fn capabilities_external_unregister(name: String) -> AppResult<Vec<external::ServerStatus>> {
    external::unregister(&name)
}

/// The terminal rule families, so the panel can show what is actually loaded
/// rather than a count it made up.
#[tauri::command]
pub fn capabilities_terminal_rules() -> Value {
    let rules = capabilities::tokenjuice::rules::all();
    let mut families: Vec<Value> = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    for rule in rules {
        let family = rule.rule.family.as_str();
        if seen.contains(&family) {
            continue;
        }
        seen.push(family);
        families.push(json!({
            "family": family,
            "id": rule.rule.id,
            "description": rule.rule.description,
        }));
    }
    json!({
        "count": rules.len(),
        "families": families,
        "fallback": capabilities::tokenjuice::rules::FALLBACK_RULE,
    })
}

/// Run the reading gate on a path without reading it into the agent's context.
///
/// This is what makes the reading panel more than a counter: the user can point
/// it at a file and see the decision the gate would make, and the card the agent
/// would get, before a session ever touches it.
#[tauri::command]
pub fn capabilities_reading_probe(path: String) -> AppResult<Value> {
    let path = std::path::PathBuf::from(&path);
    let content = std::fs::read_to_string(&path).map_err(|error| {
        AppError::message(format!("could not read {}: {error}", path.display()))
    })?;
    // The probe shows the card the agent would receive, so it names the tool
    // the agent actually has. That name is learned at the proxy boundary from a
    // real request; before one has been seen, `read_file` is Vibe's own.
    let tool = capabilities::sparse::ReadTool::agent(&capabilities::registry().read_tool());
    let outcome = capabilities::sparse::apply_with(&path, content, false, &tool);
    // The card's whole purpose is that it tells the agent how to get the rest of
    // the file. `next_actions` is a method rather than a field, so it has to be
    // asked for explicitly or the panel would show a map with no roads on it.
    let next_actions = outcome.card.as_ref().map(|card| card.next_actions());
    Ok(json!({
        "path": path.display().to_string(),
        "decision": outcome.decision,
        "intercepted": outcome.intercepted,
        "rawChars": outcome.raw_chars,
        "outChars": outcome.out_chars,
        "denoiseRules": outcome.denoise_rules,
        "card": outcome.card,
        "nextActions": next_actions,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_capability_key_is_refused_by_name() {
        let error = CapabilityId::from_key("teleportation");
        assert!(error.is_none());
    }

    #[test]
    fn the_terminal_rule_list_is_not_empty_and_names_its_fallback() {
        let view = capabilities_terminal_rules();
        assert!(view["count"].as_u64().unwrap() > 100);
        assert_eq!(view["fallback"], capabilities::tokenjuice::rules::FALLBACK_RULE);
        assert!(!view["families"].as_array().unwrap().is_empty());
    }

    #[test]
    fn probing_a_missing_file_reports_the_path_rather_than_panicking() {
        let error = capabilities_reading_probe("/definitely/not/here.txt".into()).unwrap_err();
        assert!(error.to_string().contains("/definitely/not/here.txt"));
    }

    /// The card exists to tell the agent how to get the rest of the file, and
    /// `next_actions` is a method rather than a field — so it is the one part of
    /// the card that a naive serialisation would silently drop. The panel would
    /// then show a map with no roads on it.
    #[test]
    fn a_probed_document_carries_the_calls_that_get_the_rest_of_it() {
        let dir = std::env::temp_dir().join("vibedesk-capability-probe");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("spec.md");
        let body = format!("# Spec\n\n{}", "paragraph text here\n\n".repeat(900));
        std::fs::write(&path, &body).unwrap();

        let view = capabilities_reading_probe(path.display().to_string()).unwrap();
        assert_eq!(view["intercepted"], true);
        assert!(view["card"].is_object());

        let actions = view["nextActions"].as_array().expect("next actions were sent");
        assert!(!actions.is_empty());
        // The probe shows the card the agent would receive, so the calls name
        // the agent's own read tool — not the ACP method it never calls.
        assert!(actions
            .iter()
            .all(|action| action.as_str().unwrap().contains("read_file")));
        assert!(actions
            .iter()
            .all(|action| !action.as_str().unwrap().contains("fs/read_text_file")));
    }
}
