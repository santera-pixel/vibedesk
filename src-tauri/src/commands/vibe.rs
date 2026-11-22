use std::path::PathBuf;

use serde_json::{json, Value};
use tauri::State;

use crate::billing;
use crate::config;
use crate::error::AppResult;
use crate::install::{self, InstallOutcome, InstallStatus};
use crate::paths;
use crate::plugins::{self, PluginRecord, PluginReloadReport};
use crate::release_notes::{self, ReleaseNote};
use crate::skills::{self, SkillRecord};
use crate::update;
use crate::AppState;

async fn workspace(state: &AppState) -> PathBuf {
    state
        .bridge
        .cwd()
        .await
        .unwrap_or_else(paths::default_workspace)
}

#[tauri::command]
pub async fn vibe_environment(state: State<'_, AppState>) -> AppResult<config::VibeEnvironment> {
    let cwd = workspace(&state).await;
    let mut environment = config::build_environment(Some(&cwd));
    environment.cli_version = update::current_version();
    Ok(environment)
}

#[tauri::command]
pub async fn vibe_config_schema(state: State<'_, AppState>) -> AppResult<Value> {
    state.bridge.request("_config/schema", json!({})).await
}

#[tauri::command]
pub async fn vibe_auth_status(state: State<'_, AppState>) -> AppResult<Value> {
    state.bridge.request("_auth/status", json!({})).await
}

#[tauri::command]
pub async fn vibe_whoami(state: State<'_, AppState>) -> AppResult<Value> {
    let session_id = state
        .bridge
        .status()
        .await
        .session_id
        .unwrap_or_default();

    let identity = state
        .bridge
        .request("_identity/read", json!({ "sessionId": session_id }))
        .await
        .unwrap_or(Value::Null);

    let account = state
        .bridge
        .request("_account/read", json!({ "sessionId": session_id }))
        .await
        .unwrap_or(Value::Null);

    Ok(json!({ "identity": identity, "account": account }))
}

/// The billing window's data: plan, allowance and identity in one round trip.
///
/// Three extension methods feed it, and any of them may be missing on an older
/// agent — a failed read becomes `null` and the panel says "not reported"
/// rather than showing an error for a field the agent never had.
#[tauri::command]
pub async fn vibe_account(state: State<'_, AppState>) -> AppResult<billing::AccountSnapshot> {
    let session_id = state
        .bridge
        .status()
        .await
        .session_id
        .unwrap_or_default();

    let read = |method: &'static str| {
        let bridge = state.bridge.clone();
        let session_id = session_id.clone();
        async move {
            bridge
                .request(method, json!({ "sessionId": session_id }))
                .await
                .unwrap_or(Value::Null)
        }
    };

    let (account, identity, auth) = tokio::join!(
        read("_account/read"),
        read("_identity/read"),
        read("_auth/status"),
    );

    Ok(billing::parse(&account, &identity, &auth))
}

#[tauri::command]
pub async fn vibe_log_level(
    state: State<'_, AppState>,
    session_id: String,
    level: String,
) -> AppResult<Value> {
    state
        .bridge
        .request(
            "_logLevel/write",
            json!({ "sessionId": session_id, "sessionOverride": level }),
        )
        .await
}

#[tauri::command]
pub async fn vibe_release_notes() -> AppResult<Vec<ReleaseNote>> {
    Ok(release_notes::load(40))
}

/// First-run detection: is `vibe-acp` on this machine, and if not, what would
/// we install it with?
#[tauri::command]
pub async fn vibe_install_status() -> AppResult<InstallStatus> {
    Ok(install::status())
}

/// Run the detected install command. The agent is stopped first so the
/// installer can replace the binary on Windows.
#[tauri::command]
pub async fn vibe_install(state: State<'_, AppState>) -> AppResult<InstallOutcome> {
    let _ = state.bridge.stop().await;
    install::run().await
}

#[tauri::command]
pub async fn vibe_skills(state: State<'_, AppState>) -> AppResult<Vec<SkillRecord>> {
    let cwd = workspace(&state).await;
    Ok(skills::discover_all(Some(&cwd)))
}

#[tauri::command]
pub async fn plugins_list(state: State<'_, AppState>) -> AppResult<Vec<PluginRecord>> {
    let cwd = workspace(&state).await;
    let records = plugins::discover(Some(&cwd));
    if let Ok(mut cache) = state.plugins.lock() {
        *cache = records.clone();
    }
    Ok(records)
}

/// Re-scan the plugin directories and, when something changed, open a fresh
/// session so the agent re-reads skills, MCP servers and hooks.
///
/// Restarting the session is the only honest way to re-pin: the agent resolves
/// plugins once at session creation, so a hot swap without a new session would
/// report success while the running turn still used the old snapshot.
#[tauri::command]
pub async fn plugins_reload(
    state: State<'_, AppState>,
    repin: bool,
) -> AppResult<PluginReloadReport> {
    let cwd = workspace(&state).await;
    let previous = state
        .plugins
        .lock()
        .map(|cache| cache.clone())
        .unwrap_or_default();

    let mut report = plugins::reload(Some(&cwd), &previous);
    let changed = !report.added.is_empty() || !report.removed.is_empty() || !report.updated.is_empty();

    if repin && changed {
        let cwd_string = cwd.display().to_string();
        if let Ok(result) = state
            .bridge
            .request(
                "session/new",
                json!({ "cwd": cwd_string, "mcpServers": [] }),
            )
            .await
        {
            if let Some(session_id) = result.get("sessionId").and_then(Value::as_str) {
                state.bridge.set_session(Some(session_id.to_string())).await;
                report.repinned = true;
            }
        }
    }

    if let Ok(mut cache) = state.plugins.lock() {
        *cache = report.plugins.clone();
    }
    Ok(report)
}

#[tauri::command]
pub async fn plugins_set_enabled(
    state: State<'_, AppState>,
    name: String,
    enabled: bool,
) -> AppResult<Vec<PluginRecord>> {
    let records = plugins::set_enabled(&name, enabled)?;
    if let Ok(mut cache) = state.plugins.lock() {
        *cache = records.clone();
    }
    Ok(records)
}
