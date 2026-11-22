use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::paths;

/// VibeDesk's own preferences. Deliberately separate from `~/.vibe/config.toml`:
/// the agent owns that file, and writing to it behind the user's back would be
/// a surprise.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub last_workspace: Option<String>,
    pub last_update_check: Option<String>,
    pub latest_version: Option<String>,
    pub theme: Option<String>,
    pub dismissed_update: Option<String>,
    /// Creator mode: the agent is told it is building VibeDesk's own interface
    /// with the user, so it proposes instead of describing. Persisted because a
    /// mode that resets on every launch is a mode the user has to re-arm.
    pub creator_mode: bool,
    /// Folders the user pinned as workstations, in the order they added them.
    pub workstations: Vec<Workstation>,
    /// Whether a swarm run is handed the installed skills.
    ///
    /// On by default, because the alternative is the behaviour that made this
    /// necessary: a run that reinvents a method the machine already has, at
    /// fifteen times the token cost of a chat. The switch exists because a user
    /// comparing two runs needs to be able to turn the method off, and because a
    /// skill that is wrong about a repository should be removable from the
    /// equation without uninstalling it.
    ///
    /// `default_true` rather than `bool`, because `#[serde(default)]` on the
    /// struct means a settings file written before this field existed would
    /// otherwise read as `false` — silently switching the feature off for every
    /// existing user.
    #[serde(default = "default_true")]
    pub swarm_skills: bool,
}

fn default_true() -> bool {
    true
}

/// The defaults a machine with no `settings.json` gets.
///
/// Written out rather than derived, because a derived `Default` gives every
/// `bool` the value `false` — including `swarm_skills`, whose whole point is
/// that it is on. `#[serde(default = "default_true")]` does not save it: that
/// attribute only applies when *deserialising* an object that lacks the key, and
/// `load()` returns this value when the file is missing or unreadable. A fresh
/// install has no `settings.json` until the user changes something, so the
/// derived default silently ran every swarm with no skills at all — the exact
/// behaviour the field's own doc comment says it exists to prevent.
impl Default for Settings {
    fn default() -> Self {
        Self {
            last_workspace: None,
            last_update_check: None,
            latest_version: None,
            theme: None,
            dismissed_update: None,
            creator_mode: false,
            workstations: Vec::new(),
            swarm_skills: true,
        }
    }
}

/// A pinned project folder. The path is canonical, so it doubles as the key.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Workstation {
    pub path: String,
    pub name: String,
    pub added_at: String,
}

pub fn load() -> Settings {
    let path = paths::settings_path();
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Settings::default();
    };
    let mut settings: Settings = serde_json::from_str(&raw).unwrap_or_default();
    // Builds before this one stored `canonicalize` output, which on Windows is a
    // `\\?\` verbatim path. Normalise on read so an existing settings file is
    // repaired without the user re-pinning anything.
    let last = settings
        .last_workspace
        .as_deref()
        .map(paths::strip_verbatim);
    settings.last_workspace = last;
    for entry in &mut settings.workstations {
        entry.path = paths::strip_verbatim(&entry.path);
    }
    settings
}

pub fn save(settings: &Settings) -> Result<(), String> {
    let path = paths::settings_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let body = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    std::fs::write(&path, body).map_err(|e| e.to_string())
}

pub fn remember_workspace(path: &PathBuf) {
    let mut settings = load();
    let value = paths::strip_verbatim(&path.to_string_lossy());
    if settings.last_workspace.as_deref() == Some(value.as_str()) {
        return;
    }
    settings.last_workspace = Some(value);
    let _ = save(&settings);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The file format is the contract: a choice written by one build has to
    /// read back unchanged in the next, under the camelCase key the webview
    /// sends.
    #[test]
    fn settings_round_trip_keeps_the_creator_flag() {
        let settings = Settings {
            creator_mode: true,
            ..Default::default()
        };
        let body = serde_json::to_string(&settings).unwrap();
        assert!(body.contains(r#""creatorMode":true"#), "{body}");
        let back: Settings = serde_json::from_str(&body).unwrap();
        assert!(back.creator_mode);
    }

    /// A settings file written by a build that still had a motion policy must
    /// load without it. The field is gone, and an unknown key is not an error.
    #[test]
    fn a_settings_file_with_a_retired_motion_key_still_loads() {
        let back: Settings =
            serde_json::from_str(r#"{"creatorMode":true,"motion":"reduced"}"#).unwrap();
        assert!(back.creator_mode);
    }
}

#[cfg(test)]
mod default_tests {
    use super::*;

    /// A machine with no `settings.json` must still hand a run its skills.
    ///
    /// This is the bug the manual `Default` fixes: a derived one gave
    /// `swarm_skills: false`, and a fresh install has no settings file, so every
    /// swarm on a new machine ran with an empty catalogue and the lead invented
    /// skill names from whatever it had read.
    #[test]
    fn a_missing_settings_file_still_hands_a_run_its_skills() {
        assert!(Settings::default().swarm_skills);
    }

    /// And a file written before the field existed reads the same way.
    #[test]
    fn an_older_settings_file_reads_the_field_as_on() {
        let parsed: Settings = serde_json::from_str(r#"{"theme":"dark"}"#).unwrap();
        assert!(parsed.swarm_skills);
        assert_eq!(parsed.theme.as_deref(), Some("dark"));
    }

    /// An explicit `false` is still honoured: the switch has to work.
    #[test]
    fn an_explicit_off_is_kept() {
        let parsed: Settings = serde_json::from_str(r#"{"swarmSkills":false}"#).unwrap();
        assert!(!parsed.swarm_skills);
    }
}
