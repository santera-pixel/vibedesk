//! What the agent will actually accept, as opposed to what config.toml declares.
//!
//! The swarm used to pick models from `config.toml`'s `[[models]]` array. That
//! array is the *user's* declarations, not the agent's catalogue: this machine
//! declares two models and the agent offers six, so a run could only ever use
//! two of them and the panel could not show the other four at all.
//!
//! The agent is the authority. `session/new` returns a `configOptions` array
//! whose `model` option lists every alias the session will accept, and whose
//! `thinking` option lists the reasoning levels — **per model**, which is the
//! fact that shapes this module. `glm-5-3` accepts `low, high, max`;
//! `mistral-medium-3.5` accepts only `off, high`; `deepseek` accepts all five.
//! A level that is valid for one model is refused by another, so a model and a
//! level are one choice, not two.
//!
//! Two sources, one shape:
//!
//! * [`Catalog::from_agent`] reads a live session's `configOptions`. This is the
//!   truth, and it is free — the window already has it.
//! * [`Catalog::from_config`] reads `config.toml`. This is the fallback for a
//!   headless run, which has no session to ask, and it carries no thinking
//!   levels because the file does not declare them per model.
//!
//! [`Catalog::merge`] prefers the agent and keeps any config-only alias, so a
//! model the user declared but the agent has not loaded still appears — and
//! [`Catalog::level_for`] answers "is this pair legal" without a round trip.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One model the agent will accept, and the reasoning levels it accepts with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelOption {
    /// The alias `session/set_config_option` takes. This is the value, not the
    /// display name: `glm-5-3` is accepted, `GLM-5.3 (Mistral Hosted)` is not.
    pub value: String,
    /// What the panel shows.
    pub name: String,
    /// The underlying model id, when the agent reports one. `glm-5-3` is an
    /// alias for `zai-glm-5-3`, and a roster that shows only the alias hides
    /// which model actually ran.
    #[serde(default)]
    pub detail: Option<String>,
    /// The reasoning levels this model accepts, in the agent's order.
    ///
    /// Empty means the agent did not say — a config-only model, or a session
    /// that reports no `thinking` option. Empty is not "no levels": it is
    /// "unknown", and [`Catalog::level_for`] treats it as "do not constrain".
    #[serde(default)]
    pub thinking: Vec<String>,
    /// The level this model is configured to run at.
    ///
    /// The one level there is evidence for when [`Self::thinking`] is empty: the
    /// user wrote it for this model, so the provider accepts it. A role default
    /// is a guess, and a guess sent to a model whose levels are unknown is how a
    /// worker asking for `medium` reached a model that only offers
    /// `none, low, high, xhigh, max` and came back a 400.
    #[serde(default)]
    pub default: Option<String>,
}

impl ModelOption {
    /// Whether this model accepts a reasoning level.
    ///
    /// An empty level list means unknown, so any level is allowed and the
    /// session's own check is the guard. A non-empty list is a real constraint.
    pub fn accepts(&self, level: &str) -> bool {
        self.thinking.is_empty() || self.thinking.iter().any(|l| l == level)
    }

    /// The level to use when none was asked for.
    ///
    /// The agent's own default is the last word, but it is not in this struct —
    /// so the middle of the list is the honest guess: it is a level the model
    /// accepts, and it is not the cheapest or the most expensive.
    pub fn default_thinking(&self) -> Option<String> {
        if self.thinking.is_empty() {
            return None;
        }
        Some(self.thinking[self.thinking.len() / 2].clone())
    }

    /// The level to use when this model's levels are unknown.
    ///
    /// The configured level, because it is the only one the provider is known to
    /// accept for this model. `None` when the config declares none either, which
    /// leaves the caller with nothing better than the level it asked for.
    pub fn declared(&self) -> Option<String> {
        self.default
            .as_deref()
            .map(str::trim)
            .filter(|level| !level.is_empty())
            .map(str::to_string)
    }
}

/// Every model the swarm may assign, with the levels each one accepts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Catalog {
    pub models: Vec<ModelOption>,
    /// Where the list came from, for the panel to say so.
    #[serde(default)]
    pub source: Source,
}

/// Which authority produced a catalogue.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Source {
    /// Read from a live session's `configOptions`.
    Agent,
    /// Read from `config.toml`'s `[[models]]`, with the levels the file declares.
    Config,
    /// The agent's list, with config-only aliases appended.
    #[default]
    Merged,
}

impl Catalog {
    /// Read a live session's `configOptions`.
    ///
    /// The shape is the one `session/new` returns and
    /// `session/set_config_option` echoes back. A session that reports no
    /// `model` option yields an empty catalogue rather than an error: the caller
    /// falls back to config.toml, and a member that cannot be given a model is
    /// still a member that can run on the session default.
    pub fn from_agent(session: &Value) -> Self {
        let options = session
            .get("configOptions")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        let thinking = options
            .iter()
            .find(|option| option.get("id").and_then(Value::as_str) == Some("thinking"))
            .and_then(|option| option.get("options"))
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(|v| v.get("value").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        // The level the session is on now. It is the active model's, so it is
        // only a fallback for a model whose own levels the session did not
        // report — which is the case this field exists for.
        let current = options
            .iter()
            .find(|option| option.get("id").and_then(Value::as_str) == Some("thinking"))
            .and_then(|option| option.get("currentValue"))
            .and_then(Value::as_str)
            .map(str::to_string);

        let models = options
            .iter()
            .find(|option| option.get("id").and_then(Value::as_str) == Some("model"))
            .and_then(|option| option.get("options"))
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(|v| {
                        let value = v.get("value").and_then(Value::as_str)?.to_string();
                        let name = v
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or(&value)
                            .to_string();
                        let detail = v
                            .get("description")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                            .filter(|d| !d.is_empty() && d != &value);
                        Some(ModelOption {
                            value,
                            name,
                            detail,
                            // The session's `thinking` option is the list for the
                            // model it is *currently* on. A catalogue read before
                            // any model switch therefore carries the levels of
                            // the active model only, which is why the panel asks
                            // the agent again after a switch rather than trusting
                            // this list for every model.
                            thinking: thinking.clone(),
                            default: current.clone(),
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        Self {
            models,
            source: Source::Agent,
        }
    }

    /// Read `config.toml`'s `[[models]]`.
    ///
    /// The levels come from the file's own `thinking_levels`, which is the only
    /// place a locally configured model can state them: the agent's catalogue
    /// narrows a model it knows, and a model the user added is not in it. An
    /// entry that declares none leaves the set empty, and `settle` then treats
    /// the level as unconstrained rather than inventing a set the agent would
    /// refuse.
    pub fn from_config(config: &crate::config::VibeConfig) -> Self {
        let models = config
            .models
            .iter()
            .map(|model| ModelOption {
                value: model.alias.clone(),
                name: model
                    .display_name
                    .clone()
                    .unwrap_or_else(|| model.alias.clone()),
                detail: Some(model.name.clone()).filter(|n| n != &model.alias),
                thinking: model.thinking_levels.clone(),
                default: model.thinking.clone(),
            })
            .collect();

        Self {
            models,
            source: Source::Config,
        }
    }

    /// The agent's list, with any config-only alias appended.
    ///
    /// The agent wins on a shared alias because it is the side that validates.
    /// A config alias the agent does not offer is kept and marked by its empty
    /// level list, so the panel can show it and the roster can report that the
    /// member fell back to the session default — which is what actually happens.
    pub fn merge(agent: Catalog, config: Catalog) -> Self {
        if agent.models.is_empty() {
            return config;
        }
        if config.models.is_empty() {
            return agent;
        }

        let mut models = agent.models;
        for candidate in config.models {
            if !models.iter().any(|m| m.value == candidate.value) {
                models.push(candidate);
            }
        }

        Self {
            models,
            source: Source::Merged,
        }
    }

    /// Whether the catalogue has anything to assign.
    pub fn is_empty(&self) -> bool {
        self.models.is_empty()
    }

    /// The aliases, in the agent's order.
    pub fn aliases(&self) -> Vec<String> {
        self.models.iter().map(|m| m.value.clone()).collect()
    }

    /// One model by alias.
    pub fn get(&self, alias: &str) -> Option<&ModelOption> {
        self.models.iter().find(|m| m.value == alias)
    }

    /// The reasoning levels a model accepts.
    pub fn levels(&self, alias: &str) -> Vec<String> {
        self.get(alias)
            .map(|m| m.thinking.clone())
            .unwrap_or_default()
    }

    /// Whether a model accepts a reasoning level.
    ///
    /// An unknown model is not a constraint: the caller has already failed to
    /// find it, and refusing here would turn a missing catalogue entry into a
    /// run that cannot start.
    pub fn level_for(&self, alias: &str, level: &str) -> bool {
        match self.get(alias) {
            Some(model) => model.accepts(level),
            None => true,
        }
    }

    /// A level that is legal for a model, preferring the one asked for.
    ///
    /// This is the function that keeps a run from being refused by the agent:
    /// a level chosen for one model is not necessarily legal for another, and a
    /// member that asks for an illegal level is a member that silently keeps the
    /// session default. Falling back to the model's own default is the honest
    /// repair, and the roster records what was actually applied.
    ///
    /// `asked` is `false` when the level came from a role default rather than
    /// from the user. That distinction is the whole point when the model's levels
    /// are unknown: a role default is this program's guess, so it is replaced by
    /// the level the config declares for the model — the one level there is
    /// evidence for. A level the user asked for is kept, because overriding an
    /// explicit request would be the program deciding it knows better.
    pub fn settle(&self, alias: &str, wanted: &str, asked: bool) -> Option<String> {
        let wanted = wanted.trim();
        let Some(model) = self.get(alias) else {
            // An unknown model is not a constraint: the caller has already
            // failed to find it, and refusing here would turn a missing
            // catalogue entry into a run that cannot start.
            return (!wanted.is_empty()).then(|| wanted.to_string());
        };
        if model.thinking.is_empty() {
            // Unknown levels. The declared level is the only one with evidence
            // behind it, so a guess is replaced by it and an explicit request is
            // left alone.
            if !asked {
                if let Some(declared) = model.declared() {
                    return Some(declared);
                }
            }
            return (!wanted.is_empty()).then(|| wanted.to_string());
        }
        if !wanted.is_empty() && model.accepts(wanted) {
            return Some(wanted.to_string());
        }
        model.default_thinking()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The exact shape `session/new` returns on this machine, trimmed to the two
    /// options this module reads.
    fn session(models: &[(&str, &str, &str)], thinking: &[&str]) -> Value {
        json!({
            "configOptions": [
                {
                    "id": "model",
                    "name": "Model",
                    "type": "select",
                    "currentValue": models.first().map(|m| m.0).unwrap_or(""),
                    "options": models
                        .iter()
                        .map(|(value, name, detail)| json!({
                            "value": value,
                            "name": name,
                            "description": detail,
                        }))
                        .collect::<Vec<_>>(),
                },
                {
                    "id": "thinking",
                    "name": "Thinking",
                    "type": "select",
                    "currentValue": thinking.first().copied().unwrap_or(""),
                    "options": thinking
                        .iter()
                        .map(|value| json!({ "value": value, "name": value }))
                        .collect::<Vec<_>>(),
                },
            ]
        })
    }

    #[test]
    fn the_agent_catalogue_is_read_from_the_session_it_returns() {
        let catalog = Catalog::from_agent(&session(
            &[
                ("glm-5-3", "GLM-5.3 (Mistral Hosted)", "zai-glm-5-3"),
                ("deepseek", "DeepSeek V4.1 Flash 1017", "deepseek-v4-1-flash-1017"),
            ],
            &["low", "high", "max"],
        ));

        assert_eq!(catalog.source, Source::Agent);
        assert_eq!(catalog.aliases(), vec!["glm-5-3", "deepseek"]);
        assert_eq!(catalog.get("glm-5-3").unwrap().name, "GLM-5.3 (Mistral Hosted)");
        // The alias is what the agent accepts; the detail is what actually runs.
        assert_eq!(catalog.get("glm-5-3").unwrap().detail.as_deref(), Some("zai-glm-5-3"));
        assert_eq!(catalog.levels("glm-5-3"), vec!["low", "high", "max"]);
    }

    #[test]
    fn a_description_that_repeats_the_value_is_not_a_detail() {
        // `zai-glm-5-3-flash` is its own description. Showing it twice reads as
        // two different facts.
        let catalog = Catalog::from_agent(&session(
            &[("zai-glm-5-3-flash", "GLM-5.3 Flash", "zai-glm-5-3-flash")],
            &["low"],
        ));
        assert_eq!(catalog.get("zai-glm-5-3-flash").unwrap().detail, None);
    }

    #[test]
    fn a_session_with_no_model_option_is_an_empty_catalogue_not_an_error() {
        let catalog = Catalog::from_agent(&json!({ "configOptions": [] }));
        assert!(catalog.is_empty());
        assert_eq!(catalog.source, Source::Agent);
    }

    #[test]
    fn a_session_that_reports_no_thinking_option_leaves_levels_unknown() {
        let catalog = Catalog::from_agent(&json!({
            "configOptions": [{
                "id": "model",
                "options": [{ "value": "deepseek", "name": "DeepSeek" }],
            }]
        }));
        assert_eq!(catalog.levels("deepseek"), Vec::<String>::new());
        // Unknown is not a constraint: the session's own check is the guard.
        assert!(catalog.level_for("deepseek", "max"));
    }

    #[test]
    fn a_level_is_legal_only_for_the_model_that_accepts_it() {
        // The fact that shaped this module: the levels are per model, so the
        // same level is legal on one and refused by another.
        let catalog = Catalog {
            models: vec![
                ModelOption {
                    value: "glm-5-3".into(),
                    name: "GLM".into(),
                    detail: None,
                    thinking: vec!["low".into(), "high".into(), "max".into()],
                    default: None,
                },
                ModelOption {
                    value: "mistral-medium-3.5".into(),
                    name: "Medium".into(),
                    detail: None,
                    thinking: vec!["off".into(), "high".into()],
                    default: None,
                },
            ],
            source: Source::Agent,
        };

        assert!(catalog.level_for("glm-5-3", "max"));
        assert!(!catalog.level_for("mistral-medium-3.5", "max"));
        assert!(catalog.level_for("mistral-medium-3.5", "off"));
        assert!(!catalog.level_for("glm-5-3", "off"));
    }

    #[test]
    fn settling_a_level_repairs_one_the_model_refuses() {
        let catalog = Catalog {
            models: vec![ModelOption {
                value: "mistral-medium-3.5".into(),
                name: "Medium".into(),
                detail: None,
                thinking: vec!["off".into(), "high".into()],
                default: None,
            }],
            source: Source::Agent,
        };

        // Asked for a level this model does not have: fall back to its own.
        assert_eq!(catalog.settle("mistral-medium-3.5", "max", true).as_deref(), Some("high"));
        // Asked for one it does have: keep it.
        assert_eq!(catalog.settle("mistral-medium-3.5", "off", true).as_deref(), Some("off"));
        // Asked for nothing: the model's default.
        assert_eq!(catalog.settle("mistral-medium-3.5", "", false).as_deref(), Some("high"));
    }

    #[test]
    fn settling_an_unknown_model_keeps_what_was_asked_for() {
        let catalog = Catalog::default();
        assert_eq!(catalog.settle("mystery", "high", true).as_deref(), Some("high"));
        assert_eq!(catalog.settle("mystery", "", false), None);
    }

    #[test]
    fn the_default_level_is_one_the_model_accepts_and_is_not_an_extreme() {
        let model = ModelOption {
            value: "deepseek".into(),
            name: "DeepSeek".into(),
            detail: None,
            thinking: vec![
                "off".into(),
                "low".into(),
                "medium".into(),
                "high".into(),
                "max".into(),
            ],
            default: None,
        };
        assert_eq!(model.default_thinking().as_deref(), Some("medium"));

        let two = ModelOption {
            thinking: vec!["off".into(), "high".into()],
            ..model
        };
        assert_eq!(two.default_thinking().as_deref(), Some("high"));
    }

    #[test]
    fn merging_keeps_the_agents_list_and_appends_a_config_only_alias() {
        let agent = Catalog {
            models: vec![ModelOption {
                value: "glm-5-3".into(),
                name: "GLM".into(),
                detail: None,
                thinking: vec!["high".into()],
                default: None,
            }],
            source: Source::Agent,
        };
        let config = Catalog {
            models: vec![
                ModelOption {
                    value: "glm-5-3".into(),
                    name: "GLM from config".into(),
                    detail: None,
                    thinking: Vec::new(),
                    default: None,
                },
                ModelOption {
                    value: "local-only".into(),
                    name: "Local".into(),
                    detail: None,
                    thinking: Vec::new(),
                    default: None,
                },
            ],
            source: Source::Config,
        };

        let merged = Catalog::merge(agent, config);
        assert_eq!(merged.source, Source::Merged);
        assert_eq!(merged.aliases(), vec!["glm-5-3", "local-only"]);
        // The agent wins on a shared alias, because it is the side that validates.
        assert_eq!(merged.get("glm-5-3").unwrap().name, "GLM");
        assert_eq!(merged.levels("glm-5-3"), vec!["high"]);
    }

    #[test]
    fn merging_an_empty_agent_catalogue_is_the_config_catalogue() {
        let config = Catalog {
            models: vec![ModelOption {
                value: "deepseek".into(),
                name: "DeepSeek".into(),
                detail: None,
                thinking: Vec::new(),
                default: None,
            }],
            source: Source::Config,
        };
        assert_eq!(Catalog::merge(Catalog::default(), config.clone()), config);
    }

    #[test]
    fn merging_an_empty_config_catalogue_is_the_agent_catalogue() {
        let agent = Catalog {
            models: vec![ModelOption {
                value: "deepseek".into(),
                name: "DeepSeek".into(),
                detail: None,
                thinking: vec!["high".into()],
                default: None,
            }],
            source: Source::Agent,
        };
        assert_eq!(Catalog::merge(agent.clone(), Catalog::default()), agent);
    }

    #[test]
    fn the_config_catalogue_carries_the_levels_the_file_declares() {
        let mut config = crate::config::VibeConfig::default();
        config.models.push(crate::config::ModelConfig {
            name: "deepseek-v4-1-flash-1017".into(),
            provider: "mistral".into(),
            alias: "deepseek".into(),
            display_name: Some("DeepSeek V4.1 Flash 1017".into()),
            thinking: Some("high".into()),
            thinking_levels: vec![
                "off".into(),
                "low".into(),
                "high".into(),
                "xhigh".into(),
                "max".into(),
            ],
            supports_images: true,
            max_context_length: Some(1048576),
        });

        let catalog = Catalog::from_config(&config);
        assert_eq!(catalog.source, Source::Config);
        assert_eq!(catalog.aliases(), vec!["deepseek"]);
        assert_eq!(catalog.get("deepseek").unwrap().name, "DeepSeek V4.1 Flash 1017");
        assert_eq!(catalog.get("deepseek").unwrap().detail.as_deref(), Some("deepseek-v4-1-flash-1017"));
        // The file's set is the model's set, so a level outside it is repaired
        // rather than sent: `medium` is the level this model rejects upstream.
        assert_eq!(catalog.levels("deepseek"), vec!["off", "low", "high", "xhigh", "max"]);
        assert_eq!(catalog.settle("deepseek", "medium", true).as_deref(), Some("high"));
        assert_eq!(catalog.settle("deepseek", "low", true).as_deref(), Some("low"));
    }

    #[test]
    fn a_config_entry_that_declares_no_levels_leaves_the_level_unconstrained() {
        let mut config = crate::config::VibeConfig::default();
        config.models.push(crate::config::ModelConfig {
            name: "deepseek-v4-1-flash-1017".into(),
            provider: "mistral".into(),
            alias: "deepseek".into(),
            display_name: None,
            thinking: Some("high".into()),
            thinking_levels: Vec::new(),
            supports_images: true,
            max_context_length: None,
        });

        let catalog = Catalog::from_config(&config);
        // `thinking = "high"` is a default, not the set of levels accepted.
        assert!(catalog.levels("deepseek").is_empty());
        // An explicit request is the user's, so it stands even though nothing is
        // known about the model.
        assert_eq!(catalog.settle("deepseek", "medium", true).as_deref(), Some("medium"));
        // A role default is this program's guess, and a guess is not worth a
        // refusal: the level the file declares for the model is the one level
        // there is evidence for, so it replaces the guess.
        assert_eq!(catalog.settle("deepseek", "medium", false).as_deref(), Some("high"));
        assert_eq!(catalog.settle("deepseek", "low", false).as_deref(), Some("high"));
    }

    #[test]
    fn a_config_entry_with_no_levels_and_no_declared_level_keeps_the_guess() {
        let mut config = crate::config::VibeConfig::default();
        config.models.push(crate::config::ModelConfig {
            name: "mystery-model".into(),
            provider: "mistral".into(),
            alias: "mystery".into(),
            display_name: None,
            thinking: None,
            thinking_levels: Vec::new(),
            supports_images: false,
            max_context_length: None,
        });

        let catalog = Catalog::from_config(&config);
        // Nothing is known and nothing is declared, so there is nothing better
        // than the level the role asked for.
        assert_eq!(catalog.settle("mystery", "medium", false).as_deref(), Some("medium"));
    }
}

/* ------------------------------------------------------------------ *
 * Reading the catalogue from a real agent
 * ------------------------------------------------------------------ */

/// Read every model's reasoning levels from a real `vibe-acp`.
///
/// The session reports the levels of the model it is *currently* on, so the only
/// way to learn the rest is to switch to each one and read again. That is seven
/// round trips and one process — about four seconds — which is why the caller
/// caches the answer rather than asking per render.
///
/// The probe runs in a scratch directory, not the workspace: a catalogue does
/// not depend on the project, and a probe that opened a session in the user's
/// repository would leave a journal there for a question about models.
pub async fn probe() -> Result<Catalog, String> {
    use crate::acp::bridge::{AcpBridge, RecordingSink};
    use std::sync::Arc;

    let scratch = std::env::temp_dir().join("vibedesk-catalog-probe");
    std::fs::create_dir_all(&scratch)
        .map_err(|e| format!("could not make a probe directory: {e}"))?;

    let bridge = AcpBridge::with_sink(Arc::new(RecordingSink::default()));
    bridge
        .start(Some(scratch.clone()))
        .await
        .map_err(|e| format!("the agent did not start: {e}"))?;

    let session = bridge
        .request(
            "session/new",
            serde_json::json!({
                "cwd": scratch.to_string_lossy(),
                "mcpServers": [],
            }),
        )
        .await
        .map_err(|e| format!("the agent did not open a session: {e}"))?;

    let session_id = session
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or_else(|| "the agent opened a session with no id".to_string())?
        .to_string();

    let mut catalog = Catalog::from_agent(&session);

    // Each model's own levels. A switch the agent refuses leaves the levels it
    // already had, which is the active model's — wrong, but the only thing known,
    // and `settle` repairs an illegal level at spawn time either way.
    for model in &mut catalog.models {
        let Ok(result) = bridge
            .request(
                "session/set_config_option",
                serde_json::json!({
                    "sessionId": session_id,
                    "configId": "model",
                    "value": model.value,
                }),
            )
            .await
        else {
            continue;
        };
        if let Some(levels) = levels_of(&result) {
            model.thinking = levels;
        }
    }

    let _ = bridge.stop().await;
    Ok(catalog)
}

/// The `thinking` option's values in a session or a config-option result.
fn levels_of(session: &Value) -> Option<Vec<String>> {
    session
        .get("configOptions")
        .and_then(Value::as_array)?
        .iter()
        .find(|option| option.get("id").and_then(Value::as_str) == Some("thinking"))?
        .get("options")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|v| v.get("value").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
}
