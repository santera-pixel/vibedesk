//! Commands for the panel data plane.
//!
//! Two things the UI needs and cannot derive: what is running, and how to stop
//! it. Everything else arrives as `panel://data` events.

use serde::Serialize;
use tauri::{AppHandle, State};

use crate::data::{self, SourceStatus};
use crate::error::{AppError, AppResult};
use crate::AppState;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DataPlaneStatus {
    /// True when the user has stopped every source. Nothing starts until this
    /// is false again, including after a config edit.
    pub paused: bool,
    /// True when `VIBEDESK_SAFE_MODE` is set. Sources cannot be started at all.
    pub safe_mode: bool,
    pub sources: Vec<SourceStatus>,
    pub limits: DataLimits,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DataLimits {
    pub max_sources_per_panel: usize,
    pub max_body_bytes: usize,
    pub max_json_depth: usize,
    pub max_json_nodes: usize,
    pub max_concurrent: usize,
    pub interval_min: u64,
    pub interval_max: u64,
    pub timeout_min: u64,
    pub timeout_max: u64,
}

#[tauri::command]
pub fn data_status(state: State<'_, AppState>) -> DataPlaneStatus {
    DataPlaneStatus {
        paused: state.data.paused(),
        safe_mode: crate::customize::safe_mode(),
        sources: state.data.status(),
        limits: DataLimits {
            max_sources_per_panel: data::MAX_SOURCES_PER_PANEL,
            max_body_bytes: data::MAX_BODY_BYTES,
            max_json_depth: data::MAX_JSON_DEPTH,
            max_json_nodes: data::MAX_JSON_NODES,
            max_concurrent: data::MAX_CONCURRENT,
            interval_min: data::INTERVAL_MS.0,
            interval_max: data::INTERVAL_MS.1,
            timeout_min: data::TIMEOUT_MS.0,
            timeout_max: data::TIMEOUT_MS.1,
        },
    }
}

/// The kill switch.
///
/// Pausing stops every task and refuses to start new ones, so a config edit
/// cannot quietly resume a source the user has stopped. Resuming re-reads the
/// panels rather than trusting a cached list, because the file may have changed
/// while the plane was paused.
#[tauri::command]
pub async fn data_set_paused(
    app: AppHandle,
    state: State<'_, AppState>,
    paused: bool,
) -> AppResult<DataPlaneStatus> {
    let panels = super::customize::live_panels(&state).await?;
    state.data.set_paused(&app, paused, &panels.panels);
    state
        .bridge
        .log(
            "info",
            if paused {
                "panel sources paused"
            } else {
                "panel sources resumed"
            },
        )
        .await;
    Ok(data_status(state))
}

/// One origin the current config wants, and what the user has said about it.
///
/// `data::split_by_consent` consults the ledger on every reconcile, so an
/// origin with no decision is a source that is declared and deliberately not
/// fetched. These two commands are how the user answers that question; without
/// them the ledger could only ever be empty and every `http` source would wait
/// forever.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsentEntry {
    pub origin: String,
    /// `allow`, `deny`, or `null` when the user has not been asked.
    pub decision: Option<String>,
    /// Panel ids that read this origin, deduplicated and sorted.
    pub panels: Vec<String>,
    /// Source ids that read this origin, deduplicated and sorted.
    pub sources: Vec<String>,
    /// True when at least one source on this origin is actually running.
    pub running: bool,
}

/// Every origin the config wants, with its decision.
///
/// Derived from the plane's own status rather than from the panel file, so the
/// list is what the app is *doing*, not what the file *says*. A source that
/// failed to parse never appears here, which is correct: it is not asking.
#[tauri::command]
pub fn data_consent_list(state: State<'_, AppState>) -> Vec<ConsentEntry> {
    let ledger = crate::consent::load();
    let mut by_origin: std::collections::BTreeMap<String, ConsentEntry> =
        std::collections::BTreeMap::new();

    for source in state.data.status() {
        let entry = by_origin
            .entry(source.origin.clone())
            .or_insert_with(|| ConsentEntry {
                origin: source.origin.clone(),
                decision: ledger.decision(&source.origin).map(|d| d.token().to_string()),
                panels: Vec::new(),
                sources: Vec::new(),
                running: false,
            });
        if !entry.panels.contains(&source.panel_id) {
            entry.panels.push(source.panel_id.clone());
        }
        if !entry.sources.contains(&source.source_id) {
            entry.sources.push(source.source_id.clone());
        }
        entry.running |= source.running;
    }

    for entry in by_origin.values_mut() {
        entry.panels.sort();
        entry.sources.sort();
    }

    by_origin.into_values().collect()
}

/// Grant, refuse, or forget an origin.
///
/// `decision` is `allow`, `deny`, or `None` to put the origin back to asking.
/// The write goes through `consent::decide`, which refuses anything that is not
/// a loopback origin — so this command cannot be used to widen the network
/// reach of a panel, only to answer the question the panel already asked.
#[tauri::command]
pub async fn data_consent_set(
    app: AppHandle,
    state: State<'_, AppState>,
    origin: String,
    decision: Option<String>,
) -> AppResult<DataPlaneStatus> {
    let parsed = match decision.as_deref() {
        None => None,
        Some(token) => Some(
            crate::consent::Decision::parse(token)
                .ok_or_else(|| AppError::message(format!("`{token}` is not allow or deny")))?,
        ),
    };

    crate::consent::decide(&origin, parsed).map_err(AppError::message)?;

    // Re-read the panels rather than trusting the cached set: the file may have
    // changed while the origin was waiting.
    let panels = super::customize::live_panels(&state).await?;
    state.data.reconcile(&app, &panels.panels);

    state
        .bridge
        .log(
            "info",
            match parsed {
                Some(crate::consent::Decision::Allow) => format!("consent granted for {origin}"),
                Some(crate::consent::Decision::Deny) => format!("consent refused for {origin}"),
                None => format!("consent cleared for {origin}"),
            },
        )
        .await;

    Ok(data_status(state))
}
