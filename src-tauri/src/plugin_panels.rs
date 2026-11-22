//! Panels a plugin ships.
//!
//! A plugin contributes panels by putting a `panels.json` in its own `vibedesk/`
//! folder. Unlike the other plugin configs, which are listed and wait for the
//! user to apply them, a panel is read on every load: a panel is additive
//! chrome, not a restyle of the app, and the plugin's own enable flag is the
//! switch. Disabling the plugin removes its panels.
//!
//! Two rules make that safe.
//!
//! * **Ids are namespaced here, never trusted from the file.** A panel becomes
//!   `plugin:<plugin>/<id>` and a source `plugin:<plugin>/<source-id>`, so a
//!   plugin cannot name another plugin's panel or source and cannot shadow a
//!   user panel. `panels::validate` refuses a user file that tries either.
//! * **The file is read the way `sources.rs` reads one**: `symlink_metadata`, a
//!   regular file only, and a size cap. A plugin cannot point its `panels.json`
//!   at a file it does not own, and a directory named `panels.json` is not read
//!   as one.
//!
//! A plugin whose file is missing, unreadable, oversized or invalid is skipped
//! and the reason is kept for `issues`, so one bad plugin does not take the
//! others with it.

use std::path::Path;

use crate::customize;
use crate::panels::{self, CustomPanel, PanelsConfig, PLUGIN_ID_PREFIX};
use crate::plugins::{self, PluginRecord};

/// Most panels the installed plugins may contribute together. A rail is a
/// column of icons, not a menu.
pub const MAX_PLUGIN_PANELS: usize = 8;

/// Every panel the enabled plugins contribute, in plugin order.
pub fn collect(cwd: Option<&Path>) -> Vec<CustomPanel> {
    scan(&plugins::discover(cwd), customize::safe_mode()).0
}

/// Why a plugin's panels did not load. Empty when every plugin that ships a
/// `panels.json` was read.
pub fn issues(cwd: Option<&Path>) -> Vec<String> {
    scan(&plugins::discover(cwd), customize::safe_mode()).1
}

/// The user's panels followed by the plugin panels.
///
/// This is what every read returns. The two are kept apart on disk — a plugin
/// panel is never written to the user's file — and joined only here.
pub fn merged(cwd: Option<&Path>, user: PanelsConfig) -> PanelsConfig {
    let mut config = user;
    config.panels.extend(collect(cwd));
    config
}

/// Drop every plugin-contributed panel.
///
/// Load-bearing on write: the editor saves the whole list it was shown, so
/// without this the first time a user adds a panel every plugin panel would be
/// persisted into their file and then refused by `validate` on the next load.
pub fn user_only(config: &PanelsConfig) -> PanelsConfig {
    PanelsConfig {
        panels: config
            .panels
            .iter()
            .filter(|panel| !is_plugin_panel(panel))
            .cloned()
            .collect(),
    }
}

/// Whether a panel came from a plugin. The id is checked as well as the field,
/// because a file can carry one without the other.
pub fn is_plugin_panel(panel: &CustomPanel) -> bool {
    panel.plugin.is_some() || panel.id.starts_with(PLUGIN_ID_PREFIX)
}

/// Read every enabled plugin's panels.
///
/// The plugin list and the safe-mode decision are parameters so both can be
/// tested without the environment or the user's real plugin folder.
fn scan(plugins: &[PluginRecord], safe: bool) -> (Vec<CustomPanel>, Vec<String>) {
    let mut panels: Vec<CustomPanel> = Vec::new();
    let mut issues: Vec<String> = Vec::new();

    if safe {
        return (panels, issues);
    }

    let mut dropped = 0usize;

    for plugin in plugins.iter().filter(|plugin| plugin.enabled) {
        // A namespaced id is `plugin:<name>/<id>`, so a name carrying a
        // separator would make the split ambiguous and let one plugin address
        // another's source. Refused rather than escaped: the id is the
        // isolation boundary, and an id that needs quoting is not one.
        if plugin.name.contains('/') || plugin.name.contains('\\') {
            issues.push(format!(
                "plugin `{}`: name contains a path separator, so its panels cannot be namespaced",
                plugin.name
            ));
            continue;
        }

        let root = Path::new(&plugin.path);
        let custom = root.join("vibedesk");

        // `symlink_metadata` so a plugin cannot point `vibedesk/` at a folder it
        // does not own. A missing folder is not an issue: most plugins ship no
        // panels.
        match std::fs::symlink_metadata(&custom) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => {
                issues.push(format!(
                    "plugin `{}`: vibedesk is not a directory",
                    plugin.name
                ));
                continue;
            }
            Err(_) => continue,
        }

        let raw = match read_limited(&custom.join("panels.json")) {
            Ok(Some(raw)) => raw,
            Ok(None) => continue,
            Err(reason) => {
                issues.push(format!("plugin `{}`: {reason}", plugin.name));
                continue;
            }
        };

        let config: PanelsConfig = match serde_json::from_str(&raw) {
            Ok(config) => config,
            Err(error) => {
                issues.push(format!(
                    "plugin `{}`: panels.json is not valid JSON: {error}",
                    plugin.name
                ));
                continue;
            }
        };

        // Validated before the ids are namespaced: the rules are the user's
        // rules, and a namespaced id is not one a file may write.
        if let Err(reason) = panels::validate(&config) {
            issues.push(format!("plugin `{}`: {reason}", plugin.name));
            continue;
        }

        for mut panel in config.clamped().panels {
            namespace(&mut panel, &plugin.name);
            if panels.len() >= MAX_PLUGIN_PANELS {
                dropped += 1;
                continue;
            }
            panels.push(panel);
        }
    }

    if dropped > 0 {
        issues.push(format!(
            "{dropped} plugin panel(s) dropped: the limit is {MAX_PLUGIN_PANELS}"
        ));
    }

    (panels, issues)
}

/// Read one file, refusing anything that is not a small regular file.
///
/// `Ok(None)` means the plugin ships no panels, which is the common case and
/// not a problem to report.
fn read_limited(path: &Path) -> Result<Option<String>, String> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("could not read {}: {error}", path.display())),
    };
    if !meta.file_type().is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    if meta.len() > panels::MAX_PANELS_BYTES {
        return Err(format!(
            "panels.json is {} bytes; the limit is {}",
            meta.len(),
            panels::MAX_PANELS_BYTES
        ));
    }
    std::fs::read_to_string(path)
        .map(Some)
        .map_err(|error| format!("could not read {}: {error}", path.display()))
}

/// Put a panel and its sources under the contributing plugin's namespace.
///
/// Applied unconditionally rather than read from the file: whatever a plugin
/// calls its panel, the id it ends up with is under its own name. A file that
/// writes the prefix itself never reaches here — `validate` refuses it first.
fn namespace(panel: &mut CustomPanel, plugin: &str) {
    panel.id = format!("{PLUGIN_ID_PREFIX}{plugin}/{}", panel.id);
    for source in &mut panel.sources {
        source.id = format!("{PLUGIN_ID_PREFIX}{plugin}/{}", source.id);
    }
    panel.plugin = Some(plugin.to_string());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "vibedesk-plugin-panels-{}-{name}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn record(name: &str, root: &Path, enabled: bool) -> PluginRecord {
        PluginRecord {
            name: name.into(),
            display_name: None,
            version: None,
            description: None,
            author: None,
            license: None,
            scope: "user".into(),
            path: root.display().to_string(),
            format: "native".into(),
            enabled,
            default_enabled: true,
            tool_namespace: None,
            components: Vec::new(),
            issues: Vec::new(),
            digest: None,
        }
    }

    /// A plugin folder with a `vibedesk/panels.json` holding `body`.
    fn plugin(root: &Path, name: &str, body: &str) -> PluginRecord {
        let dir = root.join(name);
        std::fs::create_dir_all(dir.join("vibedesk")).unwrap();
        std::fs::write(dir.join("vibedesk/panels.json"), body).unwrap();
        record(name, &dir, true)
    }

    fn one_panel(id: &str, source: Option<&str>) -> String {
        let sources = match source {
            Some(source) => format!(
                r#","sources":[{{"id":"{source}","type":"http","url":"http://127.0.0.1:8787/status"}}]"#
            ),
            None => String::new(),
        };
        format!(
            r#"{{"panels":[{{"id":"{id}","title":"CI","blocks":[{{"kind":"text","text":"hi"}}]{sources}}}]}}"#
        )
    }

    #[test]
    fn a_plugin_panel_is_discovered_namespaced_and_named() {
        let dir = scratch("discover");
        let plugins = vec![plugin(&dir, "ci-tools", &one_panel("ci", Some("ci")))];

        let (panels, issues) = scan(&plugins, false);
        assert!(issues.is_empty(), "{issues:?}");
        assert_eq!(panels.len(), 1);
        assert_eq!(panels[0].id, "plugin:ci-tools/ci");
        assert_eq!(panels[0].plugin.as_deref(), Some("ci-tools"));
        assert_eq!(panels[0].sources[0].id, "plugin:ci-tools/ci");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn disabling_a_plugin_removes_its_panels() {
        let dir = scratch("disabled");
        let mut disabled = plugin(&dir, "ci-tools", &one_panel("ci", None));
        disabled.enabled = false;

        let (panels, issues) = scan(&[disabled], false);
        assert!(panels.is_empty());
        assert!(issues.is_empty(), "{issues:?}");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Two plugins may both call a source `ci`. The namespace is what keeps
    /// them apart, and it is applied here rather than taken from the file.
    #[test]
    fn two_plugins_declaring_the_same_source_get_distinct_ids() {
        let dir = scratch("sources");
        let plugins = vec![
            plugin(&dir, "alpha", &one_panel("ci", Some("ci"))),
            plugin(&dir, "beta", &one_panel("ci", Some("ci"))),
        ];

        let (panels, issues) = scan(&plugins, false);
        assert!(issues.is_empty(), "{issues:?}");
        assert_eq!(panels.len(), 2);
        assert_eq!(panels[0].sources[0].id, "plugin:alpha/ci");
        assert_eq!(panels[1].sources[0].id, "plugin:beta/ci");
        assert_ne!(panels[0].id, panels[1].id);
    }

    /// The namespace is closed even to plugins: a file that writes an id in it
    /// is refused by the same rule that refuses a user file, so a plugin cannot
    /// address another plugin's panel or source.
    #[test]
    fn a_plugin_cannot_name_another_plugins_panel() {
        let dir = scratch("impersonate");
        let plugins = vec![plugin(&dir, "alpha", &one_panel("plugin:beta/ci", None))];

        let (panels, issues) = scan(&plugins, false);
        assert!(panels.is_empty());
        assert!(
            issues[0].contains("reserved for plugin-contributed"),
            "{issues:?}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn safe_mode_returns_no_plugin_panels() {
        let dir = scratch("safe");
        let plugins = vec![plugin(&dir, "ci-tools", &one_panel("ci", None))];

        let (panels, issues) = scan(&plugins, true);
        assert!(panels.is_empty());
        assert!(issues.is_empty(), "{issues:?}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_plugin_panel_cap_is_enforced() {
        let dir = scratch("cap");
        let panels: Vec<String> = (0..MAX_PLUGIN_PANELS + 2)
            .map(|index| {
                format!(r#"{{"id":"p{index}","title":"P{index}","blocks":[{{"kind":"text","text":"x"}}]}}"#)
            })
            .collect();
        let body = format!(r#"{{"panels":[{}]}}"#, panels.join(","));
        let plugins = vec![plugin(&dir, "many", &body)];

        let (kept, issues) = scan(&plugins, false);
        assert_eq!(kept.len(), MAX_PLUGIN_PANELS);
        assert_eq!(kept[0].id, "plugin:many/p0");
        assert!(
            issues.iter().any(|issue| issue.contains("dropped")),
            "{issues:?}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// One bad plugin must not stop the others: the valid sibling still loads
    /// and the reason the other did not is kept.
    #[test]
    fn an_invalid_plugin_is_skipped_with_an_issue_while_a_valid_one_loads() {
        let dir = scratch("invalid");
        let plugins = vec![
            plugin(&dir, "broken", r#"{"panels":[{"id":"ci","title":"CI"}]}"#),
            plugin(&dir, "fine", &one_panel("ci", None)),
        ];

        let (panels, issues) = scan(&plugins, false);
        assert_eq!(panels.len(), 1);
        assert_eq!(panels[0].id, "plugin:fine/ci");
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert!(issues[0].contains("plugin `broken`"), "{issues:?}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_oversized_plugin_file_is_skipped_with_an_issue() {
        let dir = scratch("oversized");
        let padding = "x".repeat(panels::MAX_PANELS_BYTES as usize + 1);
        let body = format!(r#"{{"panels":[],"padding":"{padding}"}}"#);
        let plugins = vec![plugin(&dir, "huge", &body)];

        let (panels, issues) = scan(&plugins, false);
        assert!(panels.is_empty());
        assert!(issues[0].contains("the limit is"), "{issues:?}");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The file is read the way `sources.rs` reads one: a directory named like
    /// the file is refused, and so is a link to a file the plugin does not own.
    #[test]
    fn a_plugin_cannot_reach_outside_its_own_folder() {
        let dir = scratch("escape");

        // A directory where the file should be.
        let as_dir = dir.join("as-dir");
        std::fs::create_dir_all(as_dir.join("vibedesk/panels.json")).unwrap();
        let (panels, issues) = scan(&[record("as-dir", &as_dir, true)], false);
        assert!(panels.is_empty());
        assert!(issues[0].contains("not a regular file"), "{issues:?}");

        // A `vibedesk` that is a file rather than a folder.
        let as_file = dir.join("as-file");
        std::fs::create_dir_all(&as_file).unwrap();
        std::fs::write(as_file.join("vibedesk"), "not a folder").unwrap();
        let (panels, issues) = scan(&[record("as-file", &as_file, true)], false);
        assert!(panels.is_empty());
        assert!(issues[0].contains("not a directory"), "{issues:?}");

        // A link to another plugin's panels.json. Creating one needs a
        // privilege Windows does not always grant, so the assertion is skipped
        // when the link cannot be made.
        let outside = plugin(&dir, "outside", &one_panel("ci", None));
        let thief = dir.join("thief");
        std::fs::create_dir_all(thief.join("vibedesk")).unwrap();
        let link = thief.join("vibedesk/panels.json");
        let target = Path::new(&outside.path).join("vibedesk/panels.json");
        if link_file(&target, &link) {
            let (panels, issues) = scan(&[record("thief", &thief, true)], false);
            assert!(panels.is_empty());
            assert!(issues[0].contains("not a regular file"), "{issues:?}");
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_plugin_name_with_a_separator_is_refused() {
        let dir = scratch("separator");
        let body = one_panel("ci", None);

        // The id is `plugin:<name>/<id>`, so a name carrying a separator would
        // make the split ambiguous. The folder is named plainly; only the
        // manifest name is hostile, which is the case a directory name cannot
        // produce on its own.
        let root = dir.join("plain");
        std::fs::create_dir_all(root.join("vibedesk")).unwrap();
        std::fs::write(root.join("vibedesk/panels.json"), &body).unwrap();

        let (panels, issues) = scan(&[record("evil/other", &root, true)], false);
        assert!(panels.is_empty(), "{panels:?}");
        assert!(issues[0].contains("path separator"), "{issues:?}");

        let (panels, issues) = scan(&[record("evil\\other", &root, true)], false);
        assert!(panels.is_empty(), "{panels:?}");
        assert!(issues[0].contains("path separator"), "{issues:?}");

        // The same folder under a plain name still loads, so the guard rejects
        // the name and not the plugin.
        let (panels, issues) = scan(&[record("plain", &root, true)], false);
        assert_eq!(panels.len(), 1, "{issues:?}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    fn link_file(from: &Path, to: &Path) -> bool {
        std::os::unix::fs::symlink(from, to).is_ok()
    }

    #[cfg(windows)]
    fn link_file(from: &Path, to: &Path) -> bool {
        std::os::windows::fs::symlink_file(from, to).is_ok()
    }

    /// The round trip that matters: the editor saves the whole list it was
    /// shown, so a plugin panel must be filtered out before the write and must
    /// not be in the file afterwards.
    #[test]
    fn saving_does_not_persist_a_plugin_panel() {
        let dir = scratch("save");
        let plugins = vec![plugin(&dir, "ci-tools", &one_panel("ci", None))];
        let (plugin_panels, _) = scan(&plugins, false);

        let user: PanelsConfig = serde_json::from_str(
            r#"{"panels":[{"id":"notes","title":"Notes","blocks":[{"kind":"text","text":"hi"}]}]}"#,
        )
        .unwrap();
        let mut shown = user.clone();
        shown.panels.extend(plugin_panels);

        let path = dir.join("panels.json");
        panels::save_panels_to(&path, &user_only(&shown)).unwrap();

        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(on_disk.contains("\"notes\""), "{on_disk}");
        assert!(!on_disk.contains("plugin:"), "{on_disk}");
        assert!(!on_disk.contains("\"plugin\""), "{on_disk}");

        // And the file that was written still loads, which is what a persisted
        // plugin panel would have broken.
        let reloaded = panels::load_panels_from(&path).unwrap();
        assert_eq!(reloaded.panels.len(), 1);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_plugin_without_panels_is_not_an_issue() {
        let dir = scratch("none");
        let bare = dir.join("bare");
        std::fs::create_dir_all(&bare).unwrap();

        let (panels, issues) = scan(&[record("bare", &bare, true)], false);
        assert!(panels.is_empty());
        assert!(issues.is_empty(), "{issues:?}");

        std::fs::remove_dir_all(&dir).ok();
    }
}
