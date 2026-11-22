//! The `aegis` step: the Aegis method pack.
//!
//! Aegis is third-party with its own installer, so this step clones it and runs
//! its own registration script rather than reimplementing either. The clone is
//! shallow: the pack is 11 MB of markdown and scripts, and history is not part
//! of what it does.

use crate::provision::Probe;
use std::path::PathBuf;

const REPO: &str = "https://github.com/GanyuanRan/Aegis.git";

fn root() -> PathBuf {
    crate::paths::vibe_home().join("aegis")
}

pub fn probe() -> Probe {
    let root = root();
    let registered = root.join("AGENTS.md").is_file();
    if registered {
        return Probe {
            satisfied: true,
            version: version_at(&root),
            detail: root.display().to_string(),
        };
    }
    Probe {
        satisfied: false,
        version: None,
        detail: format!("{} is not a clone of {REPO}", root.display()),
    }
}

fn version_at(root: &std::path::Path) -> Option<String> {
    let raw = std::fs::read_to_string(root.join("VERSION")).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

pub async fn install() -> Result<String, String> {
    let root = root();
    if root.join("AGENTS.md").is_file() {
        return Ok(format!("already present at {}", root.display()));
    }
    if let Some(parent) = root.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    }

    let git = crate::paths::find_program(&["git"])
        .ok_or_else(|| "git is not on PATH, so the pack cannot be cloned".to_string())?;

    let output = tokio::process::Command::new(git)
        .args(["clone", "--depth", "1", REPO])
        .arg(&root)
        .output()
        .await
        .map_err(|error| format!("could not run git: {error}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git clone {REPO} failed: {}", stderr.trim()));
    }
    if !root.join("AGENTS.md").is_file() {
        return Err(format!(
            "cloned {REPO} but {} has no AGENTS.md, so it is not the pack",
            root.display()
        ));
    }

    Ok(format!(
        "cloned {REPO} into {}",
        root.display()
    ))
}
