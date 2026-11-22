use serde::{Serialize, Serializer};

/// One error type for everything that crosses the IPC boundary.
///
/// Tauri serialises command errors, so the webview receives a plain string
/// with enough context to show the user what actually failed.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("the Vibe agent is not running")]
    AgentNotRunning,

    #[error("the Vibe agent is already running")]
    AgentAlreadyRunning,

    #[error("could not find the vibe-acp binary: {0}")]
    BinaryNotFound(String),

    #[error("no session is open")]
    NoSession,

    #[error("agent request failed ({code}): {message}")]
    Rpc { code: i64, message: String },

    #[error("agent request timed out after {0}s")]
    Timeout(u64),

    /// The agent stopped talking mid-turn. The turn itself is still running, so
    /// the UI must keep its Stop control rather than pretending the turn ended.
    #[error("the agent went silent for {0}s — the turn is still running. Stop it, or wait for it to finish.")]
    TurnStalled(u64),

    /// A second prompt arrived while one was still in flight. Sending it would
    /// only earn `-32603 A turn is already running` from the agent.
    #[error("a turn is already running — stop it before sending another prompt")]
    TurnInProgress,

    #[error("the agent process exited: {0}")]
    AgentExited(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("could not parse {path}: {source}")]
    Toml {
        path: String,
        #[source]
        source: toml::de::Error,
    },

    #[error("invalid json: {0}")]
    Json(#[from] serde_json::Error),

    #[error("{0}")]
    Message(String),
}

impl AppError {
    pub fn message(value: impl Into<String>) -> Self {
        AppError::Message(value.into())
    }

    /// Rewrite an agent error the user can act on into a sentence that says
    /// what to do about it.
    ///
    /// `-31002 Session is already open` is the CLI refusing to hand a session
    /// to a second process — correct behaviour, but the raw code reads like a
    /// crash. The lock is held by a live Vibe process, so the only fix is to
    /// close the chat there; nothing VibeDesk can do on its own.
    pub fn explain(self) -> Self {
        const SESSION_ALREADY_OPEN: i64 = -31002;
        match self {
            AppError::Rpc { code, message } if code == SESSION_ALREADY_OPEN => {
                AppError::Message(format!(
                    "This chat is open in another Vibe session. Close it there, then open it here. ({message})"
                ))
            }
            other => other,
        }
    }
}

impl Serialize for AppError {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

pub type AppResult<T> = Result<T, AppError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_already_open_becomes_an_instruction() {
        let raw = AppError::Rpc {
            code: -31002,
            message: "Session is already open: 877e7404".into(),
        };
        let text = raw.explain().to_string();
        assert!(text.starts_with("This chat is open in another Vibe session."));
        assert!(text.contains("Close it there"));
        // The original detail survives, so a bug report still has the id.
        assert!(text.contains("877e7404"));
    }

    #[test]
    fn every_other_error_is_passed_through_untouched() {
        let raw = AppError::Rpc {
            code: -32603,
            message: "A turn is already running".into(),
        };
        assert_eq!(
            raw.explain().to_string(),
            "agent request failed (-32603): A turn is already running"
        );
    }

    /// The webview decides whether to keep its Stop control by matching this
    /// sentence, so it has to say the turn is still running.
    #[test]
    fn a_stalled_turn_says_the_turn_is_still_running() {
        let text = AppError::TurnStalled(180).to_string();
        assert!(text.contains("went silent for 180s"));
        assert!(text.contains("the turn is still running"));
    }

    /// `promptWithRecovery` retries on the agent's own `-32603`. This error
    /// must not read like that one, or the retry would fire pointlessly.
    #[test]
    fn a_second_prompt_is_told_to_stop_the_first() {
        let text = AppError::TurnInProgress.to_string();
        assert!(text.contains("stop it before sending another prompt"));
        assert!(!text.contains("agent request failed"));
    }
}
