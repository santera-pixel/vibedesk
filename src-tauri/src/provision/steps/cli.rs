//! The `cli` step: the Vibe CLI itself.
//!
//! `install.rs` keeps its job and its public functions. This step calls
//! `install::status()` and `install::run()`; it does not replace them.

use crate::provision::Probe;

pub fn probe() -> Probe {
    let status = crate::install::status();
    if status.binary_found {
        Probe {
            satisfied: true,
            version: None,
            detail: status
                .binary_path
                .clone()
                .unwrap_or_else(|| "vibe-acp found".into()),
        }
    } else {
        Probe {
            satisfied: false,
            version: None,
            detail: match &status.plan {
                Some(plan) => format!("not installed; would run `{}`", plan.command),
                None => "not installed, and no uv, pipx or python to install it with".into(),
            },
        }
    }
}

pub async fn install() -> Result<String, String> {
    let outcome = crate::install::run().await.map_err(|error| error.to_string())?;
    if outcome.ok {
        Ok(outcome
            .binary_path
            .unwrap_or_else(|| "vibe-acp installed".into()))
    } else {
        Err(format!(
            "install exited {}: {}",
            outcome
                .code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "without a code".into()),
            outcome.output.trim()
        ))
    }
}
