use std::path::PathBuf;

use serde::Serialize;
use tauri::State;

use crate::actions::{self, ActionsConfig};
use crate::customize::{self, ThemeConfig};
use crate::error::{AppError, AppResult};
use crate::layout::{self, LayoutConfig};
use crate::panels::{self, PanelsConfig};
use crate::plugin_panels;
use crate::paths;
use crate::sources::{self, ConfigKind, PendingState, PluginSource};
use crate::AppState;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomizePaths {
    pub config_dir: String,
    pub pending_dir: String,
    pub theme_path: String,
    pub layout_path: String,
    pub actions_path: String,
    pub panels_path: String,
    pub manifest_path: String,
    pub safe_mode: bool,
}

#[tauri::command]
pub fn customize_paths() -> CustomizePaths {
    CustomizePaths {
        config_dir: customize::config_dir().display().to_string(),
        pending_dir: sources::pending_dir().display().to_string(),
        theme_path: customize::theme_path().display().to_string(),
        layout_path: layout::layout_path().display().to_string(),
        actions_path: actions::actions_path().display().to_string(),
        panels_path: panels::panels_path().display().to_string(),
        manifest_path: crate::manifest::manifest_path().display().to_string(),
        safe_mode: customize::safe_mode(),
    }
}

#[tauri::command]
pub fn theme_get() -> AppResult<ThemeConfig> {
    customize::load_theme().map_err(AppError::message)
}

#[tauri::command]
pub async fn theme_set(state: State<'_, AppState>, theme: ThemeConfig) -> AppResult<ThemeConfig> {
    let saved = customize::save_theme(&theme).map_err(AppError::message)?;
    state.bridge.log("info", "theme.json written").await;
    Ok(saved)
}

#[tauri::command]
pub async fn theme_reset(state: State<'_, AppState>) -> AppResult<ThemeConfig> {
    let cleared = customize::reset_theme().map_err(AppError::message)?;
    state.bridge.log("info", "theme.json removed").await;
    Ok(cleared)
}

#[tauri::command]
pub fn layout_get() -> AppResult<LayoutConfig> {
    layout::load_layout().map_err(AppError::message)
}

/// Called on every drag release, so it stays cheap: clamp, write, return the
/// effective values. The frontend uses the return value rather than its own
/// optimistic state, which is how a clamped width becomes visible.
#[tauri::command]
pub async fn layout_set(
    state: State<'_, AppState>,
    layout: LayoutConfig,
) -> AppResult<LayoutConfig> {
    let saved = layout::save_layout(&layout).map_err(AppError::message)?;
    state.bridge.log("info", "layout.json written").await;
    Ok(saved)
}

#[tauri::command]
pub async fn layout_reset(state: State<'_, AppState>) -> AppResult<LayoutConfig> {
    let cleared = layout::reset_layout().map_err(AppError::message)?;
    state.bridge.log("info", "layout.json removed").await;
    Ok(cleared)
}

#[tauri::command]
pub fn actions_get() -> AppResult<ActionsConfig> {
    actions::load_actions().map_err(AppError::message)
}

#[tauri::command]
pub async fn actions_set(
    state: State<'_, AppState>,
    actions: ActionsConfig,
) -> AppResult<ActionsConfig> {
    let saved = actions::save_actions(&actions).map_err(AppError::message)?;
    state.bridge.log("info", "actions.json written").await;
    Ok(saved)
}

#[tauri::command]
pub async fn actions_reset(state: State<'_, AppState>) -> AppResult<ActionsConfig> {
    let cleared = actions::reset_actions().map_err(AppError::message)?;
    state.bridge.log("info", "actions.json removed").await;
    Ok(cleared)
}

/// Every panel that is live: the user's, then the ones the enabled plugins
/// contribute.
///
/// Async because a plugin panel lives in the workspace's plugin folder, and the
/// workspace is the agent's current directory. The data plane reads through
/// this too, so what runs is the same set the rail lists.
pub async fn live_panels(state: &AppState) -> AppResult<PanelsConfig> {
    let cwd = workspace(state).await;
    let user = panels::load_panels().map_err(AppError::message)?;
    Ok(plugin_panels::merged(Some(&cwd), user))
}

#[tauri::command]
pub async fn panels_get(state: State<'_, AppState>) -> AppResult<PanelsConfig> {
    live_panels(&state).await
}

/// Write the user's panels, then answer with the merged view.
///
/// The editor saves the whole list it was shown, so the plugin panels are
/// filtered out first: persisting one would put an id in the user's file that
/// `validate` refuses on the next load.
#[tauri::command]
pub async fn panels_set(
    state: State<'_, AppState>,
    panels: PanelsConfig,
) -> AppResult<PanelsConfig> {
    let cwd = workspace(&state).await;
    let saved =
        panels::save_panels(&plugin_panels::user_only(&panels)).map_err(AppError::message)?;
    state.bridge.log("info", "panels.json written").await;
    Ok(plugin_panels::merged(Some(&cwd), saved))
}

#[tauri::command]
pub async fn panels_reset(state: State<'_, AppState>) -> AppResult<PanelsConfig> {
    let cwd = workspace(&state).await;
    let cleared = panels::reset_panels().map_err(AppError::message)?;
    state.bridge.log("info", "panels.json removed").await;
    Ok(plugin_panels::merged(Some(&cwd), cleared))
}

/// Why a plugin's panels did not load. Small on purpose: the reasons are shown
/// next to the panels that did.
#[tauri::command]
pub async fn panels_plugin_issues(state: State<'_, AppState>) -> AppResult<Vec<String>> {
    let cwd = workspace(&state).await;
    Ok(plugin_panels::issues(Some(&cwd)))
}

/* ------------------------------------------------------------------ *
 * Sources
 *
 * Two ways a customization can arrive from somewhere other than the user's
 * own editor: a plugin ships one, or the agent writes a proposal. Neither is
 * applied by being present. Both land here as a listing, and the user picks.
 * ------------------------------------------------------------------ */

async fn workspace(state: &AppState) -> PathBuf {
    state
        .bridge
        .cwd()
        .await
        .unwrap_or_else(paths::default_workspace)
}

#[tauri::command]
pub fn sources_pending() -> PendingState {
    sources::read_pending()
}

/// Promote a proposal the user has seen. `digest` is what the UI displayed; a
/// mismatch means the file moved underneath it and the honest answer is to make
/// the user look again.
#[tauri::command]
pub async fn sources_apply(
    state: State<'_, AppState>,
    kind: ConfigKind,
    digest: String,
) -> AppResult<()> {
    sources::apply_pending(kind, &digest).map_err(AppError::message)?;
    state
        .bridge
        .log("info", &format!("applied the agent's {}", kind.label()))
        .await;
    Ok(())
}

#[tauri::command]
pub async fn sources_reject(state: State<'_, AppState>, kind: ConfigKind) -> AppResult<()> {
    sources::reject_pending(kind).map_err(AppError::message)?;
    state
        .bridge
        .log("info", &format!("rejected the agent's {}", kind.label()))
        .await;
    Ok(())
}

#[tauri::command]
pub async fn sources_plugins(state: State<'_, AppState>) -> AppResult<Vec<PluginSource>> {
    let cwd = workspace(&state).await;
    Ok(sources::discover_plugin_sources(Some(&cwd)))
}

/// The frontend names a plugin and a kind, never a path: the backend
/// re-discovers, so the webview cannot hand in an arbitrary file to be read and
/// promoted into the live config.
#[tauri::command]
pub async fn sources_apply_plugin(
    state: State<'_, AppState>,
    plugin: String,
    kind: ConfigKind,
    digest: String,
) -> AppResult<()> {
    let cwd = workspace(&state).await;
    sources::apply_plugin_source(Some(&cwd), &plugin, kind, &digest).map_err(AppError::message)?;
    state
        .bridge
        .log(
            "info",
            &format!("applied {plugin}'s {}", kind.label()),
        )
        .await;
    Ok(())
}
