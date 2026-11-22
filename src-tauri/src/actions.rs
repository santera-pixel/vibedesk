//! Custom actions: buttons the user, a plugin, or the agent can add.
//!
//! An action is a label plus one of seven things it can do. The set of things is
//! **closed** — `run.kind` is a tagged enum, not a string, so a file cannot
//! invent an eighth behaviour. That is the same rule the theme module follows:
//! customization is data, never code.
//!
//! The one place this could have gone wrong is `command`. A button that ran an
//! arbitrary shell line would turn `actions.json` into a script host, and the
//! whole point of validating a theme is lost if the file next to it can execute.
//! So `command` names one of VibeDesk's own commands from a closed enum. A user
//! who wants a shell command has the agent for that, with the approval prompt in
//! between — which is exactly the boundary this module refuses to move.
//!
//! `open-url` and `reveal` are the two kinds that reach outside the app, so both
//! are narrowed: a URL must be http(s), and a revealed path must be relative and
//! stay inside the workspace.
//!
//! `mode`, `option` and `cancel` are the three that reach *into* the agent, and
//! they exist because "control the agent" used to mean "send it text" and
//! nothing else. They are safe for the same reason `command` is: `mode` and
//! `option` name ids the agent itself advertised in `availableModes` and
//! `configOptions`, so a file chooses among real modes rather than inventing
//! one, and `cancel` is the same call the stop button makes. None of them can
//! widen what the agent is allowed to do — they can only pick from what it
//! already offers.

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// An actions file larger than this is a mistake, not a set of buttons.
pub const MAX_ACTIONS_BYTES: u64 = 16 * 1024;
pub const MAX_ACTIONS: usize = 24;
pub const MAX_ID: usize = 32;
pub const MAX_LABEL: usize = 40;
const MAX_PROMPT: usize = 4000;
const MAX_URL: usize = 2048;
const MAX_PATH: usize = 260;
/// A mode id or a config-option id. Both are agent-authored tokens, not prose.
const MAX_TOKEN: usize = 64;
/// An elicitation form's values, serialised. A button that submits more than
/// this is a button that should have been a form.
const MAX_ELICITATION_CONTENT: usize = 4096;
/// A config-option value. `select` values are ids; `boolean` values are `true`
/// or `false`; a free-text option is the only case that needs room.
const MAX_VALUE: usize = 200;

/// VibeDesk's own commands. A closed set: adding one is a code change, which is
/// the point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BuiltinCommand {
    ReloadSession,
    ReloadPlugins,
    NewChat,
    ToggleTheme,
    ToggleSidebar,
    FocusComposer,
    CheckUpdates,
    OpenPlugins,
    OpenConfig,
    OpenSessions,
    OpenContext,
    OpenSkills,
    OpenCustomize,
    OpenHelp,
    /// Open the swarm panel. Opening, not starting: a run spends fifteen times
    /// a chat's tokens, and a JSON file must not be able to begin one.
    OpenSwarm,
}

impl BuiltinCommand {
    /// Every command, in declaration order. The manifest publishes this list so
    /// the agent can name a real command instead of guessing; the test below
    /// fails if a variant is added without being listed here.
    pub const ALL: [BuiltinCommand; 15] = [
        Self::ReloadSession,
        Self::ReloadPlugins,
        Self::NewChat,
        Self::ToggleTheme,
        Self::ToggleSidebar,
        Self::FocusComposer,
        Self::CheckUpdates,
        Self::OpenPlugins,
        Self::OpenConfig,
        Self::OpenSessions,
        Self::OpenContext,
        Self::OpenSkills,
        Self::OpenCustomize,
        Self::OpenHelp,
        Self::OpenSwarm,
    ];
}

/// Icons an action may use. A closed set so the frontend never has to resolve a
/// name it does not have, and a file cannot smuggle in a component path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ActionIcon {
    Bolt,
    Book,
    Bug,
    Check,
    Code,
    File,
    Folder,
    GitBranch,
    Globe,
    Hammer,
    Info,
    List,
    Play,
    Refresh,
    Search,
    Settings,
    Sparkles,
    Terminal,
    TestTube,
    Wrench,
}

impl ActionIcon {
    /// Every icon, in declaration order. Published in the manifest so the agent
    /// picks a name that exists; the test below fails if a variant is added
    /// without being listed here.
    pub const ALL: [ActionIcon; 20] = [
        Self::Bolt,
        Self::Book,
        Self::Bug,
        Self::Check,
        Self::Code,
        Self::File,
        Self::Folder,
        Self::GitBranch,
        Self::Globe,
        Self::Hammer,
        Self::Info,
        Self::List,
        Self::Play,
        Self::Refresh,
        Self::Search,
        Self::Settings,
        Self::Sparkles,
        Self::Terminal,
        Self::TestTube,
        Self::Wrench,
    ];
}

/// Where the button appears. Two slots, both meaningful; a third would just be
/// a way to crowd the title bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ActionSlot {
    /// The strip above the composer.
    Composer,
    /// The icon rail, below the built-in panels.
    Rail,
}

impl Default for ActionSlot {
    fn default() -> Self {
        Self::Composer
    }
}

impl ActionSlot {
    /// Both slots, for the manifest.
    pub const ALL: [ActionSlot; 2] = [Self::Composer, Self::Rail];
}

/// What a button does. Tagged on `kind`, so an unknown kind is a parse error
/// rather than a silently ignored action.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ActionRun {
    /// Send text to the agent exactly as if it had been typed.
    Prompt { text: String },
    /// Run one of VibeDesk's own commands.
    Command { command: BuiltinCommand },
    /// Open a URL in the user's browser.
    OpenUrl { url: String },
    /// Reveal a workspace path in the OS file manager.
    Reveal { path: String },
    /// Switch the agent's session mode. `mode` is an id the agent advertised in
    /// `availableModes`; an id it does not offer is refused by the agent, not by
    /// this file, so the file cannot invent a mode.
    Mode { mode: String },
    /// Set one of the agent's config options — the model, a permission profile,
    /// a temperature. `option` is an id the agent advertised in
    /// `configOptions`, and `value` is one of the values it listed.
    Option { option: String, value: String },
    /// Cancel the turn the agent is running. The same call the stop button
    /// makes, so a button can stop a runaway turn without a trip to the UI.
    Cancel,
    /// Answer the permission request the agent is paused on with an allow.
    ///
    /// This is the control that unblocks a stopped agent. `prompt` sends text
    /// to a running agent; this answers the question that is holding it. The
    /// option is chosen by *kind*, not by id, because the ids belong to the
    /// agent and differ between agents — `allow_once` is the one thing every
    /// ACP agent agrees on. A request that offers no allow option is answered
    /// with a plain reject rather than left hanging, because an agent paused
    /// forever is worse than an agent that was told no.
    Approve {
        #[serde(default)]
        scope: PermissionScope,
    },
    /// Answer the permission request with a reject. Same rules as `approve`.
    Reject {
        #[serde(default)]
        scope: PermissionScope,
    },
    /// Answer the elicitation form the agent is paused on.
    ///
    /// `accept` submits `content`, which is a literal object rather than an
    /// expression: the agent validates it against the form's schema, and a
    /// value that changed shape between the click and the answer would be
    /// rejected. `decline` and `cancel` need no content.
    Answer {
        action: ElicitationAnswer,
        #[serde(default)]
        content: Option<serde_json::Value>,
    },
}

/// Which of the two options the agent offered to take.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PermissionScope {
    /// Answer this request only.
    #[default]
    Once,
    /// Answer this request and every later one of the same kind.
    Always,
}

impl PermissionScope {
    pub const ALL: [&'static str; 2] = ["once", "always"];
}

/// How a button answers an elicitation form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ElicitationAnswer {
    /// Submit the form. `content` supplies the values.
    Accept,
    /// Dismiss the form without values.
    Decline,
    /// Abandon the request entirely.
    Cancel,
}

impl ElicitationAnswer {
    pub const ALL: [&'static str; 3] = ["accept", "decline", "cancel"];
}

impl ActionRun {
    /// Every run kind, in declaration order. The manifest publishes this so the
    /// agent names a real kind instead of guessing; the test below fails if a
    /// variant is added without being listed here.
    pub const KINDS: [&'static str; 10] = [
        "prompt", "command", "open-url", "reveal", "mode", "option", "cancel", "approve", "reject",
        "answer",
    ];
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomAction {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub icon: Option<ActionIcon>,
    #[serde(default)]
    pub slot: ActionSlot,
    /// Shown as the button's tooltip. Optional.
    #[serde(default)]
    pub hint: Option<String>,
    pub run: ActionRun,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ActionsConfig {
    pub actions: Vec<CustomAction>,
}

pub fn actions_path() -> PathBuf {
    super::customize::config_dir().join("actions.json")
}

/// Read the actions from disk. A missing file is an empty set, not an error.
pub fn load_actions() -> Result<ActionsConfig, String> {
    load_actions_at(&actions_path(), super::customize::safe_mode())
}

/// The safe-mode decision is a parameter so the bypass can be tested without
/// writing to the real data directory.
pub fn load_actions_at(path: &std::path::Path, safe: bool) -> Result<ActionsConfig, String> {
    if safe {
        return Ok(ActionsConfig::default());
    }
    load_actions_from(path)
}

pub fn load_actions_from(path: &std::path::Path) -> Result<ActionsConfig, String> {
    let Ok(meta) = std::fs::metadata(path) else {
        return Ok(ActionsConfig::default());
    };
    if meta.len() > MAX_ACTIONS_BYTES {
        return Err(format!(
            "actions.json is {} bytes; the limit is {}",
            meta.len(),
            MAX_ACTIONS_BYTES
        ));
    }

    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("could not read {}: {e}", path.display()))?;
    if raw.trim().is_empty() {
        return Ok(ActionsConfig::default());
    }

    let config: ActionsConfig = serde_json::from_str(&raw)
        .map_err(|e| format!("actions.json is not valid JSON: {e}"))?;
    validate(&config)?;
    Ok(config)
}

/// Validate, then write. Invalid input never reaches disk.
pub fn save_actions(config: &ActionsConfig) -> Result<ActionsConfig, String> {
    save_actions_to(&actions_path(), config)
}

pub fn save_actions_to(
    path: &std::path::Path,
    config: &ActionsConfig,
) -> Result<ActionsConfig, String> {
    validate(config)?;

    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    }

    let body = serde_json::to_string_pretty(config)
        .map_err(|e| format!("could not serialise the actions: {e}"))?;
    if body.len() as u64 > MAX_ACTIONS_BYTES {
        return Err(format!("actions are larger than {MAX_ACTIONS_BYTES} bytes"));
    }

    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, &body)
        .map_err(|e| format!("could not write {}: {e}", temp.display()))?;
    std::fs::rename(&temp, path)
        .map_err(|e| format!("could not replace {}: {e}", path.display()))?;

    Ok(config.clone())
}

pub fn reset_actions() -> Result<ActionsConfig, String> {
    reset_actions_at(&actions_path())
}

pub fn reset_actions_at(path: &std::path::Path) -> Result<ActionsConfig, String> {
    if path.exists() {
        std::fs::remove_file(path)
            .map_err(|e| format!("could not remove {}: {e}", path.display()))?;
    }
    Ok(ActionsConfig::default())
}

/// Every problem with an actions file, so the UI can show all of them at once.
pub fn validate(config: &ActionsConfig) -> Result<(), String> {
    let mut problems: Vec<String> = Vec::new();

    if config.actions.len() > MAX_ACTIONS {
        problems.push(format!(
            "{} actions is more than the {MAX_ACTIONS} allowed",
            config.actions.len()
        ));
    }

    let mut seen: BTreeSet<&str> = BTreeSet::new();

    for (index, action) in config.actions.iter().enumerate() {
        let label = if action.id.trim().is_empty() {
            format!("action {}", index + 1)
        } else {
            format!("action `{}`", action.id)
        };

        if !is_id(&action.id) {
            problems.push(format!(
                "{label}: id must be 1-{MAX_ID} lowercase letters, digits or dashes"
            ));
        } else if !seen.insert(action.id.as_str()) {
            problems.push(format!("{label}: id is used twice"));
        }

        if let Err(reason) = check_text(&action.label, MAX_LABEL, "label") {
            problems.push(format!("{label}: {reason}"));
        }
        if let Some(hint) = &action.hint {
            if let Err(reason) = check_text(hint, MAX_LABEL * 4, "hint") {
                problems.push(format!("{label}: {reason}"));
            }
        }

        if let Err(reason) = check_run(&action.run) {
            problems.push(format!("{label}: {reason}"));
        }
    }

    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("\n"))
    }
}

fn check_run(run: &ActionRun) -> Result<(), String> {
    match run {
        ActionRun::Prompt { text } => {
            if text.trim().is_empty() {
                return Err("prompt text is empty".into());
            }
            check_text(text, MAX_PROMPT, "prompt text")
        }
        ActionRun::Command { .. } => Ok(()),
        ActionRun::OpenUrl { url } => check_url(url),
        ActionRun::Reveal { path } => check_relative_path(path),
        ActionRun::Mode { mode } => check_token(mode, "mode id"),
        ActionRun::Option { option, value } => {
            check_token(option, "option id")?;
            check_text(value, MAX_VALUE, "option value")
        }
        ActionRun::Cancel => Ok(()),
        ActionRun::Approve { .. } | ActionRun::Reject { .. } => Ok(()),
        ActionRun::Answer { action, content } => {
            if *action != ElicitationAnswer::Accept && content.is_some() {
                return Err(
                    "`content` is only read by `accept`; a decline or a cancel carries no values"
                        .into(),
                );
            }
            if let Some(content) = content {
                if !content.is_object() {
                    return Err("`content` must be an object of form values".into());
                }
                let size = serde_json::to_string(content)
                    .map(|text| text.len())
                    .unwrap_or(usize::MAX);
                if size > MAX_ELICITATION_CONTENT {
                    return Err(format!(
                        "`content` is {size} bytes; the limit is {MAX_ELICITATION_CONTENT}"
                    ));
                }
            }
            Ok(())
        }
    }
}

/// A mode id or a config-option id.
///
/// These are agent-authored tokens, not prose: `plan`, `act`, `model`,
/// `temperature`. Whitespace is the thing to refuse — an id with a space in it
/// is a sentence someone pasted, and it will never match a real option.
fn check_token(value: &str, what: &str) -> Result<(), String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(format!("{what} is empty"));
    }
    if trimmed.chars().count() > MAX_TOKEN {
        return Err(format!("{what} is longer than {MAX_TOKEN} characters"));
    }
    if trimmed
        .chars()
        .any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(format!("{what} contains whitespace or control characters"));
    }
    Ok(())
}

fn check_url(url: &str) -> Result<(), String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err("url is empty".into());
    }
    if trimmed.len() > MAX_URL {
        return Err(format!("url is longer than {MAX_URL} characters"));
    }
    if trimmed.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("url contains whitespace or control characters".into());
    }

    let lower = trimmed.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return Err("url must start with http:// or https://".into());
    }
    // `https://user:pass@host` would put a credential in a file the agent can
    // read, so the authority must not carry one.
    let after_scheme = &trimmed[trimmed.find("//").map(|i| i + 2).unwrap_or(0)..];
    let authority = after_scheme.split(['/', '?', '#']).next().unwrap_or("");
    if authority.contains('@') {
        return Err("url must not embed credentials".into());
    }

    Ok(())
}

fn check_relative_path(path: &str) -> Result<(), String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("path is empty".into());
    }
    if trimmed.len() > MAX_PATH {
        return Err(format!("path is longer than {MAX_PATH} characters"));
    }
    if trimmed.chars().any(|c| c.is_control()) {
        return Err("path contains control characters".into());
    }
    if trimmed.starts_with('/') || trimmed.starts_with('\\') || trimmed.starts_with('~') {
        return Err("path must be relative to the workspace".into());
    }
    // A drive letter or a scheme would escape the workspace on Windows.
    if trimmed.contains(':') {
        return Err("path must not contain `:`".into());
    }
    if trimmed
        .split(['/', '\\'])
        .any(|segment| segment == "..")
    {
        return Err("path must not climb out of the workspace with `..`".into());
    }

    Ok(())
}

fn check_text(value: &str, max: usize, what: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("{what} is empty"));
    }
    if value.chars().count() > max {
        return Err(format!("{what} is longer than {max} characters"));
    }
    if value
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err(format!("{what} contains control characters"));
    }
    Ok(())
}

fn is_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action(id: &str, run: ActionRun) -> CustomAction {
        CustomAction {
            id: id.into(),
            label: "Do the thing".into(),
            icon: None,
            slot: ActionSlot::Composer,
            hint: None,
            run,
        }
    }

    fn config(actions: Vec<CustomAction>) -> ActionsConfig {
        ActionsConfig { actions }
    }

    #[test]
    fn accepts_one_of_each_kind() {
        let config = config(vec![
            action(
                "review",
                ActionRun::Prompt {
                    text: "Review the diff".into(),
                },
            ),
            action(
                "reload",
                ActionRun::Command {
                    command: BuiltinCommand::ReloadPlugins,
                },
            ),
            action(
                "docs",
                ActionRun::OpenUrl {
                    url: "https://docs.mistral.ai/vibe/code/overview".into(),
                },
            ),
            action(
                "manifest",
                ActionRun::Reveal {
                    path: "Cargo.toml".into(),
                },
            ),
        ]);
        assert!(validate(&config).is_ok());
    }

    #[test]
    fn rejects_an_unknown_kind() {
        let raw = r#"{"actions":[{"id":"x","label":"X","run":{"kind":"shell","command":"rm -rf /"}}]}"#;
        assert!(serde_json::from_str::<ActionsConfig>(raw).is_err());
    }

    #[test]
    fn accepts_the_agent_control_kinds() {
        let config = config(vec![
            action(
                "plan",
                ActionRun::Mode {
                    mode: "plan".into(),
                },
            ),
            action(
                "model",
                ActionRun::Option {
                    option: "model".into(),
                    value: "mistral-large-latest".into(),
                },
            ),
            action("stop", ActionRun::Cancel),
        ]);
        assert!(validate(&config).is_ok());
    }

    #[test]
    fn rejects_a_mode_id_that_is_a_sentence() {
        // An id with a space in it will never match a mode the agent offers, so
        // it is a mistake worth reporting rather than a mode worth trying.
        let config = config(vec![action(
            "x",
            ActionRun::Mode {
                mode: "switch to plan mode".into(),
            },
        )]);
        assert!(validate(&config).is_err());
    }

    #[test]
    fn rejects_an_empty_option_id_or_value() {
        for run in [
            ActionRun::Option {
                option: "  ".into(),
                value: "x".into(),
            },
            ActionRun::Option {
                option: "model".into(),
                value: "   ".into(),
            },
        ] {
            let config = config(vec![action("x", run)]);
            assert!(validate(&config).is_err());
        }
    }

    #[test]
    fn every_run_kind_is_listed() {
        // The manifest publishes `KINDS`, so a variant missing from it is a kind
        // the agent cannot discover. Each listed name must parse, and the count
        // must match the enum — adding a variant without listing it fails here.
        assert_eq!(ActionRun::KINDS.len(), 10);
        for kind in ActionRun::KINDS {
            let raw = match kind {
                "prompt" => r#"{"kind":"prompt","text":"hi"}"#,
                "command" => r#"{"kind":"command","command":"new-chat"}"#,
                "open-url" => r#"{"kind":"open-url","url":"https://example.com"}"#,
                "reveal" => r#"{"kind":"reveal","path":"a.txt"}"#,
                "mode" => r#"{"kind":"mode","mode":"plan"}"#,
                "option" => r#"{"kind":"option","option":"model","value":"x"}"#,
                "cancel" => r#"{"kind":"cancel"}"#,
                "approve" => r#"{"kind":"approve","scope":"once"}"#,
                "reject" => r#"{"kind":"reject","scope":"always"}"#,
                "answer" => r#"{"kind":"answer","action":"decline"}"#,
                other => panic!("{other} is listed but has no sample"),
            };
            assert!(
                serde_json::from_str::<ActionRun>(raw).is_ok(),
                "{kind} should parse"
            );
        }
    }

    #[test]
    fn approve_and_reject_default_to_once() {
        // A file that says `{"kind":"approve"}` must not be a file that grants
        // every future request. The default is the narrow one.
        let run: ActionRun = serde_json::from_str(r#"{"kind":"approve"}"#).unwrap();
        assert!(matches!(
            run,
            ActionRun::Approve {
                scope: PermissionScope::Once
            }
        ));
        let run: ActionRun = serde_json::from_str(r#"{"kind":"reject"}"#).unwrap();
        assert!(matches!(
            run,
            ActionRun::Reject {
                scope: PermissionScope::Once
            }
        ));
        let cfg = config(vec![action("ok", ActionRun::Approve { scope: PermissionScope::Always })]);
        assert!(validate(&cfg).is_ok());
    }

    #[test]
    fn an_answer_carries_content_only_when_it_accepts() {
        // `decline` and `cancel` carry no values, so a `content` beside them is
        // a field that would be silently ignored. Refused instead.
        let cfg = config(vec![action(
            "x",
            ActionRun::Answer {
                action: ElicitationAnswer::Decline,
                content: Some(serde_json::json!({ "region": "eu" })),
            },
        )]);
        assert!(validate(&cfg).is_err());

        let cfg = config(vec![action(
            "x",
            ActionRun::Answer {
                action: ElicitationAnswer::Accept,
                content: Some(serde_json::json!({ "region": "eu" })),
            },
        )]);
        assert!(validate(&cfg).is_ok());

        // An accept with no content is a form whose fields are all optional.
        let cfg = config(vec![action(
            "x",
            ActionRun::Answer {
                action: ElicitationAnswer::Accept,
                content: None,
            },
        )]);
        assert!(validate(&cfg).is_ok());
    }

    #[test]
    fn an_answer_content_must_be_a_bounded_object() {
        let cfg = config(vec![action(
            "x",
            ActionRun::Answer {
                action: ElicitationAnswer::Accept,
                content: Some(serde_json::json!(["not", "an", "object"])),
            },
        )]);
        assert!(validate(&cfg).is_err());

        let big = "x".repeat(MAX_ELICITATION_CONTENT);
        let cfg = config(vec![action(
            "x",
            ActionRun::Answer {
                action: ElicitationAnswer::Accept,
                content: Some(serde_json::json!({ "blob": big })),
            },
        )]);
        assert!(validate(&cfg).is_err());
    }

    #[test]
    fn rejects_an_unknown_builtin_command() {
        let raw = r#"{"actions":[{"id":"x","label":"X","run":{"kind":"command","command":"format-disk"}}]}"#;
        assert!(serde_json::from_str::<ActionsConfig>(raw).is_err());
    }

    #[test]
    fn rejects_non_http_urls() {
        for url in [
            "javascript:alert(1)",
            "file:///C:/Windows/System32",
            "data:text/html,<script>",
            "ftp://example.com",
            "not a url",
        ] {
            let config = config(vec![action("x", ActionRun::OpenUrl { url: url.into() })]);
            assert!(validate(&config).is_err(), "{url} should be rejected");
        }
    }

    #[test]
    fn rejects_urls_with_embedded_credentials() {
        let config = config(vec![action(
            "x",
            ActionRun::OpenUrl {
                url: "https://user:pass@example.com/x".into(),
            },
        )]);
        assert!(validate(&config).is_err());
    }

    #[test]
    fn rejects_paths_that_escape_the_workspace() {
        for path in [
            "../secrets.txt",
            "a/../../b",
            "/etc/passwd",
            "C:/Windows/win.ini",
            "~/notes.md",
            "\\\\server\\share",
        ] {
            let config = config(vec![action("x", ActionRun::Reveal { path: path.into() })]);
            assert!(validate(&config).is_err(), "{path} should be rejected");
        }
    }

    #[test]
    fn rejects_duplicate_and_malformed_ids() {
        let dup = config(vec![
            action("same", ActionRun::Command { command: BuiltinCommand::NewChat }),
            action("same", ActionRun::Command { command: BuiltinCommand::NewChat }),
        ]);
        assert!(validate(&dup).is_err());

        for id in ["", "Has Space", "UPPER", "with_underscore"] {
            let bad = config(vec![action(
                id,
                ActionRun::Command {
                    command: BuiltinCommand::NewChat,
                },
            )]);
            assert!(validate(&bad).is_err(), "{id} should be rejected");
        }
    }

    #[test]
    fn rejects_empty_prompt_text() {
        let config = config(vec![action(
            "x",
            ActionRun::Prompt {
                text: "   ".into(),
            },
        )]);
        assert!(validate(&config).is_err());
    }

    #[test]
    fn round_trips_a_file_and_keeps_defaults_for_missing_fields() {
        let dir = std::env::temp_dir().join(format!("vibedesk-actions-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("actions.json");

        std::fs::write(
            &path,
            r#"{"actions":[{"id":"go","label":"Go","run":{"kind":"command","command":"new-chat"}}]}"#,
        )
        .unwrap();
        let loaded = load_actions_from(&path).unwrap();
        assert_eq!(loaded.actions.len(), 1);
        // `slot` was omitted, so it takes the default rather than failing.
        assert_eq!(loaded.actions[0].slot, ActionSlot::Composer);
        assert!(loaded.actions[0].icon.is_none());

        // A bad file is reported, not silently applied.
        std::fs::write(
            &path,
            r#"{"actions":[{"id":"x","label":"X","run":{"kind":"open-url","url":"javascript:1"}}]}"#,
        )
        .unwrap();
        assert!(load_actions_from(&path).is_err());

        // A missing file is an empty set, not an error.
        std::fs::remove_file(&path).unwrap();
        assert!(load_actions_from(&path).unwrap().actions.is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }
}
