use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config;
use crate::paths;
use crate::plugins::{self, PluginRecord};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillRecord {
    pub name: String,
    pub description: String,
    pub scope: String,
    pub source: String,
    pub plugin: Option<String>,
    pub user_invocable: bool,
    pub enabled: bool,
}

/// Every skill the agent can see, in the order Vibe resolves them:
/// project, then user, then plugin, then built-in.
pub fn discover(cwd: Option<&Path>, plugin_records: &[PluginRecord]) -> Vec<SkillRecord> {
    let (config, _) = config::load_effective(cwd);
    let mut records: Vec<SkillRecord> = Vec::new();

    if let Some(cwd) = cwd {
        for dir in [cwd.join(".vibe/skills"), cwd.join(".agents/skills")] {
            scan_dir(&dir, "project", &mut records);
        }
    }

    for dir in [
        paths::vibe_home().join("skills"),
        paths::home_dir().map(|home| home.join(".agents/skills")).unwrap_or_default(),
    ] {
        scan_dir(&dir, "user", &mut records);
    }

    for entry in &config.skill_paths {
        let path = resolve_path(entry, cwd);
        scan_dir(&path, "user", &mut records);
    }

    for plugin in plugin_records.iter().filter(|p| p.enabled) {
        let root = PathBuf::from(&plugin.path);
        for dir in [root.join("skills"), root.join(".opencode/skills")] {
            let mut found = Vec::new();
            scan_dir(&dir, "plugin", &mut found);
            for mut skill in found {
                skill.plugin = Some(plugin.name.clone());
                records.push(skill);
            }
        }
    }

    // Mastery bundles are their own store, not a plugin: `vibe-mastery` clones
    // each upstream repo under `~/.vibe/mastery/sources` and records the ones it
    // enabled in `config.json`. Nothing else on disk says which of those the
    // agent can actually load, so the config is the owner and this reads it
    // rather than guessing at directory layouts. Without this the panel showed
    // only the one bundle that ships inside the plugin and hid the other twenty
    // skills the agent was loading.
    for bundle in mastery_bundles() {
        let root = PathBuf::from(&bundle.root);
        let mut found = Vec::new();
        // An authored bundle points straight at its skill directory; a cloned
        // one points at a repo whose skills live under `skills/`.
        if root.join("SKILL.md").is_file() {
            scan_dir(&root, "mastery", &mut found);
        } else {
            scan_dir(&root.join("skills"), "mastery", &mut found);
        }
        for mut skill in found {
            skill.plugin = Some(bundle.label.clone());
            records.push(skill);
        }
    }

    // First definition wins, matching the agent's precedence.
    let mut seen = std::collections::HashSet::new();
    records.retain(|skill| seen.insert(skill.name.clone()));
    records.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    records
}

/// One bundle as `vibe-mastery` records it.
#[derive(Debug, Clone, Deserialize)]
struct MasteryBundle {
    #[serde(default)]
    label: String,
    #[serde(default)]
    root: String,
    #[serde(default = "enabled_by_default")]
    enabled: bool,
}

fn enabled_by_default() -> bool {
    true
}

#[derive(Debug, Deserialize)]
struct MasteryConfig {
    #[serde(default)]
    bundles: Vec<MasteryBundle>,
}

/// The enabled mastery bundles, or nothing when the store is absent.
///
/// A missing or unreadable config is an empty list rather than an error: the
/// mastery store is optional, and a panel that refuses to list anything because
/// one optional store is missing would be worse than one that lists the rest.
fn mastery_bundles() -> Vec<MasteryBundle> {
    mastery_bundles_at(&paths::vibe_home().join("mastery").join("config.json"))
}

fn mastery_bundles_at(config_path: &Path) -> Vec<MasteryBundle> {
    let Ok(raw) = std::fs::read_to_string(config_path) else {
        return Vec::new();
    };
    let Ok(config) = serde_json::from_str::<MasteryConfig>(&raw) else {
        return Vec::new();
    };
    config
        .bundles
        .into_iter()
        .filter(|bundle| bundle.enabled && !bundle.root.is_empty())
        .collect()
}

fn resolve_path(entry: &str, cwd: Option<&Path>) -> PathBuf {
    let path = PathBuf::from(entry);
    if path.is_absolute() {
        return path;
    }
    match cwd {
        Some(cwd) => cwd.join(path),
        None => path,
    }
}

fn scan_dir(dir: &Path, scope: &str, out: &mut Vec<SkillRecord>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let skill_file = if path.is_dir() {
            path.join("SKILL.md")
        } else if path.extension().is_some_and(|ext| ext == "md") {
            path.clone()
        } else {
            continue;
        };
        if !skill_file.is_file() {
            continue;
        }

        let fallback = path
            .file_stem()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "skill".into());
        let meta = read_frontmatter(&skill_file);

        out.push(SkillRecord {
            name: meta.name.unwrap_or(fallback),
            description: meta.description.unwrap_or_default(),
            scope: scope.to_string(),
            source: skill_file.display().to_string(),
            plugin: None,
            user_invocable: meta.user_invocable,
            enabled: meta.enabled,
        });
    }
}

#[derive(Default)]
struct Frontmatter {
    name: Option<String>,
    description: Option<String>,
    user_invocable: bool,
    enabled: bool,
}

fn read_frontmatter(path: &Path) -> Frontmatter {
    let mut meta = Frontmatter {
        user_invocable: true,
        enabled: true,
        ..Frontmatter::default()
    };

    let Ok(raw) = std::fs::read_to_string(path) else {
        return meta;
    };

    let mut lines = raw.lines();
    if lines.next().map(str::trim) != Some("---") {
        return meta;
    }

    for line in lines {
        let trimmed = line.trim();
        if trimmed == "---" {
            break;
        }
        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        let value = value.trim().trim_matches('"').trim_matches('\'');
        match key.trim() {
            "name" => meta.name = Some(value.to_string()),
            "description" => meta.description = Some(value.to_string()),
            "user-invocable" | "user_invocable" => {
                meta.user_invocable = !matches!(value, "false" | "no" | "off");
            }
            "enabled" => meta.enabled = !matches!(value, "false" | "no" | "off"),
            _ => {}
        }
    }

    meta
}

/// Convenience wrapper used by the `vibe_skills` command.
pub fn discover_all(cwd: Option<&Path>) -> Vec<SkillRecord> {
    let plugins = plugins::discover(cwd);
    discover(cwd, &plugins)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vd-skills-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_skill(dir: &Path, name: &str, description: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {description}\n---\n\nbody\n"),
        )
        .unwrap();
    }

    #[test]
    fn a_cloned_bundle_is_found_under_its_skills_directory() {
        let root = scratch("cloned");
        write_skill(&root.join("skills/3dviz-pro-max"), "3dviz-pro-max", "3D scenes");
        write_skill(&root.join("skills/other"), "other", "another");

        let mut found = Vec::new();
        scan_dir(&root.join("skills"), "mastery", &mut found);

        let mut names: Vec<&str> = found.iter().map(|s| s.name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["3dviz-pro-max", "other"]);
        assert!(found.iter().all(|s| s.scope == "mastery"));
    }

    #[test]
    fn an_authored_bundle_points_straight_at_its_skill() {
        let root = scratch("authored");
        write_skill(&root, "assay", "a procedure");

        let mut found = Vec::new();
        scan_dir(&root, "mastery", &mut found);

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "assay");
        assert_eq!(found[0].description, "a procedure");
    }

    #[test]
    fn only_enabled_bundles_with_a_root_are_listed() {
        let dir = scratch("config");
        let path = dir.join("config.json");
        std::fs::write(
            &path,
            r#"{"version":2,"bundles":[
                {"id":"a","label":"A","root":"C:/a","enabled":true},
                {"id":"b","label":"B","root":"C:/b","enabled":false},
                {"id":"c","label":"C","root":"","enabled":true}
            ]}"#,
        )
        .unwrap();

        let bundles = mastery_bundles_at(&path);
        assert_eq!(bundles.len(), 1);
        assert_eq!(bundles[0].label, "A");
    }

    #[test]
    fn a_missing_or_broken_config_is_an_empty_list_not_a_failure() {
        let dir = scratch("broken");
        assert!(mastery_bundles_at(&dir.join("absent.json")).is_empty());

        let path = dir.join("config.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert!(mastery_bundles_at(&path).is_empty());
    }
}

