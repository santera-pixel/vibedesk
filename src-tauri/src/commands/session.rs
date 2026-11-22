use serde::Serialize;
use serde_json::{json, Value};
use tauri::State;

use crate::error::{AppError, AppResult};
use crate::AppState;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrustState {
    pub trusted: bool,
    pub root: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSnapshot {
    pub session_id: String,
    pub modes: Option<Value>,
    pub config_options: Vec<Value>,
    pub trust: Option<TrustState>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    pub session_id: String,
    pub cwd: String,
    pub title: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptOutcome {
    pub stop_reason: String,
    pub usage: Option<Value>,
}

fn snapshot_from(result: &Value, trust: Option<TrustState>) -> AppResult<SessionSnapshot> {
    let session_id = result
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::message("the agent returned no sessionId"))?
        .to_string();

    let config_options = result
        .get("configOptions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    Ok(SessionSnapshot {
        session_id,
        modes: result.get("modes").cloned().filter(|v| !v.is_null()),
        config_options,
        trust,
    })
}

/// `_trust/status` answers `{ trust_status, details }`; the UI only needs to
/// know whether the folder is trusted and why.
async fn trust_for(state: &AppState, session_id: &str, cwd: &str) -> Option<TrustState> {
    let result = state
        .bridge
        .request(
            "_trust/status",
            json!({ "sessionId": session_id, "cwd": cwd }),
        )
        .await
        .ok()?;

    let status = result
        .get("trust_status")
        .and_then(Value::as_str)
        .unwrap_or("untrusted");

    let details = result.get("details");
    let root = details
        .and_then(|d| d.get("repoRoot").or_else(|| d.get("repo_root")))
        .and_then(Value::as_str)
        .map(str::to_string);

    Some(TrustState {
        trusted: status != "untrusted",
        root,
        reason: Some(match status {
            "trusted" => "Folder is trusted permanently.".into(),
            "session" => "Trusted for this session only.".into(),
            _ => "This folder is not trusted yet; the agent will ask before writing.".into(),
        }),
    })
}

#[tauri::command]
pub async fn session_new(state: State<'_, AppState>, cwd: String) -> AppResult<SessionSnapshot> {
    let result = state
        .bridge
        .request(
            "session/new",
            json!({
                "cwd": cwd.clone(),
                "mcpServers": crate::config::mcp_servers_for_session(Some(std::path::Path::new(&cwd))),
            }),
        )
        .await?;

    let trust = {
        let id = result
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        trust_for(&state, &id, &cwd).await
    };

    let snapshot = snapshot_from(&result, trust)?;
    state.bridge.set_session(Some(snapshot.session_id.clone())).await;
    crate::settings::remember_workspace(&std::path::PathBuf::from(&cwd));
    Ok(snapshot)
}

#[tauri::command]
pub async fn session_load(
    state: State<'_, AppState>,
    cwd: String,
    session_id: String,
) -> AppResult<SessionSnapshot> {
    // `session/load` replays the whole transcript as `session/update`
    // notifications, so a large chat can outlast any fixed deadline. Silence is
    // the only honest failure signal here too.
    let mcp_servers = crate::config::mcp_servers_for_session(Some(std::path::Path::new(&cwd)));
    let result = state
        .bridge
        .request_streaming(
            "session/load",
            json!({ "sessionId": session_id, "cwd": cwd, "mcpServers": mcp_servers }),
        )
        .await
        .map_err(AppError::explain)?;

    let trust = trust_for(&state, &session_id, &cwd).await;

    // `session/load` may answer with an empty object; the id we asked for is
    // authoritative in that case.
    let mut snapshot = if result.get("sessionId").is_some() {
        snapshot_from(&result, trust)?
    } else {
        SessionSnapshot {
            session_id: session_id.clone(),
            modes: None,
            config_options: Vec::new(),
            trust,
        }
    };
    snapshot.session_id = session_id;

    state.bridge.set_session(Some(snapshot.session_id.clone())).await;
    Ok(snapshot)
}

/// One image the user attached to a prompt.
///
/// ACP carries images as base64 content blocks, so the webview hands over the
/// bytes it already read from the clipboard or a drop and this forwards them
/// unchanged. The mime type is required because ACP has no default and a
/// mislabelled block is rejected by the agent rather than guessed at.
#[derive(Debug, Clone, serde::Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptImage {
    pub data: String,
    pub mime_type: String,
}

#[tauri::command]
pub async fn session_prompt(
    state: State<'_, AppState>,
    session_id: String,
    text: String,
    images: Option<Vec<PromptImage>>,
) -> AppResult<PromptOutcome> {
    // Text first, then the images, so a prompt that is only an image still
    // carries the user's words ahead of it when there are any.
    let mut prompt = Vec::with_capacity(1 + images.as_ref().map_or(0, Vec::len));
    if !text.is_empty() {
        prompt.push(json!({ "type": "text", "text": text }));
    }
    for image in images.unwrap_or_default() {
        prompt.push(json!({
            "type": "image",
            "data": image.data,
            "mimeType": image.mime_type,
        }));
    }
    if prompt.is_empty() {
        return Err(AppError::message("a prompt needs text or an image"));
    }

    // `turn` owns the busy flag end to end. It stays set when the agent goes
    // silent, because a silent agent is still a running agent.
    let result = state
        .bridge
        .turn(
            "session/prompt",
            json!({
                "sessionId": session_id,
                "prompt": prompt,
            }),
        )
        .await?;

    Ok(PromptOutcome {
        stop_reason: result
            .get("stopReason")
            .and_then(Value::as_str)
            .unwrap_or("end_turn")
            .to_string(),
        usage: result.get("usage").cloned().filter(|v| !v.is_null()),
    })
}

#[tauri::command]
pub async fn session_cancel(state: State<'_, AppState>, session_id: String) -> AppResult<()> {
    state
        .bridge
        .notify("session/cancel", json!({ "sessionId": session_id }))
        .await
}

#[tauri::command]
pub async fn session_list(
    state: State<'_, AppState>,
    cwd: Option<String>,
) -> AppResult<Vec<SessionSummary>> {
    let mut params = json!({});
    if let Some(cwd) = cwd {
        params["cwd"] = Value::String(cwd);
    }

    let result = state.bridge.request("session/list", params).await?;
    let sessions = result
        .get("sessions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    Ok(sessions
        .iter()
        .filter_map(|session| {
            Some(SessionSummary {
                session_id: session.get("sessionId")?.as_str()?.to_string(),
                cwd: session
                    .get("cwd")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                title: session
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("Untitled session")
                    .to_string(),
                updated_at: session
                    .get("updatedAt")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            })
        })
        .collect())
}

#[tauri::command]
pub async fn session_set_mode(
    state: State<'_, AppState>,
    session_id: String,
    mode_id: String,
) -> AppResult<()> {
    state
        .bridge
        .request(
            "session/set_mode",
            json!({ "sessionId": session_id, "modeId": mode_id }),
        )
        .await
        .map(|_| ())
}

#[tauri::command]
pub async fn session_set_config_option(
    state: State<'_, AppState>,
    session_id: String,
    config_id: String,
    value: Value,
) -> AppResult<()> {
    state
        .bridge
        .request(
            "session/set_config_option",
            json!({ "sessionId": session_id, "configId": config_id, "value": value }),
        )
        .await
        .map(|_| ())
}

#[tauri::command]
pub async fn session_set_title(
    state: State<'_, AppState>,
    session_id: String,
    title: String,
) -> AppResult<()> {
    state
        .bridge
        .request(
            "_session/set_title",
            json!({ "sessionId": session_id, "title": title }),
        )
        .await
        .map(|_| ())
}

#[tauri::command]
pub async fn session_fork(
    state: State<'_, AppState>,
    session_id: String,
) -> AppResult<SessionSnapshot> {
    let cwd = state
        .bridge
        .cwd()
        .await
        .map(|path| path.display().to_string())
        .unwrap_or_default();

    let mcp_servers = crate::config::mcp_servers_for_session(Some(std::path::Path::new(&cwd)));
    let result = state
        .bridge
        .request(
            "session/fork",
            json!({ "sessionId": session_id, "cwd": cwd, "mcpServers": mcp_servers }),
        )
        .await?;

    let trust = {
        let id = result
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        trust_for(&state, &id, &cwd).await
    };

    let snapshot = snapshot_from(&result, trust)?;
    state.bridge.set_session(Some(snapshot.session_id.clone())).await;
    Ok(snapshot)
}

#[tauri::command]
pub async fn respond_permission(
    state: State<'_, AppState>,
    request_id: String,
    option_id: Option<String>,
) -> AppResult<()> {
    state.bridge.respond_permission(&request_id, option_id).await
}

#[tauri::command]
pub async fn respond_elicitation(
    state: State<'_, AppState>,
    request_id: String,
    action: String,
    content: Option<Value>,
) -> AppResult<()> {
    state
        .bridge
        .respond_elicitation(&request_id, action, content)
        .await
}
