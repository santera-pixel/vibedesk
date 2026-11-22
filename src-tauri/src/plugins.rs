use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::config;
use crate::error::{AppError, AppResult};
use crate::paths;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginComponent {
    pub kind: String,
    pub name: String,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginRecord {
    pub name: String,
    pub display_name: Option<String>,
    pub version: Option<String>,
    pub description: Option<String>,
    pub author: Option<String>,
    pub license: Option<String>,
    pub scope: String,
    pub path: String,
    pub format: String,
    pub enabled: bool,
    pub default_enabled: bool,
    pub tool_namespace: Option<String>,
    pub components: Vec<PluginComponent>,
    pub issues: Vec<String>,
    pub digest: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginReloadReport {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub updated: Vec<String>,
    pub unchanged: usize,
    pub repinned: bool,
    pub plugins: Vec<PluginRecord>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Manifest {
    name: Option<String>,
    version: Option<String>,
    description: Option<String>,
    author: Option<Value>,
    license: Option<String>,
    extensions: Option<BTreeMap<String, Value>>,
}

/// Discover every plugin root under the user and project plugin directories.
pub fn discover(cwd: Option<&Path>) -> Vec<PluginRecord> {
    let (config, _) = config::load_effective(cwd);
    let mut records = Vec::new();

    for dir in config::plugins_dirs(cwd) {
        let scope = if dir.starts_with(paths::vibe_home()) {
            "user"
        } else {
            "project"
        };
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            if let Some(record) = read_plugin(&path, scope, &config) {
                records.push(record);
            }
        }
    }

    // Project scope wins on name collisions, matching Vibe's precedence.
    records.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| scope_rank(&b.scope).cmp(&scope_rank(&a.scope)))
    });
    records.dedup_by(|a, b| a.name == b.name);
    records.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    records
}

fn scope_rank(scope: &str) -> u8 {
    if scope == "project" {
        2
    } else {
        1
    }
}

fn read_plugin(root: &Path, scope: &str, config: &config::VibeConfig) -> Option<PluginRecord> {
    let (manifest_path, format) = locate_manifest(root)?;
    let raw = std::fs::read_to_string(&manifest_path).ok()?;
    let manifest: Manifest = serde_json::from_str(&raw).ok()?;

    let name = manifest
        .name
        .clone()
        .or_else(|| root.file_name().map(|n| n.to_string_lossy().to_string()))?;

    let mut issues = Vec::new();
    let mut display_name = None;
    let mut tool_namespace = None;
    let mut default_enabled = true;

    if let Some(extensions) = &manifest.extensions {
        match extensions.get("ai.mistral.vibe") {
            Some(extension) => {
                display_name = extension
                    .get("displayName")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                tool_namespace = extension
                    .get("toolNamespace")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                if let Some(flag) = extension.get("defaultEnabled").and_then(Value::as_bool) {
                    default_enabled = flag;
                }
            }
            None if format == "native" => {
                issues.push(
                    "No ai.mistral.vibe extension: hooks, knowledge and agents are ignored.".into(),
                );
            }
            None => {}
        }
    }

    let enabled = if config.disabled_plugins.iter().any(|n| n == &name) {
        false
    } else if config.enabled_plugins.iter().any(|n| n == &name) {
        true
    } else {
        default_enabled
    };

    let components = collect_components(root, format);
    let digest = Some(short_digest(&raw));

    Some(PluginRecord {
        name,
        display_name,
        version: manifest.version,
        description: manifest.description,
        author: manifest.author.as_ref().and_then(author_name),
        license: manifest.license,
        scope: scope.to_string(),
        path: root.display().to_string(),
        format: format.to_string(),
        enabled,
        default_enabled,
        tool_namespace,
        components,
        issues,
        digest,
    })
}

fn author_name(value: &Value) -> Option<String> {
    match value {
        Value::String(name) => Some(name.clone()),
        Value::Object(map) => map
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_string),
        _ => None,
    }
}

fn locate_manifest(root: &Path) -> Option<(PathBuf, &'static str)> {
    let candidates: [(&str, &str); 5] = [
        ("plugin.json", "native"),
        (".claude-plugin/plugin.json", "claude"),
        (".codex-plugin/plugin.json", "codex"),
        (".kimi.plugin.json", "kimi"),
        (".kimi-plugin/plugin.json", "kimi"),
    ];
    for (relative, format) in candidates {
        let path = root.join(relative);
        if path.is_file() {
            return Some((path, format));
        }
    }
    if root.join(".opencode").is_dir() || root.join("opencode.json").is_file() {
        return Some((root.join("opencode.json"), "opencode"));
    }
    None
}

fn collect_components(root: &Path, format: &str) -> Vec<PluginComponent> {
    let mut components = Vec::new();

    // Skills — the one component every adapted format can contribute.
    for skill_dir in [root.join("skills"), root.join(".opencode/skills")] {
        let Ok(entries) = std::fs::read_dir(&skill_dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let skill_file = if path.is_dir() {
                path.join("SKILL.md")
            } else if path.file_name().is_some_and(|n| n == "SKILL.md") {
                path.clone()
            } else {
                continue;
            };
            if !skill_file.is_file() {
                continue;
            }
            let (name, description) = read_skill_frontmatter(&skill_file);
            components.push(PluginComponent {
                kind: "skill".into(),
                name: name.unwrap_or_else(|| {
                    path.file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| "skill".into())
                }),
                detail: description,
            });
        }
    }

    // MCP servers.
    if let Ok(raw) = std::fs::read_to_string(root.join("mcp.json")) {
        if let Ok(value) = serde_json::from_str::<Value>(&raw) {
            if let Some(servers) = value.get("mcpServers").and_then(Value::as_object) {
                for (name, server) in servers {
                    let transport = server
                        .get("type")
                        .or_else(|| server.get("transport"))
                        .and_then(Value::as_str)
                        .unwrap_or("stdio");
                    components.push(PluginComponent {
                        kind: "mcp".into(),
                        name: name.clone(),
                        detail: Some(transport.to_string()),
                    });
                }
            }
        }
    }

    if format != "native" {
        return components;
    }

    // Native-only components live under ai.mistral.vibe/.
    let native = root.join("ai.mistral.vibe");

    if let Ok(raw) = std::fs::read_to_string(native.join("hooks.toml")) {
        if let Ok(parsed) = toml::from_str::<config::HooksFile>(&raw) {
            for hook in parsed.hooks {
                components.push(PluginComponent {
                    kind: "hook".into(),
                    name: hook.name,
                    detail: Some(hook.event),
                });
            }
        }
    }

    if let Ok(entries) = std::fs::read_dir(native.join("agents")) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "toml") {
                components.push(PluginComponent {
                    kind: "agent".into(),
                    name: path
                        .file_stem()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default(),
                    detail: None,
                });
            }
        }
    }

    if let Ok(entries) = std::fs::read_dir(native.join("knowledge")) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.join("KNOWLEDGE.md").is_file() {
                components.push(PluginComponent {
                    kind: "knowledge".into(),
                    name: path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default(),
                    detail: None,
                });
            }
        }
    }

    for (file, kind) in [("connectors.json", "connector"), ("libraries.json", "library")] {
        if let Ok(raw) = std::fs::read_to_string(root.join(file)) {
            if let Ok(value) = serde_json::from_str::<Value>(&raw) {
                if let Some(map) = value.as_object() {
                    for key in map.keys() {
                        components.push(PluginComponent {
                            kind: kind.into(),
                            name: key.clone(),
                            detail: None,
                        });
                    }
                }
            }
        }
    }

    components
}

/// Read `name` and `description` out of a SKILL.md YAML frontmatter block.
pub fn read_skill_frontmatter(path: &Path) -> (Option<String>, Option<String>) {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return (None, None);
    };
    let mut name = None;
    let mut description = None;

    let mut lines = raw.lines();
    if lines.next().map(str::trim) != Some("---") {
        return (None, None);
    }
    for line in lines {
        let trimmed = line.trim();
        if trimmed == "---" {
            break;
        }
        if let Some(value) = trimmed.strip_prefix("name:") {
            name = Some(value.trim().trim_matches('"').trim_matches('\'').to_string());
        } else if let Some(value) = trimmed.strip_prefix("description:") {
            description = Some(value.trim().trim_matches('"').trim_matches('\'').to_string());
        }
    }

    (name, description)
}

fn short_digest(raw: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(raw.as_bytes());
    let digest = hasher.finalize();
    digest.iter().take(6).map(|b| format!("{b:02x}")).collect()
}

/// Re-discover plugins and diff against the previous snapshot.
pub fn reload(cwd: Option<&Path>, previous: &[PluginRecord]) -> PluginReloadReport {
    let plugins = discover(cwd);

    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut updated = Vec::new();
    let mut unchanged = 0usize;

    for plugin in &plugins {
        match previous.iter().find(|p| p.name == plugin.name) {
            None => added.push(plugin.name.clone()),
            Some(before) => {
                if before.digest != plugin.digest || before.enabled != plugin.enabled {
                    updated.push(plugin.name.clone());
                } else {
                    unchanged += 1;
                }
            }
        }
    }
    for before in previous {
        if !plugins.iter().any(|p| p.name == before.name) {
            removed.push(before.name.clone());
        }
    }

    PluginReloadReport {
        added,
        removed,
        updated,
        unchanged,
        repinned: false,
        plugins,
    }
}

/// Toggle a plugin by editing `disabled_plugins` in the user config.
///
/// A `.bak` copy is written first: the agent owns this file, and a bad write
/// would break every future session.
pub fn set_enabled(name: &str, enabled: bool) -> AppResult<Vec<PluginRecord>> {
    let path = config::user_config_path();
    let raw = std::fs::read_to_string(&path).unwrap_or_default();

    let mut document: toml::Value = if raw.trim().is_empty() {
        toml::Value::Table(toml::map::Map::new())
    } else {
        toml::from_str(&raw).map_err(|e| AppError::message(format!("config.toml: {e}")))?
    };

    if !raw.trim().is_empty() {
        let backup = path.with_extension("toml.bak");
        std::fs::write(&backup, &raw)?;
    }

    let table = document
        .as_table_mut()
        .ok_or_else(|| AppError::message("config.toml is not a table"))?;

    let key = "disabled_plugins";
    let mut disabled: Vec<String> = table
        .get(key)
        .and_then(toml::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(toml::Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    disabled.retain(|entry| entry != name);
    if !enabled {
        disabled.push(name.to_string());
    }

    table.insert(
        key.to_string(),
        toml::Value::Array(disabled.into_iter().map(toml::Value::String).collect()),
    );

    let body = toml::to_string_pretty(&document)
        .map_err(|e| AppError::message(format!("could not serialise config.toml: {e}")))?;
    std::fs::write(&path, body)?;

    Ok(discover(None))
}
