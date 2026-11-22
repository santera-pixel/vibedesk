use tauri::State;

use crate::error::AppResult;
use crate::update::{self, UpdateStatus};
use crate::AppState;

#[tauri::command]
pub async fn update_check(force: bool) -> AppResult<UpdateStatus> {
    Ok(update::check(force).await)
}

/// Stop the agent, run the installer, then report the new version.
///
/// The agent holds an open handle on `vibe-acp.exe`; on Windows the installer
/// cannot replace a running binary, so the process has to go first.
#[tauri::command]
pub async fn update_apply(state: State<'_, AppState>) -> AppResult<UpdateStatus> {
    let _ = state.bridge.stop().await;
    Ok(update::apply())
}
