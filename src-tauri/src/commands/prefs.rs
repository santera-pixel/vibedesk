use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};
use crate::settings;

/// VibeDesk's own preferences, as the webview sees them.
///
/// One struct rather than a command per flag: the settings file is written
/// whole, so a second flag would otherwise mean a second read-modify-write
/// racing the first. Adding a field here is a one-line change on both sides.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Prefs {
    pub creator_mode: bool,
    /// Whether a swarm run is handed the installed skills.
    ///
    /// Defaults to on, and the default is written out rather than derived: a
    /// `Prefs` deserialised from a body that predates the field must not read as
    /// "off", because that would switch the feature off for every existing user
    /// the first time they touched an unrelated preference.
    #[serde(default = "on_by_default")]
    pub swarm_skills: bool,
}

fn on_by_default() -> bool {
    true
}

impl From<&settings::Settings> for Prefs {
    fn from(settings: &settings::Settings) -> Self {
        Prefs {
            creator_mode: settings.creator_mode,
            swarm_skills: settings.swarm_skills,
        }
    }
}

#[tauri::command]
pub fn prefs_get() -> Prefs {
    Prefs::from(&settings::load())
}

#[tauri::command]
pub fn prefs_set(prefs: Prefs) -> AppResult<Prefs> {
    let mut current = settings::load();
    current.creator_mode = prefs.creator_mode;
    current.swarm_skills = prefs.swarm_skills;
    settings::save(&current).map_err(AppError::message)?;
    Ok(Prefs::from(&current))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefs_mirror_the_settings_field() {
        let settings = settings::Settings {
            creator_mode: true,
            ..Default::default()
        };
        assert!(Prefs::from(&settings).creator_mode);
    }

    #[test]
    fn prefs_serialize_as_camel_case() {
        let body = serde_json::to_string(&Prefs {
            creator_mode: true,
            swarm_skills: false,
        })
        .unwrap();
        assert_eq!(body, r#"{"creatorMode":true,"swarmSkills":false}"#);
    }

    #[test]
    fn a_prefs_body_without_the_field_defaults_to_off() {
        let prefs: Prefs = serde_json::from_str("{}").unwrap();
        assert!(!prefs.creator_mode);
    }

    #[test]
    fn a_prefs_body_without_the_skill_field_leaves_the_swarm_skills_on() {
        // The field is new. A body written before it existed must not read as
        // "off", or the first unrelated preference write would switch the
        // feature off for every existing user.
        let prefs: Prefs = serde_json::from_str(r#"{"creatorMode":false}"#).unwrap();
        assert!(prefs.swarm_skills);
    }
}
