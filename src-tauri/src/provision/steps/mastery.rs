//! The `mastery` step: the plugin and the skill bundles.
//!
//! Two halves. The plugin is vendored — `github.com/santera-pixel/dsh-mastery`
//! is not published yet, so there is nothing to clone, and 79 KB in the bundle
//! beats a network round trip on first launch even once it is.
//!
//! The bundles are partial sparse clones. A full clone of the three is 201 MB;
//! the cone each one needs is 20 MB, and because the clone is a real clone
//! `git pull` still works afterwards.

use crate::provision::Probe;
use std::path::{Path, PathBuf};

/// The vendored plugin, as (path under the plugin root, body).
const PLUGIN: &[(&str, &str)] = &[
    ("README.md", include_str!("../payload/vibe-mastery/README.md")),
    (
        "ai.mistral.vibe/hooks.toml",
        include_str!("../payload/vibe-mastery/ai.mistral.vibe/hooks.toml"),
    ),
    (
        "hooks/session-start.js",
        include_str!("../payload/vibe-mastery/hooks/session-start.js"),
    ),
    ("mcp.json", include_str!("../payload/vibe-mastery/mcp.json")),
    (
        "plugin.json",
        include_str!("../payload/vibe-mastery/plugin.json"),
    ),
    (
        "server/bundles.js",
        include_str!("../payload/vibe-mastery/server/bundles.js"),
    ),
    (
        "server/config.js",
        include_str!("../payload/vibe-mastery/server/config.js"),
    ),
    (
        "server/git.js",
        include_str!("../payload/vibe-mastery/server/git.js"),
    ),
    (
        "server/index.js",
        include_str!("../payload/vibe-mastery/server/index.js"),
    ),
    (
        "server/state.js",
        include_str!("../payload/vibe-mastery/server/state.js"),
    ),
    (
        "skills/assay/SKILL.md",
        include_str!("../payload/vibe-mastery/skills/assay/SKILL.md"),
    ),
];

/// The three bundles, with the cone each one needs.
const BUNDLES: &[(&str, &str, &[&str])] = &[
    (
        "3dviz-pro-max",
        "https://github.com/viettranx/3dviz-pro-max",
        &["skills/3dviz-pro-max"],
    ),
    (
        "Anti-Slop",
        "https://github.com/miqdadbadjuber/Anti-Slop",
        &["skills", "rules"],
    ),
    (
        "ui-ux-pro-max-skill",
        "https://github.com/nextlevelbuilder/ui-ux-pro-max-skill",
        &["cli/assets/skills"],
    ),
];

fn plugin_root() -> PathBuf {
    crate::paths::vibe_home().join("plugins").join("vibe-mastery")
}

fn sources_root() -> PathBuf {
    crate::paths::vibe_home().join("mastery").join("sources")
}

pub fn probe() -> Probe {
    let plugin = plugin_root().join("plugin.json").is_file();
    let present: Vec<&str> = BUNDLES
        .iter()
        .filter(|(_, _, _)| true)
        .map(|(name, _, _)| *name)
        .filter(|name| sources_root().join(name).is_dir())
        .collect();
    let satisfied = plugin && present.len() == BUNDLES.len();
    Probe {
        satisfied,
        version: None,
        detail: format!(
            "plugin {}, {} of {} bundles present",
            if plugin { "present" } else { "missing" },
            present.len(),
            BUNDLES.len()
        ),
    }
}

pub async fn install() -> Result<String, String> {
    let mut notes = Vec::new();

    let root = plugin_root();
    let mut wrote = 0usize;
    for (relative, body) in PLUGIN {
        let path = root.join(relative);
        if std::fs::read_to_string(&path).map(|existing| existing == *body).unwrap_or(false) {
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
        }
        std::fs::write(&path, body)
            .map_err(|error| format!("could not write {}: {error}", path.display()))?;
        wrote += 1;
    }
    notes.push(format!("plugin: {wrote} of {} files written", PLUGIN.len()));

    let git = crate::paths::find_program(&["git"])
        .ok_or_else(|| "git is not on PATH, so the bundles cannot be cloned".to_string())?;

    for (name, repo, cone) in BUNDLES {
        let root = sources_root().join(name);
        if root.join(".git").is_dir() {
            notes.push(format!("{name}: already cloned"));
            continue;
        }
        match sparse_clone(&git, repo, cone, &root).await {
            Ok(()) => notes.push(format!("{name}: cloned {} cone(s)", cone.len())),
            Err(error) => notes.push(format!("{name}: {error}")),
        }
    }

    Ok(notes.join("; "))
}

/// Clone one bundle's cone into `root`, leaving the old root alone on failure.
async fn sparse_clone(
    git: &Path,
    repo: &str,
    cone: &[&str],
    root: &Path,
) -> Result<(), String> {
    let staging = root.with_extension("new");
    let _ = std::fs::remove_dir_all(&staging);
    if let Some(parent) = staging.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    }

    let clone = tokio::process::Command::new(git)
        .args(["clone", "--filter=blob:none", "--sparse", "--depth", "1", repo])
        .arg(&staging)
        .output()
        .await
        .map_err(|error| format!("could not run git: {error}"))?;
    if !clone.status.success() {
        let stderr = String::from_utf8_lossy(&clone.stderr);
        let _ = std::fs::remove_dir_all(&staging);
        return Err(format!("clone failed: {}", stderr.trim()));
    }

    let mut checkout = tokio::process::Command::new(git);
    checkout.arg("-C").arg(&staging).args(["sparse-checkout", "set"]);
    for path in cone {
        checkout.arg(path);
    }
    let set = checkout
        .output()
        .await
        .map_err(|error| format!("could not run git: {error}"))?;
    if !set.status.success() {
        let stderr = String::from_utf8_lossy(&set.stderr);
        let _ = std::fs::remove_dir_all(&staging);
        return Err(format!("sparse-checkout failed: {}", stderr.trim()));
    }

    // A cone that yields no skill is the wrong cone, and swapping it in would
    // replace a working root with an empty one.
    if !has_a_skill(&staging) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err("the cone yielded no SKILL.md, so the old root was kept".into());
    }

    if root.exists() {
        let _ = std::fs::remove_dir_all(root);
    }
    std::fs::rename(&staging, root)
        .map_err(|error| format!("could not move {} into place: {error}", staging.display()))
}

fn has_a_skill(root: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(root) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && has_a_skill(&path) {
            return true;
        }
        if path.file_name().and_then(|name| name.to_str()) == Some("SKILL.md") {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_vendored_plugin_is_complete() {
        assert_eq!(PLUGIN.len(), 11);
        for (relative, body) in PLUGIN {
            assert!(!body.trim().is_empty(), "{relative} is empty");
        }
        assert!(PLUGIN.iter().any(|(path, _)| *path == "plugin.json"));
        assert!(PLUGIN.iter().any(|(path, _)| *path == "mcp.json"));
    }

    #[test]
    fn every_bundle_names_a_repo_and_a_cone() {
        assert_eq!(BUNDLES.len(), 3);
        for (name, repo, cone) in BUNDLES {
            assert!(repo.starts_with("https://github.com/"), "{name} has no repo");
            assert!(!cone.is_empty(), "{name} has no cone");
        }
    }

    #[test]
    fn a_cone_with_no_skill_is_refused() {
        let dir = std::env::temp_dir().join("vibedesk-cone-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("empty")).unwrap();
        assert!(!has_a_skill(&dir), "an empty tree has no skill");
        std::fs::write(dir.join("empty").join("SKILL.md"), "x").unwrap();
        assert!(has_a_skill(&dir), "a nested SKILL.md is found");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
