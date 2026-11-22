use std::path::PathBuf;

use tauri::State;

use crate::acp::bridge::AgentStatus;
use crate::error::{AppError, AppResult};
use crate::AppState;

#[tauri::command]
pub async fn agent_status(state: State<'_, AppState>) -> AppResult<AgentStatus> {
    Ok(state.bridge.status().await)
}

#[tauri::command]
pub async fn agent_start(
    state: State<'_, AppState>,
    cwd: Option<String>,
) -> AppResult<AgentStatus> {
    state.bridge.start(cwd.map(PathBuf::from)).await
}

/// Switch the active workstation. The agent process keeps running: ACP
/// sessions carry their own cwd, so only the bridge's view has to move.
#[tauri::command]
pub async fn agent_set_workspace(
    state: State<'_, AppState>,
    cwd: String,
) -> AppResult<()> {
    let path = PathBuf::from(&cwd);
    if !path.is_dir() {
        return Err(AppError::message(format!("{cwd} is not a folder")));
    }
    state.bridge.set_cwd(path).await;
    Ok(())
}

#[tauri::command]
pub async fn agent_stop(state: State<'_, AppState>) -> AppResult<()> {
    state.bridge.stop().await
}

#[tauri::command]
pub async fn agent_restart(
    state: State<'_, AppState>,
    cwd: Option<String>,
) -> AppResult<AgentStatus> {
    state.bridge.restart(cwd.map(PathBuf::from)).await
}
