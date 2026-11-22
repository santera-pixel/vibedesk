//! The `donsetch` step: the web capability.
//!
//! Installed through npm, which is the upstream mechanism. A machine without
//! node is `skipped` with the reason rather than `failed`: the step never ran,
//! and a swarm still works without a web tool.

use crate::provision::Probe;

const PACKAGE: &str = "donsetch";

pub fn probe() -> Probe {
    match crate::capabilities::external::find(PACKAGE) {
        Some(path) => {
            let version = crate::capabilities::external::status()
                .into_iter()
                .find(|server| server.name == PACKAGE)
                .and_then(|server| server.version);
            Probe {
                satisfied: true,
                version,
                detail: path.display().to_string(),
            }
        }
        None => Probe {
            satisfied: false,
            version: None,
            detail: match crate::paths::find_program(&["npm"]) {
                Some(_) => format!("not installed; would run `npm i -g {PACKAGE}`"),
                None => "not installed, and npm is not on PATH".into(),
            },
        },
    }
}

/// Whether this step can run at all. `false` is a `skipped`, not a `failed`.
pub fn can_run() -> Result<(), String> {
    if crate::paths::find_program(&["npm"]).is_none() {
        return Err("npm is not on PATH".into());
    }
    Ok(())
}

pub async fn install() -> Result<String, String> {
    can_run()?;
    let npm = crate::paths::find_program(&["npm"]).expect("checked by can_run");
    let output = tokio::process::Command::new(npm)
        .args(["i", "-g", PACKAGE])
        .output()
        .await
        .map_err(|error| format!("could not run npm: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("npm i -g {PACKAGE} failed: {}", stderr.trim()));
    }
    crate::capabilities::external::register(PACKAGE)
        .map_err(|error| format!("installed {PACKAGE} but could not register it: {error}"))?;
    Ok(format!("npm i -g {PACKAGE}"))
}
