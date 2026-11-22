//! The `engine` backend.
//!
//! Drives a project that provides its own test hook: a command that runs the
//! suite and prints a result. This is the strongest backend where it exists,
//! because the input is simulated inside the project's own loop rather than
//! injected from outside — there is no window to focus, no pointer to move, and
//! nothing to bring forward.
//!
//! It is generic on purpose. A backend with a Unity branch and a Godot branch
//! would be a backend most projects cannot use, and the thing every project with
//! a test hook has in common is a command and a result. So the depth is in the
//! parsers: four formats, each turning a wall of text into one outcome per test,
//! so the agent reads "subtracts failed, expected 1 got 2" instead of scrolling.
//!
//! What it cannot do, it says it cannot do. There is no pointer and no key, so
//! `click`, `hold`, `drag`, `scroll` and `type` are absent from its capabilities
//! rather than silently doing nothing. `tick` is absent for the same reason the
//! `window` backend lost it: a suite has no frames to advance, and a `tick` that
//! runs a suite is a run wearing the wrong word.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::channel::backend::{
    Action, ActionKind, ActionResult, Backend, BackendKind, Capabilities, HostKind, InputKind, Look,
    Target, TargetRef,
};
use crate::channel::guardrail::Rect;
use crate::channel::issues::{Issue, Issues, Level};
use crate::error::AppError;

/// How many tests the digest lists before it says "and N more".
const DIGEST_TESTS: usize = 40;

/// How many output lines the digest keeps.
const DIGEST_LINES: usize = 40;

/// How many issues the buffer keeps.
const ISSUE_CAP: usize = 200;

/// How long a run may take before it is killed.
const DEFAULT_TIMEOUT_MS: u64 = 300_000;

/// The ceiling on a declared timeout, so a typo cannot park a tool call for a day.
const MAX_TIMEOUT_MS: u64 = 1_800_000;

/// How the suite's output is read.
///
/// A format that does not match the output is not a silent pass. The run reports
/// the exit code and says the format found no tests, so a wrong `format` reads as
/// "nothing parsed" rather than "everything is fine".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    /// The exit code, and nothing else. The universal fallback.
    Exit,
    /// Test Anything Protocol: `ok 1 - name`, `not ok 2 - name`.
    Tap,
    /// `cargo test`: `test name ... ok`, `test name ... FAILED`.
    Cargo,
    /// `pytest -v`: `path::name PASSED`, and the `FAILED path::name - message`
    /// summary line.
    Pytest,
}

impl Format {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "exit" => Some(Format::Exit),
            "tap" => Some(Format::Tap),
            "cargo" => Some(Format::Cargo),
            "pytest" => Some(Format::Pytest),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Format::Exit => "exit",
            Format::Tap => "tap",
            Format::Cargo => "cargo",
            Format::Pytest => "pytest",
        }
    }
}

/// What one test did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Passed,
    Failed,
    Skipped,
}

/// One test's outcome.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TestOutcome {
    pub name: String,
    pub status: Status,
    /// The failure message, when the format carried one.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

/// One run of the suite.
#[derive(Debug, Clone)]
pub struct Run {
    pub exit: Option<i32>,
    pub ms: u64,
    pub tests: Vec<TestOutcome>,
    /// The tail of the combined output, for the digest.
    pub output: String,
    /// Set when the run was killed for taking too long.
    pub timed_out: bool,
}

impl Run {
    pub fn passed(&self) -> usize {
        self.count(Status::Passed)
    }

    pub fn failed(&self) -> usize {
        self.count(Status::Failed)
    }

    pub fn skipped(&self) -> usize {
        self.count(Status::Skipped)
    }

    fn count(&self, status: Status) -> usize {
        self.tests.iter().filter(|test| test.status == status).count()
    }

    /// The one-line summary the digest opens with.
    fn summary(&self) -> String {
        let mut line = format!(
            "{} tests, {} passed, {} failed, {} skipped",
            self.tests.len(),
            self.passed(),
            self.failed(),
            self.skipped()
        );
        match self.exit {
            Some(code) => line.push_str(&format!(" (exit {code}, {}ms)", self.ms)),
            None => line.push_str(&format!(" (killed, {}ms)", self.ms)),
        }
        if self.timed_out {
            line.push_str(" — killed for taking too long");
        }
        line
    }
}

/// A channel onto one project's test hook.
pub struct EngineBackend {
    command: String,
    args: Vec<String>,
    cwd: Option<String>,
    format: Format,
    /// How a filter is passed to the runner, with `{filter}` standing in for the
    /// name. Empty means this channel cannot narrow a run, and asking it to says
    /// so rather than quietly running everything.
    filter_args: Vec<String>,
    timeout_ms: u64,
    last: Option<Run>,
    issues: Issues,
    runs: u32,
}

impl EngineBackend {
    /// Open a channel and run the suite once.
    ///
    /// The first run is not a warm-up: `channel_open` returns a look, and for an
    /// engine the only honest look is the result of running it.
    pub fn open(
        command: impl Into<String>,
        args: &[String],
        cwd: Option<&str>,
        format: Format,
        filter_args: &[String],
        timeout_ms: Option<u64>,
    ) -> Result<Self, AppError> {
        let command = command.into();
        if command.trim().is_empty() {
            return Err(AppError::message("an engine channel needs a command"));
        }

        let mut backend = Self {
            command,
            args: args.to_vec(),
            cwd: cwd.map(str::to_string),
            format,
            filter_args: filter_args.to_vec(),
            timeout_ms: timeout_ms
                .unwrap_or(DEFAULT_TIMEOUT_MS)
                .clamp(1_000, MAX_TIMEOUT_MS),
            last: None,
            issues: Issues::new(ISSUE_CAP),
            runs: 0,
        };
        backend.run(None)?;
        Ok(backend)
    }

    /// Run the suite, parse it, and make the issue buffer reflect this run.
    ///
    /// The buffer is cleared first because it describes the *current* state of
    /// the suite, not its history: a test that failed and now passes is not an
    /// issue, and leaving the old entry would make a fixed suite look broken.
    fn run(&mut self, filter: Option<&str>) -> Result<&Run, AppError> {
        let mut args = self.args.clone();
        if let Some(filter) = filter.map(str::trim).filter(|f| !f.is_empty()) {
            if self.filter_args.is_empty() {
                return Err(AppError::message(format!(
                    "this channel was opened without filter_args, so it cannot narrow a run to \
                     \"{filter}\" — re-open it with filter_args, for example [\"-k\", \"{{filter}}\"]"
                )));
            }
            for template in &self.filter_args {
                args.push(template.replace("{filter}", filter));
            }
        }

        let run = execute(
            &self.command,
            &args,
            self.cwd.as_deref(),
            self.format,
            self.timeout_ms,
        )?;
        self.runs += 1;

        self.issues.clear();
        for test in run.tests.iter().filter(|t| t.status == Status::Failed) {
            let text = if test.detail.is_empty() {
                test.name.clone()
            } else {
                format!("{}: {}", test.name, test.detail)
            };
            self.issues.push(Level::Error, "test", text, now());
        }
        // A non-zero exit that no test claimed is still a failure — a build
        // error, a missing binary, a suite that crashed before reporting. Saying
        // nothing here would make a broken run look like a clean one.
        if run.failed() == 0 {
            if let Some(code) = run.exit.filter(|code| *code != 0) {
                self.issues.push(
                    Level::Error,
                    "engine",
                    format!("{} exited with {code} and reported no failing test", self.command),
                    now(),
                );
            }
        }
        if run.timed_out {
            self.issues.push(
                Level::Error,
                "engine",
                format!("{} was killed after {}ms", self.command, self.timeout_ms),
                now(),
            );
        }

        self.last = Some(run);
        Ok(self.last.as_ref().expect("just assigned"))
    }

    fn digest(&self) -> String {
        let Some(run) = self.last.as_ref() else {
            return format!("{} — not run yet", self.command);
        };

        let mut text = format!("{} · {} · {}\n", self.command, self.format.as_str(), run.summary());

        for test in run.tests.iter().take(DIGEST_TESTS) {
            let marker = match test.status {
                Status::Passed => "PASS",
                Status::Failed => "FAIL",
                Status::Skipped => "SKIP",
            };
            if test.detail.is_empty() {
                text.push_str(&format!("  {marker}  {}\n", test.name));
            } else {
                text.push_str(&format!("  {marker}  {} — {}\n", test.name, test.detail));
            }
        }
        if run.tests.len() > DIGEST_TESTS {
            text.push_str(&format!(
                "  … and {} more\n",
                run.tests.len() - DIGEST_TESTS
            ));
        }
        if run.tests.is_empty() {
            text.push_str(&format!(
                "  the {} format found no tests in the output — check the format, or the run \
                 failed before the suite started\n",
                self.format.as_str()
            ));
        }

        if !run.output.is_empty() {
            text.push_str("\n  output:\n");
            let lines: Vec<&str> = run.output.lines().collect();
            let start = lines.len().saturating_sub(DIGEST_LINES);
            for line in &lines[start..] {
                text.push_str(&format!("  | {line}\n"));
            }
        }
        text
    }
}

impl Backend for EngineBackend {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            backend: BackendKind::Engine,
            host: HostKind::Desktop,
            // Headless. Nothing is clicked, so nothing is brought forward.
            foreground: false,
            input: InputKind::Synthetic,
            actions: vec![ActionKind::Run, ActionKind::Wait, ActionKind::Read],
            capture: false,
            issues: true,
        }
    }

    fn look(&mut self, _capture: bool) -> Result<Look, AppError> {
        Ok(Look {
            digest: self.digest(),
            stage: Rect {
                x: 0.0,
                y: 0.0,
                w: 0.0,
                h: 0.0,
            },
            targets: self
                .last
                .as_ref()
                .map(|run| {
                    run.tests
                        .iter()
                        .map(|test| Target {
                            id: test.name.clone(),
                            role: "test".into(),
                            label: test.name.clone(),
                            rect: Rect {
                                x: 0.0,
                                y: 0.0,
                                w: 0.0,
                                h: 0.0,
                            },
                        })
                        .collect()
                })
                .unwrap_or_default(),
            capture: None,
            capture_type: None,
        })
    }

    fn act(&mut self, action: &Action) -> Result<ActionResult, AppError> {
        let kind = action.kind();
        match action {
            Action::Run { filter } => {
                let started = Instant::now();
                let run = self.run(filter.as_deref())?;
                let ms = started.elapsed().as_millis() as u64;
                Ok(ActionResult::ok(kind, run.summary(), ms))
            }
            Action::Wait { ms } => {
                std::thread::sleep(Duration::from_millis(*ms));
                Ok(ActionResult::ok(kind, format!("waited {ms}ms"), *ms))
            }
            Action::Read { target } => {
                let name = match target {
                    TargetRef::Named(name) => name.clone(),
                    TargetRef::Point { x, y } => {
                        return Ok(ActionResult::failed(
                            kind,
                            format!(
                                "a test is named, not pointed at — ({x}, {y}) names nothing here"
                            ),
                            0,
                        ))
                    }
                };
                let found = self
                    .last
                    .as_ref()
                    .and_then(|run| run.tests.iter().find(|test| test.name == name));
                match found {
                    Some(test) if !test.detail.is_empty() => {
                        Ok(ActionResult::ok(kind, test.detail.clone(), 0))
                    }
                    Some(test) => Ok(ActionResult::ok(
                        kind,
                        format!("{} — {:?}, no detail reported", test.name, test.status),
                        0,
                    )),
                    None => Ok(ActionResult::failed(
                        kind,
                        format!("no test called {name} in the last run — call look for the names"),
                        0,
                    )),
                }
            }
            other => Ok(ActionResult::failed(
                kind,
                format!(
                    "the engine backend cannot {:?} — it has no pointer, no key and no window",
                    other.kind()
                ),
                0,
            )),
        }
    }

    fn issues(&mut self) -> Result<Vec<Issue>, AppError> {
        let (issues, _) = self.issues.drain_new(usize::MAX);
        Ok(issues)
    }

    fn resolve(&self, target: &TargetRef) -> Option<Target> {
        let TargetRef::Named(name) = target else {
            return None;
        };
        self.last
            .as_ref()?
            .tests
            .iter()
            .find(|test| &test.name == name)
            .map(|test| Target {
                id: test.name.clone(),
                role: "test".into(),
                label: test.name.clone(),
                rect: Rect {
                    x: 0.0,
                    y: 0.0,
                    w: 0.0,
                    h: 0.0,
                },
            })
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
        // Nothing is left running: a run is waited for, and a run that overran
        // was killed. There is no child to reap.
        Ok(())
    }
}

/// Run a command to completion and parse what it said.
fn execute(
    command: &str,
    args: &[String],
    cwd: Option<&str>,
    format: Format,
    timeout_ms: u64,
) -> Result<Run, AppError> {
    let mut builder = Command::new(command);
    builder
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = cwd {
        builder.current_dir(cwd);
    }

    let started = Instant::now();
    let mut child = builder
        .spawn()
        .map_err(|error| AppError::message(format!("could not start {command}: {error}")))?;

    let collected = Arc::new(Mutex::new(String::new()));
    let (drained, drain) = std::sync::mpsc::channel::<()>();
    let mut readers = 0;
    if let Some(stdout) = child.stdout.take() {
        let sink = collected.clone();
        let drained = drained.clone();
        std::thread::spawn(move || {
            read_into(stdout, sink);
            let _ = drained.send(());
        });
        readers += 1;
    }
    if let Some(stderr) = child.stderr.take() {
        let sink = collected.clone();
        let drained = drained.clone();
        std::thread::spawn(move || {
            read_into(stderr, sink);
            let _ = drained.send(());
        });
        readers += 1;
    }
    drop(drained);

    let deadline = started + Duration::from_millis(timeout_ms);
    let mut timed_out = false;
    let exit = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code(),
            Ok(None) => {
                if Instant::now() >= deadline {
                    kill_tree(&mut child);
                    timed_out = true;
                    break None;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => {
                return Err(AppError::message(format!(
                    "could not wait for {command}: {error}"
                )))
            }
        }
    };

    // A process that starts processes — `npm test` starts node, `cargo test`
    // starts the test binary — can leave a descendant holding the pipes after
    // the child itself is gone. Waiting on the readers without a bound would
    // make the timeout a suggestion, so they get a grace window and whatever
    // arrived inside it is what gets parsed.
    let grace = if timed_out {
        Duration::from_secs(2)
    } else {
        Duration::from_secs(30)
    };
    let until = Instant::now() + grace;
    for _ in 0..readers {
        let left = until.saturating_duration_since(Instant::now());
        if drain.recv_timeout(left).is_err() {
            break;
        }
    }
    let ms = started.elapsed().as_millis() as u64;
    let output = collected
        .lock()
        .map(|text| text.clone())
        .unwrap_or_default();

    Ok(Run {
        exit,
        ms,
        tests: parse(format, &output),
        output,
        timed_out,
    })
}

/// Kill a child and everything it started.
///
/// `Child::kill` kills one process. A test runner is a process that starts
/// processes, and a descendant that survives keeps the pipes open, so the
/// timeout would stop the parent and nothing else. `taskkill /T` walks the live
/// tree at kill time, which a job object cannot do: a job has to be assigned
/// after the spawn, and anything the child starts in that window escapes it.
/// Elsewhere the direct child is all that is killed, and the bounded wait on the
/// readers is what keeps the call from hanging.
fn kill_tree(child: &mut std::process::Child) {
    #[cfg(windows)]
    {
        let pid = child.id().to_string();
        let _ = Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn read_into(source: impl std::io::Read, sink: Arc<Mutex<String>>) {
    let reader = BufReader::new(source);
    for line in reader.lines() {
        let Ok(text) = line else { break };
        let Ok(mut buffer) = sink.lock() else { break };
        buffer.push_str(&text);
        buffer.push('\n');
    }
}

/// Turn the suite's output into one outcome per test.
pub fn parse(format: Format, output: &str) -> Vec<TestOutcome> {
    match format {
        Format::Exit => Vec::new(),
        Format::Tap => parse_tap(output),
        Format::Cargo => parse_cargo(output),
        Format::Pytest => parse_pytest(output),
    }
}

fn tap_line_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^(not ok|ok)\b\s*(\d+)?\s*(?:-\s*)?(.*)$").unwrap())
}

fn tap_directive_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)\s*#\s*(skip|todo)\b\s*(.*)$").unwrap())
}

fn parse_tap(output: &str) -> Vec<TestOutcome> {
    let mut tests: Vec<TestOutcome> = Vec::new();
    let mut pending_failure: Option<usize> = None;
    let mut in_diagnostic = false;
    let mut diagnostic: Vec<String> = Vec::new();

    for line in output.lines() {
        let trimmed = line.trim_end();

        // A `not ok` may be followed by an indented YAML block between `---` and
        // `...`. That block is where TAP puts the reason, and dropping it would
        // leave the agent with a name and no cause.
        if in_diagnostic {
            let body = trimmed.trim_start();
            if body == "..." {
                in_diagnostic = false;
                if let Some(index) = pending_failure.take() {
                    if let Some(test) = tests.get_mut(index) {
                        test.detail = diagnostic_message(&diagnostic);
                    }
                }
                diagnostic.clear();
            } else {
                diagnostic.push(body.to_string());
            }
            continue;
        }

        // A `---` opens the diagnostic block for the failure just recorded.
        if pending_failure.is_some() && trimmed.trim_start() == "---" {
            in_diagnostic = true;
            diagnostic.clear();
            continue;
        }

        let Some(captures) = tap_line_re().captures(trimmed) else {
            continue;
        };

        // A new TAP line closes the window in which a `---` could belong to the
        // failure above it.
        pending_failure = None;

        let failed = &captures[1] == "not ok";
        let mut description = captures[3].trim().to_string();

        let mut status = if failed { Status::Failed } else { Status::Passed };
        if tap_directive_re().is_match(&description) {
            // `# TODO` is a known failure and `# SKIP` is a deliberate one.
            // Neither is a regression, so neither is an error.
            status = Status::Skipped;
            description = tap_directive_re()
                .replace(&description, "")
                .trim()
                .to_string();
        }

        let name = if description.is_empty() {
            format!("test {}", captures.get(2).map(|m| m.as_str()).unwrap_or("?"))
        } else {
            description
        };

        tests.push(TestOutcome {
            name,
            status,
            detail: String::new(),
        });

        if failed {
            pending_failure = Some(tests.len() - 1);
        }
    }

    tests
}

/// The `message:` field of a TAP diagnostic, else the whole block.
fn diagnostic_message(lines: &[String]) -> String {
    for line in lines {
        if let Some(rest) = line.strip_prefix("message:") {
            return rest.trim().trim_matches('\'').trim_matches('"').to_string();
        }
    }
    lines
        .iter()
        .filter(|line| !line.is_empty() && !line.contains(':'))
        .cloned()
        .collect::<Vec<_>>()
        .join(" ")
}

fn cargo_test_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^test (.+) \.\.\. (ok|FAILED|ignored)$").unwrap())
}

fn cargo_block_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^---- (.+) stdout ----$").unwrap())
}

/// Attach a cargo failure block's text to the test it belongs to.
///
/// The block arrives after the `test ... FAILED` line, so the outcome already
/// exists and this fills in the reason rather than adding a second entry.
fn attach_block(name: &str, lines: &mut Vec<String>, tests: &mut [TestOutcome]) {
    if let Some(test) = tests.iter_mut().find(|test| test.name == name) {
        if test.detail.is_empty() {
            test.detail = lines
                .iter()
                .map(|line| line.trim())
                .filter(|line| !line.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
        }
    }
    lines.clear();
}

fn parse_cargo(output: &str) -> Vec<TestOutcome> {
    let mut tests: Vec<TestOutcome> = Vec::new();
    let mut current_block: Option<String> = None;
    let mut block_lines: Vec<String> = Vec::new();

    for line in output.lines() {
        if let Some(captures) = cargo_block_re().captures(line) {
            if let Some(name) = current_block.take() {
                attach_block(&name, &mut block_lines, &mut tests);
            }
            current_block = Some(captures[1].to_string());
            continue;
        }

        if current_block.is_some() {
            // A cargo failure block ends at the next `failures:` heading or at
            // the summary line.
            if line.starts_with("failures:") || line.starts_with("test result:") {
                if let Some(name) = current_block.take() {
                    attach_block(&name, &mut block_lines, &mut tests);
                }
            } else {
                block_lines.push(line.to_string());
                continue;
            }
        }

        let Some(captures) = cargo_test_re().captures(line) else {
            continue;
        };
        let name = captures[1].to_string();
        let status = match &captures[2] {
            "ok" => Status::Passed,
            "ignored" => Status::Skipped,
            _ => Status::Failed,
        };
        tests.push(TestOutcome {
            name,
            status,
            detail: String::new(),
        });
    }

    if let Some(name) = current_block.take() {
        attach_block(&name, &mut block_lines, &mut tests);
    }

    tests
}

fn pytest_verbose_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^(\S+::\S+)\s+(PASSED|FAILED|ERROR|SKIPPED|XFAIL|XPASS)\b").unwrap()
    })
}

fn pytest_summary_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^(FAILED|ERROR)\s+(\S+::\S+)(?:\s+-\s+(.*))?$").unwrap())
}

/// Fold one observation into the list.
///
/// `-v` gives the status and the summary gives the message, so a name seen twice
/// keeps the status from the first and the detail from whichever had one.
fn record(name: String, status: Status, detail: String, tests: &mut Vec<TestOutcome>) {
    if let Some(existing) = tests.iter_mut().find(|test| test.name == name) {
        if existing.detail.is_empty() && !detail.is_empty() {
            existing.detail = detail;
        }
        if existing.status == Status::Passed && status != Status::Passed {
            existing.status = status;
        }
        return;
    }
    tests.push(TestOutcome {
        name,
        status,
        detail,
    });
}

fn parse_pytest(output: &str) -> Vec<TestOutcome> {
    let mut tests: Vec<TestOutcome> = Vec::new();

    for line in output.lines() {
        let trimmed = line.trim_end();

        // `ERROR` is a failure too: a fixture that blew up is not a pass.
        if let Some(captures) = pytest_summary_re().captures(trimmed) {
            record(
                captures[2].to_string(),
                Status::Failed,
                captures
                    .get(3)
                    .map(|m| m.as_str().trim().to_string())
                    .unwrap_or_default(),
                &mut tests,
            );
            continue;
        }

        let Some(captures) = pytest_verbose_re().captures(trimmed) else {
            continue;
        };
        let status = match &captures[2] {
            "PASSED" => Status::Passed,
            "SKIPPED" | "XFAIL" => Status::Skipped,
            _ => Status::Failed,
        };
        record(captures[1].to_string(), status, String::new(), &mut tests);
    }

    tests
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(tests: &[TestOutcome]) -> Vec<(&str, Status)> {
        tests
            .iter()
            .map(|test| (test.name.as_str(), test.status))
            .collect()
    }

    #[test]
    fn tap_reads_passes_failures_and_directives() {
        let output = "\
TAP version 13
ok 1 - adds two numbers
not ok 2 - subtracts
  ---
  message: 'expected 1, got 2'
  ...
ok 3 - divides # SKIP not implemented
ok 4 - multiplies # TODO known broken
1..4
";
        let tests = parse(Format::Tap, output);
        assert_eq!(
            names(&tests),
            vec![
                ("adds two numbers", Status::Passed),
                ("subtracts", Status::Failed),
                ("divides", Status::Skipped),
                ("multiplies", Status::Skipped),
            ]
        );
        assert_eq!(tests[1].detail, "expected 1, got 2");
    }

    #[test]
    fn tap_keeps_a_name_that_contains_a_dash() {
        // The `- ` separator is optional and a name may contain one, so the
        // parser must not split on the first dash it finds.
        let tests = parse(Format::Tap, "ok 1 - parses a - b correctly\n");
        assert_eq!(tests[0].name, "parses a - b correctly");
    }

    #[test]
    fn tap_without_a_description_names_the_test_by_number() {
        let tests = parse(Format::Tap, "not ok 7\n");
        assert_eq!(tests[0].name, "test 7");
        assert_eq!(tests[0].status, Status::Failed);
    }

    #[test]
    fn cargo_reads_the_three_outcomes_and_the_failure_block() {
        let output = "\
running 3 tests
test math::adds ... ok
test math::subtracts ... FAILED
test math::slow ... ignored

failures:

---- math::subtracts stdout ----
thread 'math::subtracts' panicked at src/math.rs:5:5:
assertion `left == right` failed
  left: 1
 right: 2

failures:
    math::subtracts

test result: FAILED. 1 passed; 1 failed; 1 ignored; 0 measured; 0 filtered out
";
        let tests = parse(Format::Cargo, output);
        assert_eq!(
            names(&tests),
            vec![
                ("math::adds", Status::Passed),
                ("math::subtracts", Status::Failed),
                ("math::slow", Status::Skipped),
            ]
        );
        assert!(
            tests[1].detail.contains("assertion `left == right` failed"),
            "the panic message is the detail, got {:?}",
            tests[1].detail
        );
    }

    #[test]
    fn cargo_does_not_mistake_a_test_name_with_dots_for_the_separator() {
        let tests = parse(Format::Cargo, "test a::b::c ... ok\n");
        assert_eq!(tests[0].name, "a::b::c");
    }

    #[test]
    fn pytest_reads_the_verbose_lines_and_the_summary_message() {
        let output = "\
tests/test_math.py::test_adds PASSED
tests/test_math.py::test_subtracts FAILED
tests/test_math.py::test_skipped SKIPPED
=========================== short test summary info ============================
FAILED tests/test_math.py::test_subtracts - AssertionError: assert 1 == 2
========================= 1 failed, 1 passed, 1 skipped =========================
";
        let tests = parse(Format::Pytest, output);
        assert_eq!(
            names(&tests),
            vec![
                ("tests/test_math.py::test_adds", Status::Passed),
                ("tests/test_math.py::test_subtracts", Status::Failed),
                ("tests/test_math.py::test_skipped", Status::Skipped),
            ]
        );
        assert_eq!(
            tests[1].detail,
            "AssertionError: assert 1 == 2",
            "the summary line supplies the message the verbose line lacks"
        );
    }

    #[test]
    fn pytest_reads_a_summary_with_no_verbose_lines() {
        let tests = parse(
            Format::Pytest,
            "FAILED tests/test_a.py::test_one - boom\nERROR tests/test_b.py::test_two\n",
        );
        assert_eq!(
            names(&tests),
            vec![
                ("tests/test_a.py::test_one", Status::Failed),
                ("tests/test_b.py::test_two", Status::Failed),
            ]
        );
        assert_eq!(tests[0].detail, "boom");
    }

    #[test]
    fn a_format_that_does_not_match_finds_nothing_rather_than_passing() {
        // The failure this guards: a wrong `format` reading as a clean run.
        let tests = parse(Format::Cargo, "ok 1 - adds two numbers\n");
        assert!(tests.is_empty());
    }

    #[test]
    fn the_exit_format_reports_no_tests() {
        assert!(parse(Format::Exit, "anything at all\n").is_empty());
    }

    #[test]
    fn a_format_round_trips_through_its_name() {
        for format in [Format::Exit, Format::Tap, Format::Cargo, Format::Pytest] {
            assert_eq!(Format::parse(format.as_str()), Some(format));
        }
        assert_eq!(Format::parse("junit"), None);
    }

    /// A command that prints a fixed script and exits with a fixed code.
    ///
    /// Written as a shell one-liner rather than a fixture file so the test has
    /// no setup and no cleanup, and so the child process is real.
    fn script(body: &str, exit: i32) -> (String, Vec<String>) {
        if cfg!(windows) {
            (
                "cmd".to_string(),
                vec!["/C".to_string(), format!("{body} & exit {exit}")],
            )
        } else {
            (
                "sh".to_string(),
                vec!["-c".to_string(), format!("{body}; exit {exit}")],
            )
        }
    }

    #[test]
    fn a_real_child_process_is_run_and_parsed() {
        let (command, args) = script("echo ok 1 - adds two numbers", 0);
        let mut backend =
            EngineBackend::open(command, &args, None, Format::Tap, &[], None).unwrap();

        let look = backend.look(false).unwrap();
        assert!(look.digest.contains("1 tests, 1 passed, 0 failed"));
        assert!(look.digest.contains("PASS  adds two numbers"));
        assert_eq!(look.targets.len(), 1);
        assert_eq!(look.targets[0].role, "test");
    }

    #[test]
    fn a_failing_test_becomes_an_error_issue_and_a_passing_one_does_not() {
        let (command, args) = script("echo not ok 1 - subtracts", 1);
        let mut backend =
            EngineBackend::open(command, &args, None, Format::Tap, &[], None).unwrap();

        let issues = backend.issues().unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].level, Level::Error);
        assert_eq!(issues[0].source, "test");
        assert!(issues[0].text.contains("subtracts"));
    }

    #[test]
    fn a_non_zero_exit_with_no_failing_test_is_still_an_issue() {
        // A build error, a missing binary, a suite that crashed before it
        // reported. Silence here would make a broken run look clean.
        let (command, args) = script("echo building", 2);
        let mut backend =
            EngineBackend::open(command, &args, None, Format::Tap, &[], None).unwrap();

        let issues = backend.issues().unwrap();
        assert_eq!(issues.len(), 1);
        assert!(issues[0].text.contains("exited with 2"));
    }

    #[test]
    fn run_re_runs_the_suite_and_the_buffer_follows_the_latest_result() {
        // The failure this guards: a test that failed and now passes still
        // reported as an issue, because the old entry was never cleared.
        let (command, args) = script("echo ok 1 - adds two numbers", 0);
        let mut backend =
            EngineBackend::open(command, &args, None, Format::Tap, &[], None).unwrap();
        assert!(backend.issues().unwrap().is_empty());

        let result = backend.act(&Action::Run { filter: None }).unwrap();
        assert!(result.ok);
        assert!(result.detail.contains("1 passed"));
        assert!(backend.issues().unwrap().is_empty());
    }

    #[test]
    fn read_returns_a_named_tests_detail() {
        let (command, args) = script("echo not ok 1 - subtracts", 1);
        let mut backend =
            EngineBackend::open(command, &args, None, Format::Tap, &[], None).unwrap();

        let result = backend
            .act(&Action::Read {
                target: TargetRef::Named("subtracts".into()),
            })
            .unwrap();
        assert!(result.ok);
        assert!(result.detail.contains("subtracts"));
    }

    #[test]
    fn read_of_a_test_that_does_not_exist_says_so() {
        let (command, args) = script("echo ok 1 - adds", 0);
        let mut backend =
            EngineBackend::open(command, &args, None, Format::Tap, &[], None).unwrap();

        let result = backend
            .act(&Action::Read {
                target: TargetRef::Named("nope".into()),
            })
            .unwrap();
        assert!(!result.ok);
        assert!(result.detail.contains("no test called nope"));
    }

    #[test]
    fn a_filter_without_a_template_is_refused_rather_than_running_everything() {
        let (command, args) = script("echo ok 1 - adds", 0);
        let mut backend =
            EngineBackend::open(command, &args, None, Format::Tap, &[], None).unwrap();

        let error = backend
            .act(&Action::Run {
                filter: Some("adds".into()),
            })
            .unwrap_err();
        assert!(error.to_string().contains("filter_args"));
    }

    #[test]
    fn a_filter_is_substituted_into_the_declared_template() {
        // The template is what makes a filter portable: `cargo test <name>`,
        // `pytest -k <name>` and `node --test --test-name-pattern <name>` are
        // three different shapes and the channel declares which one it is.
        let (command, args) = script("echo ok 1 - adds", 0);
        let mut backend = EngineBackend::open(
            command,
            &args,
            None,
            Format::Tap,
            &["--only".to_string(), "{filter}".to_string()],
            None,
        )
        .unwrap();

        let result = backend
            .act(&Action::Run {
                filter: Some("adds".into()),
            })
            .unwrap();
        assert!(result.ok, "the run went through: {}", result.detail);
    }

    #[test]
    fn a_pointer_action_is_refused_with_the_reason() {
        let (command, args) = script("echo ok 1 - adds", 0);
        let mut backend =
            EngineBackend::open(command, &args, None, Format::Tap, &[], None).unwrap();

        let result = backend
            .act(&Action::Click {
                target: TargetRef::Point { x: 1.0, y: 2.0 },
                button: Default::default(),
                count: 1,
            })
            .unwrap();
        assert!(!result.ok);
        assert!(result.detail.contains("no pointer"));
    }

    #[test]
    fn the_capabilities_do_not_advertise_a_pointer_or_a_frame_clock() {
        let (command, args) = script("echo ok 1 - adds", 0);
        let backend = EngineBackend::open(command, &args, None, Format::Tap, &[], None).unwrap();

        let capabilities = backend.capabilities();
        assert_eq!(capabilities.backend, BackendKind::Engine);
        assert!(!capabilities.foreground, "a headless run takes no screen");
        assert!(!capabilities.capture, "there are no pixels to capture");
        assert!(capabilities.supports(ActionKind::Run));
        assert!(capabilities.supports(ActionKind::Read));
        assert!(capabilities.supports(ActionKind::Wait));
        for absent in [
            ActionKind::Click,
            ActionKind::Hold,
            ActionKind::Drag,
            ActionKind::Scroll,
            ActionKind::Type,
            ActionKind::Tick,
        ] {
            assert!(
                !capabilities.supports(absent),
                "{absent:?} must not be advertised"
            );
        }
    }

    #[test]
    fn a_command_that_does_not_exist_fails_at_open() {
        let opened = EngineBackend::open(
            "definitely-not-a-real-binary-xyz",
            &[],
            None,
            Format::Exit,
            &[],
            None,
        );
        let error = opened.err().expect("a missing binary must fail at open");
        assert!(error.to_string().contains("could not start"));
    }

    #[test]
    fn a_run_that_overruns_is_killed_and_reported() {
        let (command, args) = if cfg!(windows) {
            (
                "cmd".to_string(),
                vec!["/C".to_string(), "ping -n 30 127.0.0.1 >NUL".to_string()],
            )
        } else {
            ("sh".to_string(), vec!["-c".to_string(), "sleep 30".to_string()])
        };

        let started = Instant::now();
        let mut backend =
            EngineBackend::open(command, &args, None, Format::Exit, &[], Some(1_000)).unwrap();
        let elapsed = started.elapsed();

        assert!(
            elapsed < Duration::from_secs(20),
            "the run was killed rather than waited out, took {elapsed:?}"
        );
        let issues = backend.issues().unwrap();
        assert!(
            issues.iter().any(|issue| issue.text.contains("killed")),
            "the kill is reported, got {issues:?}"
        );
        assert!(backend.look(false).unwrap().digest.contains("killed"));
    }

    /// One JSON-RPC call, the way the agent makes it.
    fn call(name: &str, arguments: serde_json::Value) -> serde_json::Value {
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": name, "arguments": arguments },
        });
        crate::channel::mcp::dispatch(&request).expect("the server has to answer")
    }

    fn body_of(reply: &serde_json::Value) -> serde_json::Value {
        let text = reply["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default();
        serde_json::from_str(text)
            .unwrap_or_else(|error| panic!("the tool has to answer with json: {error} — {text}"))
    }

    #[test]
    fn the_tool_surface_runs_a_real_suite_end_to_end() {
        // The whole path the agent takes, against a real test runner rather than
        // a script that prints TAP: open, look, act, expect, close over the same
        // JSON-RPC the MCP server speaks.
        let dir = std::env::temp_dir().join(format!("vibedesk-engine-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("a.test.mjs"),
            "import { test } from 'node:test';\n\
             import assert from 'node:assert';\n\
             test('adds', () => { assert.equal(1 + 1, 2); });\n\
             test('subtracts', () => { assert.equal(3 - 1, 2); });\n\
             test('fails on purpose', () => { assert.equal(1, 2); });\n",
        )
        .unwrap();

        let opened = call(
            "channel_open",
            serde_json::json!({
                "backend": "engine",
                "command": "node",
                "args": ["--test", "--test-reporter=tap", "a.test.mjs"],
                "cwd": dir.to_string_lossy(),
                "format": "tap",
            }),
        );
        let body = body_of(&opened);
        let id = body["id"].as_str().expect("open has to name the channel");
        assert_eq!(body["capabilities"]["foreground"], false);
        assert_eq!(body["capabilities"]["capture"], false);

        let look = &body["look"];
        assert!(
            look["digest"]
                .as_str()
                .unwrap_or_default()
                .contains("3 tests, 2 passed, 1 failed"),
            "the real suite has to be counted, got {look}"
        );
        let names: Vec<&str> = look["targets"]
            .as_array()
            .expect("a look has targets")
            .iter()
            .filter_map(|target| target["label"].as_str())
            .collect();
        assert!(names.contains(&"adds"), "got {names:?}");
        assert!(names.contains(&"fails on purpose"), "got {names:?}");

        // A failing test is an error issue, so `channel_expect` fails the run.
        let checked = call("channel_expect", serde_json::json!({ "id": id }));
        let verdict = body_of(&checked);
        assert_eq!(
            verdict["ok"], false,
            "a failing suite has to fail the assertion: {verdict}"
        );
        assert!(
            verdict["issues"]
                .as_array()
                .expect("expect reports its issues")
                .iter()
                .any(|issue| issue["text"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("fails on purpose")),
            "the failing test has to be named: {verdict}"
        );

        // Reading a named test returns its detail rather than the whole log.
        let read = call(
            "channel_act",
            serde_json::json!({
                "id": id,
                "actions": [{ "kind": "read", "target": "fails on purpose" }],
            }),
        );
        let outcome = body_of(&read);
        assert_eq!(outcome["ok"], true, "the read has to be accepted: {outcome}");

        let closed = call("channel_close", serde_json::json!({ "id": id }));
        assert!(
            !closed["result"]["isError"].as_bool().unwrap_or(false),
            "close has to succeed"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_filter_without_a_template_is_refused_over_the_tool_surface() {
        let (command, args) = script("echo ok 1 - adds", 0);
        let opened = call(
            "channel_open",
            serde_json::json!({
                "backend": "engine",
                "command": command,
                "args": args,
                "format": "tap",
            }),
        );
        let body = body_of(&opened);
        let id = body["id"].as_str().expect("open has to name the channel");

        let acted = call(
            "channel_act",
            serde_json::json!({
                "id": id,
                "actions": [{ "kind": "run", "filter": "adds" }],
            }),
        );
        let outcome = body_of(&acted);
        assert_eq!(
            outcome["ok"], false,
            "a filter with no template has to be refused: {outcome}"
        );
        assert!(
            outcome["results"][0]["detail"]
                .as_str()
                .unwrap_or_default()
                .contains("filter_args"),
            "the refusal has to name what is missing, got {outcome}"
        );

        let _ = call("channel_close", serde_json::json!({ "id": id }));
    }
}
