//! The `process` backend.
//!
//! Drives a child process over its pipes: write to stdin, read stdout and
//! stderr, notice the exit code. This is the backend for a CLI the agent just
//! built, a headless test runner, a REPL, or anything else that talks in text.
//!
//! It is also the one backend a swarm member may use. A member gets its own
//! worktree and its own process, so two members driving two processes cannot
//! collide — unlike the webview and window backends, which share one desktop and
//! one driver between them.
//!
//! What it cannot do, it says it cannot do. There is no pointer to move and no
//! window to focus, so `click`, `drag`, `scroll` and `hold` are absent from its
//! capabilities rather than silently doing nothing.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::channel::backend::{
    Action, ActionKind, ActionResult, Backend, BackendKind, Capabilities, HostKind, InputKind,
    Look, Target,
    TargetRef,
};
use crate::channel::guardrail::Rect;
use crate::channel::issues::{Issue, Issues, Level};
use crate::error::AppError;

/// How many output lines the digest shows.
const DIGEST_LINES: usize = 40;

/// How many output lines are kept for `read`.
const KEPT_LINES: usize = 400;

/// How many issues the buffer keeps.
const ISSUE_CAP: usize = 200;

/// One line of output, and which pipe it came from.
#[derive(Debug, Clone)]
struct Line {
    from_stderr: bool,
    text: String,
}

struct State {
    lines: VecDeque<Line>,
    issues: Issues,
    exit: Option<i32>,
}

/// A channel onto one child process.
pub struct ProcessBackend {
    child: Child,
    stdin: Option<ChildStdin>,
    state: Arc<Mutex<State>>,
    command: String,
}

impl ProcessBackend {
    /// Start a process and begin reading both pipes.
    pub fn open(
        command: impl Into<String>,
        args: &[String],
        cwd: Option<&str>,
    ) -> Result<Self, AppError> {
        let command = command.into();
        let mut builder = Command::new(&command);
        builder
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(cwd) = cwd {
            builder.current_dir(cwd);
        }

        let mut child = builder.spawn().map_err(|error| {
            AppError::message(format!("could not start {command}: {error}"))
        })?;

        let stdin = child.stdin.take();
        let state = Arc::new(Mutex::new(State {
            lines: VecDeque::new(),
            issues: Issues::new(ISSUE_CAP),
            exit: None,
        }));

        if let Some(stdout) = child.stdout.take() {
            let sink = state.clone();
            std::thread::spawn(move || read_lines(stdout, false, sink));
        }
        if let Some(stderr) = child.stderr.take() {
            let sink = state.clone();
            std::thread::spawn(move || read_lines(stderr, true, sink));
        }

        Ok(Self {
            child,
            stdin,
            state,
            command,
        })
    }

    /// Notice an exit that has already happened, without blocking.
    fn poll_exit(&mut self) {
        if let Ok(Some(status)) = self.child.try_wait() {
            let code = status.code().unwrap_or(-1);
            if let Ok(mut state) = self.state.lock() {
                if state.exit.is_none() {
                    state.exit = Some(code);
                    if code != 0 {
                        state.issues.push(
                            Level::Error,
                            "process",
                            format!("{} exited with {code}", self.command),
                            now(),
                        );
                    }
                }
            }
        }
    }
}

impl Backend for ProcessBackend {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            backend: BackendKind::Process,
            host: HostKind::Desktop,
            // A child process is driven over its pipes. Nothing is clicked, so
            // nothing is brought forward.
            foreground: false,
            input: InputKind::Synthetic,
            actions: vec![ActionKind::Type, ActionKind::Wait, ActionKind::Read],
            capture: false,
            issues: true,
        }
    }

    fn look(&mut self, _capture: bool) -> Result<Look, AppError> {
        self.poll_exit();
        let state = self
            .state
            .lock()
            .map_err(|_| AppError::message("the output buffer is poisoned"))?;

        let start = state.lines.len().saturating_sub(DIGEST_LINES);
        let mut digest = format!("{} — ", self.command);
        match state.exit {
            Some(code) => digest.push_str(&format!("exited with {code}\n")),
            None => digest.push_str("running\n"),
        }
        for line in state.lines.iter().skip(start) {
            let marker = if line.from_stderr { "!" } else { " " };
            digest.push_str(&format!("{marker} {}\n", line.text));
        }
        if state.lines.is_empty() {
            digest.push_str("(no output yet)\n");
        }

        Ok(Look {
            digest,
            stage: Rect {
                x: 0.0,
                y: 0.0,
                w: 0.0,
                h: 0.0,
            },
            targets: Vec::new(),
            capture: None,
            capture_type: None,
        })
    }

    fn act(&mut self, action: &Action) -> Result<ActionResult, AppError> {
        let kind = action.kind();
        match action {
            Action::Type { text, .. } => {
                let stdin = self
                    .stdin
                    .as_mut()
                    .ok_or_else(|| AppError::message("the process has no stdin"))?;
                stdin
                    .write_all(text.as_bytes())
                    .and_then(|_| stdin.flush())
                    .map_err(|error| {
                        AppError::message(format!("could not write to {}: {error}", self.command))
                    })?;
                Ok(ActionResult::ok(
                    kind,
                    format!("wrote {} bytes", text.len()),
                    0,
                ))
            }
            Action::Wait { ms } => {
                std::thread::sleep(Duration::from_millis(*ms));
                self.poll_exit();
                Ok(ActionResult::ok(kind, format!("waited {ms}ms"), *ms))
            }
            Action::Read { .. } => {
                self.poll_exit();
                let state = self
                    .state
                    .lock()
                    .map_err(|_| AppError::message("the output buffer is poisoned"))?;
                let text = state
                    .lines
                    .iter()
                    .map(|line| line.text.as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                Ok(ActionResult::ok(kind, text, 0))
            }
            other => Ok(ActionResult::failed(
                kind,
                format!(
                    "the process backend cannot {:?} — it has no pointer and no window",
                    other.kind()
                ),
                0,
            )),
        }
    }

    fn issues(&mut self) -> Result<Vec<Issue>, AppError> {
        self.poll_exit();
        let mut state = self
            .state
            .lock()
            .map_err(|_| AppError::message("the issue buffer is poisoned"))?;
        let (issues, _) = state.issues.drain_new(usize::MAX);
        Ok(issues)
    }

    fn resolve(&self, _target: &TargetRef) -> Option<Target> {
        None
    }

    fn focused(&self) -> Option<Rect> {
        None
    }

    fn stage(&self) -> Rect {
        Rect {
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 0.0,
        }
    }

    fn close(&mut self) -> Result<(), AppError> {
        let _ = self.child.kill();
        let _ = self.child.wait();
        Ok(())
    }
}

fn read_lines(source: impl std::io::Read, from_stderr: bool, sink: Arc<Mutex<State>>) {
    let reader = BufReader::new(source);
    for line in reader.lines() {
        let Ok(text) = line else { break };
        let Ok(mut state) = sink.lock() else { break };
        state.lines.push_back(Line {
            from_stderr,
            text: text.clone(),
        });
        while state.lines.len() > KEPT_LINES {
            state.lines.pop_front();
        }
        // A CLI writing progress to stderr is not failing, so stderr is a
        // warning. A non-zero exit is the error, and `poll_exit` records it.
        if from_stderr {
            state.issues.push(Level::Warn, "stderr", text, now());
        }
    }
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
