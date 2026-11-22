use std::process::Stdio;
use std::time::Duration;

use serde::Serialize;
use tokio::process::Command;

use crate::error::{AppError, AppResult};
use crate::paths;

const INSTALL_TIMEOUT: Duration = Duration::from_secs(600);
const MAX_OUTPUT: usize = 8_000;
const DOCS_URL: &str = "https://pypi.org/project/mistral-vibe/";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallPlan {
    pub method: String,
    pub program: String,
    pub args: Vec<String>,
    pub command: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallStatus {
    pub binary_found: bool,
    pub binary_path: Option<String>,
    pub binary_source: String,
    pub uv_path: Option<String>,
    pub pipx_path: Option<String>,
    pub python_path: Option<String>,
    pub plan: Option<InstallPlan>,
    pub docs_url: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallOutcome {
    pub ok: bool,
    pub code: Option<i32>,
    pub output: String,
    pub binary_path: Option<String>,
    pub status: InstallStatus,
}

fn which(names: &[&str]) -> Option<String> {
    paths::find_program(names).map(|path| path.display().to_string())
}

/// Pick the install method from whatever package manager is actually present.
///
/// `uv` first because that is what Vibe's own docs recommend and what the
/// update checker upgrades; `pipx` next; a bare `python -m pip` last, since it
/// needs `--user` to avoid a permission prompt.
fn plan_for(uv: &Option<String>, pipx: &Option<String>, python: &Option<String>) -> Option<InstallPlan> {
    if let Some(uv) = uv {
        return Some(InstallPlan {
            method: "uv".into(),
            program: uv.clone(),
            args: vec!["tool".into(), "install".into(), "mistral-vibe".into()],
            command: "uv tool install mistral-vibe".into(),
        });
    }
    if let Some(pipx) = pipx {
        return Some(InstallPlan {
            method: "pipx".into(),
            program: pipx.clone(),
            args: vec!["install".into(), "mistral-vibe".into()],
            command: "pipx install mistral-vibe".into(),
        });
    }
    if let Some(python) = python {
        return Some(InstallPlan {
            method: "pip".into(),
            program: python.clone(),
            args: vec![
                "-m".into(),
                "pip".into(),
                "install".into(),
                "--user".into(),
                "mistral-vibe".into(),
            ],
            command: "python -m pip install --user mistral-vibe".into(),
        });
    }
    None
}

pub fn status() -> InstallStatus {
    let (binary_found, binary_path, binary_source) = match paths::resolve_acp_binary() {
        Ok(resolution) => (
            true,
            Some(resolution.path.display().to_string()),
            resolution.source,
        ),
        Err(error) => (false, None, error.to_string()),
    };

    let uv = which(&["uv.exe", "uv"]);
    let pipx = which(&["pipx.exe", "pipx"]);
    let python = which(&["python.exe", "python", "python3.exe", "python3"]);
    let plan = plan_for(&uv, &pipx, &python);

    InstallStatus {
        binary_found,
        binary_path,
        binary_source,
        uv_path: uv,
        pipx_path: pipx,
        python_path: python,
        plan,
        docs_url: DOCS_URL.into(),
    }
}

fn tail(text: &str) -> String {
    if text.len() <= MAX_OUTPUT {
        return text.to_string();
    }
    let mut start = text.len() - MAX_OUTPUT;
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    format!("…\n{}", &text[start..])
}

/// Run the install plan and report what happened.
///
/// The caller must have stopped the agent first: on Windows the installer
/// cannot replace a `vibe-acp.exe` that is still running.
pub async fn run() -> AppResult<InstallOutcome> {
    let before = status();

    let Some(plan) = before.plan.clone() else {
        return Err(AppError::message(
            "No package manager found. Install uv from https://docs.astral.sh/uv/ and try again.",
        ));
    };

    let mut command = Command::new(&plan.program);
    command
        .args(&plan.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let child = command.spawn().map_err(|error| {
        AppError::message(format!("could not run `{}`: {error}", plan.command))
    })?;

    let (ok, code, output) =
        match tokio::time::timeout(INSTALL_TIMEOUT, child.wait_with_output()).await {
            Ok(Ok(result)) => {
                let mut combined = String::from_utf8_lossy(&result.stdout).to_string();
                let stderr = String::from_utf8_lossy(&result.stderr);
                if !stderr.trim().is_empty() {
                    if !combined.is_empty() {
                        combined.push('\n');
                    }
                    combined.push_str(&stderr);
                }
                (
                    result.status.success(),
                    result.status.code(),
                    tail(combined.trim()),
                )
            }
            Ok(Err(error)) => (false, None, format!("could not read installer output: {error}")),
            Err(_) => (
                false,
                None,
                format!(
                    "`{}` did not finish within {} minutes.",
                    plan.command,
                    INSTALL_TIMEOUT.as_secs() / 60
                ),
            ),
        };

    let after = status();

    Ok(InstallOutcome {
        ok: ok && after.binary_found,
        code,
        output,
        binary_path: after.binary_path.clone(),
        status: after,
    })
}
