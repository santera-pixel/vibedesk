//! Provisioning as the webview sees it.
//!
//! Three commands: read the ledger, run everything, run one step. The panel
//! polls `provision_status` while `provision_run` is in flight, so the ledger is
//! the only state that crosses the boundary — the run itself is fire-and-forget
//! from the renderer's point of view.

use serde_json::Value;

use crate::error::{AppError, AppResult};
use crate::provision::{self, Step};

/// Every step, with its ledger state and a live probe.
#[tauri::command]
pub fn provision_status() -> Value {
    provision::status()
}

/// Run every step in order. Returns the final ledger.
#[tauri::command]
pub async fn provision_run() -> AppResult<Value> {
    let records = provision::run().await;
    Ok(serde_json::json!({ "records": records }))
}

/// Run one step, for the retry button on a failed row.
#[tauri::command]
pub async fn provision_run_step(id: String) -> AppResult<Value> {
    let step = Step::ALL
        .into_iter()
        .find(|step| step.id() == id)
        .ok_or_else(|| AppError::message(format!("no provisioning step named `{id}`")))?;
    let record = provision::run_step(step).await;
    Ok(serde_json::json!({ "record": record }))
}
