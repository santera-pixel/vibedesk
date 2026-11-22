//! The swarm as the agent sees it.
//!
//! A member of a swarm is a `vibe-acp` session, and so is the agent the user is
//! talking to. Giving that agent a way to start a swarm is what makes the feature
//! reachable from the conversation instead of only from a panel: the user says
//! what they want, the agent decides the work is big enough to split, and the run
//! appears in the strip above the composer.
//!
//! The channel is the filesystem, for the same reason the board is. The app owns
//! the engine and the driver lock; a second process that wants a run has to ask
//! the owner rather than take the lock. A command is a file, the answer is a file
//! beside it, and a command nobody answers is a command the app was not running
//! to receive.
//!
//! This module is the protocol only — no engine, no Tauri, no process. The app
//! answers commands in `commands::swarm`, and `swarm-mcp` asks them.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// How often a waiter looks for its answer.
///
/// The app drains the queue on its own clock, so this only bounds how long the
/// caller sits on a reply that is already written.
const POLL: Duration = Duration::from_millis(80);

/// The operations a caller may ask for.
///
/// Deliberately short. `status` is not here because it needs no owner: a run is a
/// file, and reading it is not an action. Everything in this list changes the
/// world, so everything in this list goes through the process that owns the lock.
pub const OPS: &[&str] = &["start", "approve", "stop"];

/// One request from a caller to the app.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Command {
    pub op: String,
    /// The goal, for `start`.
    #[serde(default)]
    pub goal: String,
    /// The repository to run in. Empty means the app's current workspace.
    #[serde(default)]
    pub workspace: String,
    #[serde(default)]
    pub concurrency: Option<usize>,
    #[serde(default)]
    pub budget_tokens: Option<u64>,
    /// The ceiling in money, in millionths of a dollar. Absent means the token
    /// ceiling is the only bound; the spend is reported either way.
    #[serde(default)]
    pub budget_micros: Option<u64>,
    /// The model the lead and the workers run on.
    ///
    /// Empty means the workspace's active model, which is the right default and
    /// the wrong one often enough to be worth overriding: the model a user chats
    /// with is chosen for conversation, and a swarm needs one that can hold a
    /// long tool-calling turn. The verifier is still picked for diversity, so an
    /// override here cannot collapse the run onto a single model.
    #[serde(default)]
    pub model: String,
}

impl Command {
    pub fn start(goal: impl Into<String>, workspace: impl Into<String>) -> Self {
        Self {
            op: "start".into(),
            goal: goal.into(),
            workspace: workspace.into(),
            ..Self::default()
        }
    }

    pub fn approve() -> Self {
        Self {
            op: "approve".into(),
            ..Self::default()
        }
    }

    pub fn stop() -> Self {
        Self {
            op: "stop".into(),
            ..Self::default()
        }
    }

    /// Why this command cannot be run, if it cannot.
    ///
    /// Checked on both sides. A caller that sends nonsense gets a named refusal
    /// instead of a run that fails halfway through planning.
    pub fn refuse(&self) -> Option<String> {
        if !OPS.contains(&self.op.as_str()) {
            return Some(format!(
                "`{}` is not a swarm command — one of {}",
                self.op,
                OPS.join(", ")
            ));
        }
        if self.op == "start" && self.goal.trim().is_empty() {
            return Some("a swarm needs a goal".into());
        }
        None
    }
}

/// Where commands live.
pub fn commands_dir() -> PathBuf {
    crate::paths::app_data_dir().join("swarm").join("commands")
}

/// Ask the app to do something, and wait for its answer.
///
/// The timeout is the whole point of the design: a caller that gets no answer
/// learns that the app is not running, which is a different problem from a run
/// that failed, and the two must not be reported the same way.
pub fn submit(command: &Command, timeout: Duration) -> Result<Value, String> {
    submit_at(&commands_dir(), command, timeout)
}

pub fn submit_at(root: &Path, command: &Command, timeout: Duration) -> Result<Value, String> {
    if let Some(reason) = command.refuse() {
        return Err(reason);
    }
    std::fs::create_dir_all(root)
        .map_err(|error| format!("could not create {}: {error}", root.display()))?;

    let id = next_id();
    let request = root.join(format!("{id}.json"));
    let body = serde_json::to_string_pretty(command)
        .map_err(|error| format!("could not encode the command: {error}"))?;
    std::fs::write(&request, body)
        .map_err(|error| format!("could not write {}: {error}", request.display()))?;

    let answer = root.join(format!("{id}.done.json"));
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(body) = std::fs::read_to_string(&answer) {
            let _ = std::fs::remove_file(&request);
            let _ = std::fs::remove_file(&answer);
            return serde_json::from_str(&body)
                .map_err(|error| format!("the answer to {id} is not JSON: {error}"));
        }
        if Instant::now() >= deadline {
            // Leave the request in place: the app may still be starting, and a
            // command that arrives late is better than one that vanished.
            return Err(format!(
                "VibeDesk did not answer within {}s — is the app running?",
                timeout.as_secs()
            ));
        }
        std::thread::sleep(POLL);
    }
}

/// Every command the app has not answered yet, oldest first.
pub fn pending() -> Vec<(String, Command)> {
    pending_at(&commands_dir())
}

pub fn pending_at(root: &Path) -> Vec<(String, Command)> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut found: Vec<(String, Command)> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            let id = name.strip_suffix(".json")?.to_string();
            if id.ends_with(".done") {
                return None;
            }
            // An answered command is not pending, even if the caller has not
            // collected the answer yet.
            if root.join(format!("{id}.done.json")).exists() {
                return None;
            }
            let body = std::fs::read_to_string(entry.path()).ok()?;
            let command: Command = serde_json::from_str(&body).ok()?;
            Some((id, command))
        })
        .collect();
    // The id is a timestamp, so lexical order is arrival order.
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found
}

/// Answer a command.
pub fn reply(id: &str, value: &Value) -> Result<(), String> {
    reply_at(&commands_dir(), id, value)
}

pub fn reply_at(root: &Path, id: &str, value: &Value) -> Result<(), String> {
    let path = root.join(format!("{id}.done.json"));
    let body = serde_json::to_string_pretty(value)
        .map_err(|error| format!("could not encode the answer: {error}"))?;
    std::fs::write(&path, body).map_err(|error| format!("could not write {}: {error}", path.display()))
}

/// A command id that sorts by arrival.
///
/// Microseconds plus a counter: two commands in the same microsecond must not
/// collide, and the app must see them in the order they were sent.
fn next_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros())
        .unwrap_or(0);
    format!("{now:020}-{:04}", COUNTER.fetch_add(1, Ordering::SeqCst))
}

/// The answer shape, so both sides agree on it.
pub fn ok(value: Value) -> Value {
    json!({ "ok": true, "result": value })
}

pub fn failed(reason: impl Into<String>) -> Value {
    json!({ "ok": false, "error": reason.into() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "vd-swarm-cmd-{}-{}",
            std::process::id(),
            next_id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_command_with_no_goal_is_refused_before_it_is_written() {
        let root = temp();
        let error = submit_at(&root, &Command::start("   ", "D:/x"), Duration::from_millis(50))
            .unwrap_err();
        assert!(error.contains("needs a goal"), "{error}");
        assert!(pending_at(&root).is_empty(), "nothing should have been written");
    }

    #[test]
    fn an_unknown_operation_is_refused_and_names_the_real_ones() {
        let root = temp();
        let command = Command {
            op: "explode".into(),
            ..Command::default()
        };
        let error = submit_at(&root, &command, Duration::from_millis(50)).unwrap_err();
        assert!(error.contains("explode"), "{error}");
        assert!(error.contains("start"), "{error}");
    }

    #[test]
    fn a_command_nobody_answers_times_out_and_says_the_app_is_not_running() {
        let root = temp();
        let error = submit_at(&root, &Command::stop(), Duration::from_millis(120)).unwrap_err();
        assert!(error.contains("did not answer"), "{error}");
        assert!(error.contains("running"), "{error}");
        // The request survives, so a late app can still honour it.
        assert_eq!(pending_at(&root).len(), 1);
    }

    #[test]
    fn an_answered_command_is_returned_and_both_files_are_cleared() {
        let root = temp();
        let root_for_app = root.clone();
        let app = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                let pending = pending_at(&root_for_app);
                if let Some((id, command)) = pending.first() {
                    assert_eq!(command.op, "start");
                    assert_eq!(command.goal, "build a game");
                    reply_at(&root_for_app, id, &ok(json!({ "runId": "run-1" }))).unwrap();
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("the command never arrived");
        });

        let answer = submit_at(
            &root,
            &Command::start("build a game", "D:/repo"),
            Duration::from_secs(3),
        )
        .unwrap();
        app.join().unwrap();

        assert_eq!(answer["ok"], true);
        assert_eq!(answer["result"]["runId"], "run-1");
        assert!(pending_at(&root).is_empty());
        let leftovers: Vec<_> = std::fs::read_dir(&root).unwrap().flatten().collect();
        assert!(leftovers.is_empty(), "the exchange should leave nothing behind");
    }

    #[test]
    fn a_failure_is_an_answer_not_a_timeout() {
        let root = temp();
        let root_for_app = root.clone();
        std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                if let Some((id, _)) = pending_at(&root_for_app).first() {
                    reply_at(&root_for_app, id, &failed("a run is already live")).unwrap();
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        });

        let answer = submit_at(&root, &Command::approve(), Duration::from_secs(3)).unwrap();
        assert_eq!(answer["ok"], false);
        assert_eq!(answer["error"], "a run is already live");
    }

    #[test]
    fn commands_are_seen_in_the_order_they_were_sent() {
        let root = temp();
        for goal in ["first", "second", "third"] {
            let command = Command::start(goal, "D:/repo");
            let id = next_id();
            std::fs::write(
                root.join(format!("{id}.json")),
                serde_json::to_string(&command).unwrap(),
            )
            .unwrap();
        }
        let goals: Vec<String> = pending_at(&root)
            .into_iter()
            .map(|(_, command)| command.goal)
            .collect();
        assert_eq!(goals, vec!["first", "second", "third"]);
    }

    #[test]
    fn a_malformed_command_is_skipped_rather_than_blocking_the_queue() {
        let root = temp();
        std::fs::write(root.join("0001.json"), "{ not json").unwrap();
        std::fs::write(
            root.join("0002.json"),
            serde_json::to_string(&Command::stop()).unwrap(),
        )
        .unwrap();
        let pending = pending_at(&root);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].1.op, "stop");
    }

    #[test]
    fn an_answered_command_is_not_handed_out_twice() {
        let root = temp();
        let id = next_id();
        std::fs::write(
            root.join(format!("{id}.json")),
            serde_json::to_string(&Command::stop()).unwrap(),
        )
        .unwrap();
        assert_eq!(pending_at(&root).len(), 1);
        reply_at(&root, &id, &ok(json!(null))).unwrap();
        assert!(pending_at(&root).is_empty(), "a reply must retire the request");
    }
}
