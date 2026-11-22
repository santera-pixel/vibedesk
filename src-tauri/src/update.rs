use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::paths;
use crate::settings;

const PYPI_URL: &str = "https://pypi.org/pypi/mistral-vibe/json";
const PROJECT_URL: &str = "https://pypi.org/project/mistral-vibe/";
const CHECK_INTERVAL_HOURS: i64 = 24;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    pub current_version: Option<String>,
    pub latest_version: Option<String>,
    pub update_available: bool,
    pub channel: String,
    pub checked_at: Option<String>,
    pub install_method: String,
    pub upgrade_command: Option<String>,
    pub release_url: Option<String>,
    pub notes: Option<String>,
    pub error: Option<String>,
    pub applying: bool,
}

/// Version of the installed `vibe` package, read from its own metadata.
///
/// Spawning `vibe --version` would be slower and would fail whenever the
/// binary is mid-upgrade; the package file is always there.
pub fn current_version() -> Option<String> {
    let binary = paths::resolve_acp_binary().ok()?;
    let package = paths::vibe_package_dir(&binary.path)?;
    let init = package.join("__init__.py");
    let raw = std::fs::read_to_string(init).ok()?;

    for line in raw.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("__version__") {
            let value = rest.trim_start_matches([' ', '=', ':']).trim();
            let value = value.trim_matches('"').trim_matches('\'');
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

fn install_method() -> (String, Option<String>) {
    let Ok(binary) = paths::resolve_acp_binary() else {
        return ("unknown".into(), None);
    };
    let path = binary.path.to_string_lossy().to_lowercase();

    if path.contains("uv/tools") || path.contains("uv\\tools") {
        ("uv".into(), Some("uv tool upgrade mistral-vibe".into()))
    } else if path.contains("pipx") {
        ("pipx".into(), Some("pipx upgrade mistral-vibe".into()))
    } else if path.contains("site-packages") || path.contains("scripts") {
        ("pip".into(), Some("pip install --upgrade mistral-vibe".into()))
    } else {
        ("unknown".into(), None)
    }
}

fn is_fresh(checked_at: Option<&str>) -> bool {
    let Some(value) = checked_at else {
        return false;
    };
    let Ok(parsed) = DateTime::parse_from_rfc3339(value) else {
        return false;
    };
    let age = Utc::now().signed_duration_since(parsed.with_timezone(&Utc));
    age.num_hours() < CHECK_INTERVAL_HOURS
}

fn base_status() -> UpdateStatus {
    let (install_method, upgrade_command) = install_method();
    UpdateStatus {
        current_version: current_version(),
        latest_version: None,
        update_available: false,
        channel: "stable".into(),
        checked_at: None,
        install_method,
        upgrade_command,
        release_url: Some(PROJECT_URL.into()),
        notes: None,
        error: None,
        applying: false,
    }
}

/// Check PyPI for a newer release.
///
/// `force` bypasses the 24-hour gate that keeps the app from hitting the
/// network on every launch. Failures are reported in `error` rather than
/// propagated: an offline machine should not break the UI.
pub async fn check(force: bool) -> UpdateStatus {
    let mut status = base_status();
    let stored = settings::load();

    if !force && is_fresh(stored.last_update_check.as_deref()) {
        status.checked_at = stored.last_update_check.clone();
        status.latest_version = stored.latest_version.clone();
        status.update_available = is_newer(
            status.latest_version.as_deref(),
            status.current_version.as_deref(),
        );
        return status;
    }

    match fetch_latest().await {
        Ok(latest) => {
            let checked_at = Utc::now().to_rfc3339();
            status.latest_version = Some(latest.clone());
            status.checked_at = Some(checked_at.clone());
            status.update_available = is_newer(Some(&latest), status.current_version.as_deref());
            status.release_url = Some(format!("{PROJECT_URL}{latest}/"));

            let mut next = stored;
            next.last_update_check = Some(checked_at);
            next.latest_version = Some(latest);
            let _ = settings::save(&next);
        }
        Err(error) => {
            status.error = Some(error);
            status.checked_at = stored.last_update_check;
            status.latest_version = stored.latest_version;
            status.update_available = is_newer(
                status.latest_version.as_deref(),
                status.current_version.as_deref(),
            );
        }
    }

    status
}

async fn fetch_latest() -> Result<String, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(12))
        .user_agent(concat!("VibeDesk/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())?;

    let response = client
        .get(PYPI_URL)
        .send()
        .await
        .map_err(|e| format!("could not reach PyPI: {e}"))?;

    if !response.status().is_success() {
        return Err(format!("PyPI returned HTTP {}", response.status()));
    }

    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|e| format!("PyPI response was not JSON: {e}"))?;

    body.pointer("/info/version")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "PyPI response had no info.version".to_string())
}

fn is_newer(latest: Option<&str>, current: Option<&str>) -> bool {
    let (Some(latest), Some(current)) = (latest, current) else {
        return false;
    };
    match (
        semver::Version::parse(latest),
        semver::Version::parse(current),
    ) {
        (Ok(latest), Ok(current)) => latest > current,
        _ => latest != current,
    }
}

/// Run the upgrade command for the detected install method.
///
/// The caller must stop the agent first: on Windows the running `vibe-acp.exe`
/// holds a lock on the file the installer needs to replace.
pub fn apply() -> UpdateStatus {
    let mut status = base_status();
    status.applying = true;

    let Some(command) = status.upgrade_command.clone() else {
        status.applying = false;
        status.error = Some(
            "Could not tell how Vibe was installed. Run `uv tool upgrade mistral-vibe` yourself."
                .into(),
        );
        return status;
    };

    let (program, args) = split_command(&command);
    let result = std::process::Command::new(&program)
        .args(&args)
        .output();

    status.applying = false;

    match result {
        Ok(output) if output.status.success() => {
            status.current_version = current_version();
            status.latest_version = status.current_version.clone();
            status.update_available = false;
            status.checked_at = Some(Utc::now().to_rfc3339());
            status.notes = crate::release_notes::latest().map(|note| note.body);
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let detail = if stderr.trim().is_empty() {
                stdout.trim().to_string()
            } else {
                stderr.trim().to_string()
            };
            status.error = Some(format!(
                "`{command}` failed. {}",
                if detail.is_empty() {
                    "No output.".to_string()
                } else {
                    detail
                }
            ));
        }
        Err(error) => {
            status.error = Some(format!("could not run `{command}`: {error}"));
        }
    }

    status
}

fn split_command(command: &str) -> (String, Vec<String>) {
    let mut parts = command.split_whitespace();
    let program = parts.next().unwrap_or("uv").to_string();
    (program, parts.map(str::to_string).collect())
}
