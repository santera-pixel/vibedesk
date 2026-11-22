//! Runtime customization: themes today, layout and panels next.
//!
//! The rule this module enforces is that customization is **data, never code**.
//! A theme is a map of CSS custom properties plus a list of declaration blocks;
//! nothing here can execute, import, or fetch. That is what makes it safe to
//! accept a theme from a plugin or from the agent itself.
//!
//! Two classes of input are rejected outright:
//!
//! * **Value escapes** — `url()`, `@import`, `expression()`, `javascript:` and
//!   friends, which turn a stylesheet into a network client or a script host.
//! * **Protected surfaces** — the approval bar, the elicitation form, the
//!   status bar and the composer. A theme that could restyle those could make
//!   the UI lie about what the agent is asking, so no source may target them.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::paths;

/// Emitted whenever the theme on disk changes and validates.
pub const EVENT_CONFIG: &str = "config://changed";

/// A theme file larger than this is a mistake, not a theme.
pub const MAX_THEME_BYTES: u64 = 64 * 1024;
pub const MAX_VALUE_LEN: usize = 200;
pub const MAX_RULES: usize = 200;

/// Substrings that must never appear in a declaration value.
pub const FORBIDDEN_VALUE: &[&str] = &[
    "url(",
    "@import",
    "expression(",
    "javascript:",
    "vbscript:",
    "data:",
    "behavior:",
    "binding:",
    "\\",
    "<",
    ">",
];

/// Properties that can redraw or cover the interface rather than restyle it.
pub const FORBIDDEN_PROPERTY: &[&str] = &["position", "z-index", "pointer-events", "content"];

/// Selectors that may never be targeted, from any source.
pub const PROTECTED_SELECTORS: &[&str] = &[
    "vd-protected",
    ".vd-approval",
    ".vd-elicitation",
    ".vd-statusbar",
    ".vd-composer",
    // The swarm's approve control. A theme that could hide it would let a
    // stylesheet silently stop the user from approving a run — the same hazard
    // `.vd-approval` exists for, on the one surface that spends fifteen times a
    // chat's tokens.
    ".vd-swarm-approval",
];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ThemeConfig {
    /// CSS custom properties, e.g. `--vd-accent: #ff6b35`.
    pub tokens: BTreeMap<String, String>,
    /// Extra declaration blocks for the app's own classes.
    pub rules: Vec<ThemeRule>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ThemeRule {
    pub selector: String,
    pub declarations: BTreeMap<String, String>,
}

/// What the webview receives. Each field is `None` when the file on disk is
/// invalid, so the frontend keeps the last good value instead of flashing back
/// to the default on a typo. All four follow the same rule, and each is applied
/// independently: a broken theme must not stop a valid layout.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigPayload {
    pub theme: Option<ThemeConfig>,
    pub layout: Option<crate::layout::LayoutConfig>,
    pub actions: Option<crate::actions::ActionsConfig>,
    pub panels: Option<crate::panels::PanelsConfig>,
    pub error: Option<String>,
    pub source: String,
}

pub fn config_dir() -> PathBuf {
    paths::app_data_dir().join("config")
}

pub fn theme_path() -> PathBuf {
    config_dir().join("theme.json")
}

/// `VIBEDESK_SAFE_MODE=1` ignores every user, plugin and agent customization.
/// The escape hatch when a config makes the window unusable.
pub fn safe_mode() -> bool {
    matches!(
        std::env::var("VIBEDESK_SAFE_MODE").as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    )
}

/// Read the theme from disk. A missing file is an empty theme, not an error.
pub fn load_theme() -> Result<ThemeConfig, String> {
    load_theme_at(&theme_path(), safe_mode())
}

/// The safe-mode decision is a parameter rather than a call to `safe_mode()`, so
/// the bypass can be tested without writing to the real data directory.
pub fn load_theme_at(path: &std::path::Path, safe: bool) -> Result<ThemeConfig, String> {
    if safe {
        return Ok(ThemeConfig::default());
    }
    load_theme_from(path)
}

/// The path-taking half, so the loader can be tested without touching the
/// real data directory.
pub fn load_theme_from(path: &std::path::Path) -> Result<ThemeConfig, String> {
    let Ok(meta) = std::fs::metadata(path) else {
        return Ok(ThemeConfig::default());
    };
    if meta.len() > MAX_THEME_BYTES {
        return Err(format!(
            "theme.json is {} bytes; the limit is {}",
            meta.len(),
            MAX_THEME_BYTES
        ));
    }

    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("could not read {}: {e}", path.display()))?;
    if raw.trim().is_empty() {
        return Ok(ThemeConfig::default());
    }

    let theme: ThemeConfig = serde_json::from_str(&raw)
        .map_err(|e| format!("theme.json is not valid JSON: {e}"))?;
    validate(&theme)?;
    Ok(theme)
}

/// Validate, then write. Invalid input never reaches disk.
pub fn save_theme(theme: &ThemeConfig) -> Result<ThemeConfig, String> {
    save_theme_to(&theme_path(), theme)
}

pub fn save_theme_to(
    path: &std::path::Path,
    theme: &ThemeConfig,
) -> Result<ThemeConfig, String> {
    validate(theme)?;

    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    }

    let body = serde_json::to_string_pretty(theme)
        .map_err(|e| format!("could not serialise the theme: {e}"))?;
    if body.len() as u64 > MAX_THEME_BYTES {
        return Err(format!("theme is larger than {MAX_THEME_BYTES} bytes"));
    }

    // Write beside the target and rename, so a crash mid-write cannot leave a
    // half-written theme that the watcher would then apply.
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, &body)
        .map_err(|e| format!("could not write {}: {e}", temp.display()))?;
    std::fs::rename(&temp, path)
        .map_err(|e| format!("could not replace {}: {e}", path.display()))?;

    Ok(theme.clone())
}

pub fn reset_theme() -> Result<ThemeConfig, String> {
    reset_theme_at(&theme_path())
}

pub fn reset_theme_at(path: &std::path::Path) -> Result<ThemeConfig, String> {
    if path.exists() {
        std::fs::remove_file(path)
            .map_err(|e| format!("could not remove {}: {e}", path.display()))?;
    }
    Ok(ThemeConfig::default())
}

/// Every problem with a theme, so the UI can show all of them at once.
pub fn validate(theme: &ThemeConfig) -> Result<(), String> {
    let mut problems: Vec<String> = Vec::new();

    for (name, value) in &theme.tokens {
        if !is_token_name(name) {
            problems.push(format!(
                "`{name}` is not a VibeDesk token; names look like `--vd-accent`"
            ));
            continue;
        }
        if let Err(reason) = check_value(value) {
            problems.push(format!("{name}: {reason}"));
        }
    }

    if theme.rules.len() > MAX_RULES {
        problems.push(format!(
            "{} rules is more than the {MAX_RULES} allowed",
            theme.rules.len()
        ));
    }

    for (index, rule) in theme.rules.iter().enumerate() {
        let label = if rule.selector.trim().is_empty() {
            format!("rule {}", index + 1)
        } else {
            format!("rule `{}`", rule.selector)
        };

        if let Err(reason) = check_selector(&rule.selector) {
            problems.push(format!("{label}: {reason}"));
            continue;
        }
        for (property, value) in &rule.declarations {
            if !is_property_name(property) {
                problems.push(format!("{label}: `{property}` is not a CSS property name"));
                continue;
            }
            if FORBIDDEN_PROPERTY.contains(&property.as_str()) {
                problems.push(format!(
                    "{label}: `{property}` can cover the interface, so it is not allowed"
                ));
                continue;
            }
            if let Err(reason) = check_value(value) {
                problems.push(format!("{label}: {property}: {reason}"));
            }
        }
    }

    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("\n"))
    }
}

fn is_token_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("--vd-") else {
        return false;
    };
    !rest.is_empty()
        && rest
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn is_property_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn check_value(value: &str) -> Result<(), String> {
    if value.len() > MAX_VALUE_LEN {
        return Err(format!("value is longer than {MAX_VALUE_LEN} characters"));
    }
    let lower = value.to_ascii_lowercase();
    for needle in FORBIDDEN_VALUE {
        if lower.contains(needle) {
            return Err(format!("`{needle}` is not allowed in a value"));
        }
    }
    Ok(())
}

fn check_selector(selector: &str) -> Result<(), String> {
    let trimmed = selector.trim();
    if trimmed.is_empty() {
        return Err("selector is empty".into());
    }
    if trimmed.len() > MAX_VALUE_LEN {
        return Err(format!("selector is longer than {MAX_VALUE_LEN} characters"));
    }

    let lower = trimmed.to_ascii_lowercase();
    for needle in FORBIDDEN_VALUE {
        if lower.contains(needle) {
            return Err(format!("`{needle}` is not allowed in a selector"));
        }
    }
    for protected in PROTECTED_SELECTORS {
        if lower.contains(protected) {
            return Err(format!(
                "`{protected}` is a protected surface and cannot be restyled"
            ));
        }
    }

    // Each comma-separated part has to stand on its own. Checking only the
    // start of the whole selector let `.vd-card, body` through, which reaches
    // the document shell through the second part.
    for part in trimmed.split(',') {
        check_selector_part(part.trim())?;
    }

    Ok(())
}

/// Length of a selector's root prefix, if it starts with one.
fn root_prefix_len(selector: &str) -> Option<usize> {
    if selector.starts_with(":root") {
        return Some(5);
    }
    if selector.starts_with("html") {
        return Some(4);
    }
    if selector.starts_with("[data-theme") {
        return selector.find(']').map(|index| index + 1);
    }
    None
}

fn check_selector_part(part: &str) -> Result<(), String> {
    if part.is_empty() {
        return Err("selector has an empty part".into());
    }

    // The app's own classes. A descendant of one of these matches nothing
    // outside the app, so no further rule is needed.
    if part.starts_with(".vd-") {
        return Ok(());
    }

    let Some(len) = root_prefix_len(part) else {
        return Err(
            "selectors must start with `.vd-`, `:root`, `[data-theme` or `html`".into(),
        );
    };

    // A root prefix may be followed by a pseudo-class, an attribute or an app
    // class — never by a bare element, which is how `html body` would reach the
    // whole document.
    let rest = part[len..].trim_start();
    if rest.is_empty() {
        return Ok(());
    }
    let first = rest.chars().next().unwrap_or(' ');
    if matches!(first, ':' | '[' | '.') {
        return Ok(());
    }

    Err(format!(
        "`{}` may only be followed by a pseudo-class, an attribute or a `.vd-` class",
        &part[..len]
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme(tokens: &[(&str, &str)]) -> ThemeConfig {
        ThemeConfig {
            tokens: tokens
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            rules: Vec::new(),
        }
    }

    #[test]
    fn round_trips_a_theme_file() {
        let dir = std::env::temp_dir().join(format!("vibedesk-theme-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("theme.json");

        std::fs::write(&path, r##"{"tokens":{"--vd-accent":"#ff6b35"}}"##).unwrap();
        let theme = load_theme_from(&path).unwrap();
        assert_eq!(
            theme.tokens.get("--vd-accent").map(String::as_str),
            Some("#ff6b35")
        );

        // A bad value must be reported, not silently applied.
        std::fs::write(&path, r#"{"tokens":{"--vd-accent":"url(http://x)"}}"#).unwrap();
        assert!(load_theme_from(&path).is_err());

        // A missing file is an empty theme, not an error.
        std::fs::remove_file(&path).unwrap();
        assert!(load_theme_from(&path).unwrap().tokens.is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn accepts_a_plain_token() {
        assert!(validate(&theme(&[("--vd-accent", "#ff6b35")])).is_ok());
    }

    #[test]
    fn rejects_unknown_token_names() {
        assert!(validate(&theme(&[("--accent", "#fff")])).is_err());
        assert!(validate(&theme(&[("--vd-", "#fff")])).is_err());
    }

    #[test]
    fn rejects_network_and_script_values() {
        for value in [
            "url(https://evil.test/x)",
            "expression(alert(1))",
            "javascript:alert(1)",
            "@import 'x'",
        ] {
            assert!(
                validate(&theme(&[("--vd-accent", value)])).is_err(),
                "{value} should be rejected"
            );
        }
    }

    #[test]
    fn rejects_protected_surfaces() {
        let rule = ThemeRule {
            selector: ".vd-approval .vd-btn".into(),
            declarations: BTreeMap::from([("color".into(), "red".into())]),
        };
        let mut config = ThemeConfig::default();
        config.rules.push(rule);
        assert!(validate(&config).is_err());
    }

    #[test]
    fn rejects_layout_hijacking_properties() {
        let rule = ThemeRule {
            selector: ".vd-card".into(),
            declarations: BTreeMap::from([("position".into(), "fixed".into())]),
        };
        let mut config = ThemeConfig::default();
        config.rules.push(rule);
        assert!(validate(&config).is_err());
    }

    #[test]
    fn accepts_a_normal_rule() {
        let rule = ThemeRule {
            selector: ".vd-btn-primary".into(),
            declarations: BTreeMap::from([("letter-spacing".into(), "0.02em".into())]),
        };
        let mut config = ThemeConfig::default();
        config.rules.push(rule);
        assert!(validate(&config).is_ok());
    }
}
