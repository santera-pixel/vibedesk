//! Proving a verdict, rather than reading one.
//!
//! A verifier that says "I wrote a test that fails without the change" is making
//! a claim. The claim is the whole value of the adversarial role, and it is also
//! the easiest thing in the run to fake — by accident as much as on purpose. A
//! test that passes on the base commit is not testing the change; it is testing
//! something else and reporting green.
//!
//! So the engine runs the command itself, twice: once in the base checkout and
//! once in the change. `failing_first` is then an observation — non-zero on the
//! base, zero on the change — rather than a phrase the verifier used.
//!
//! Two things this deliberately does not do. It does not decide whether the
//! command is a *good* test; a command that exits 1 on the base because it
//! references a file that does not exist yet is a weak proof, and the panel says
//! so by showing both exit codes beside the command. And it does not sandbox:
//! the command runs with the same privileges as the app, which is why the
//! verifier's own profile is the boundary and this is the check.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long one side of a proof may run.
///
/// A test suite that takes longer than this is not a failing-first test, it is a
/// build, and holding the run's tick open for it would stall every other task.
pub const PROOF_TIMEOUT: Duration = Duration::from_secs(120);

/// What running the command on both sides showed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proof {
    /// The exit code on the base checkout. `None` when it could not be run.
    pub base_exit: Option<i32>,
    /// The exit code on the change. `None` when it could not be run.
    pub change_exit: Option<i32>,
    /// Whether the command failed on the base and passed on the change.
    pub failing_first: bool,
    /// Why the proof is not what it looks like, when it is not.
    pub note: Option<String>,
    /// The last line the base run printed, when it printed one.
    ///
    /// An exit code on its own is a thin thing to hand a verifier: `exit 1` on
    /// the base is a failing assertion or a missing file, and the two mean
    /// different things about the change. The line the command ended on is what
    /// makes the code readable, and it is quoted as output rather than as a
    /// conclusion — the engine cannot tell which line of a test run mattered,
    /// and claiming to would be the guess the verifier is being taken off.
    pub base_line: Option<String>,
    /// The last line the change run printed, when it printed one.
    pub change_line: Option<String>,
}

impl Proof {
    /// A proof that could not be attempted, with the reason.
    pub fn unattempted(reason: impl Into<String>) -> Self {
        Self {
            base_exit: None,
            change_exit: None,
            failing_first: false,
            note: Some(reason.into()),
            base_line: None,
            change_line: None,
        }
    }
}

/// The shell a command string is handed to.
///
/// A command is a string because that is what a verifier writes and what a user
/// reads. Handing it to a shell is the only way to honour it, and it is also why
/// the command is shown on the board rather than hidden.
fn shell(command: &str) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // bash first, because that is the shell the criterion was written for.
        //
        // The agent runs its own commands through bash, so a verifier writes its
        // criterion in bash — `$(...)`, `for ...; do ... done`, `grep -qF`. The
        // engine re-runs that criterion to turn the verdict into an observation,
        // and it used to hand it to `cmd`, where it was a syntax error. The
        // fourteenth live run's v-t1 passed t1 with a compound bash criterion;
        // the engine's own re-run exited 1 on a parse error, the task was failed
        // on its second attempt, and the run died with "no task can make
        // progress" over work that was green. A shell that cannot parse the
        // command is not an observation about the change.
        if let Some(bash) = bash_path() {
            let mut cmd = Command::new(bash);
            cmd.arg("-c").arg(command);
            return cmd;
        }
        let mut cmd = Command::new("cmd");
        // `raw_arg`, not `arg`. Rust quotes an argument that contains spaces and
        // escapes its inner quotes, which `cmd` does not understand: a command
        // like `python -c "assert x"` reached the interpreter as an escaped
        // quote and died with a syntax error. A verifier command is a command
        // line, not an argument, so it is appended verbatim.
        cmd.raw_arg("/C").raw_arg(command);
        cmd
    }
    #[cfg(not(windows))]
    {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(command);
        cmd
    }
}

/// Where bash is, if it is anywhere.
///
/// `VIBEDESK_BASH` first, so a machine with an unusual install can say where it
/// is; then `PATH`; then the two places Git for Windows puts it. The app is
/// usually launched from a shell that has bash on `PATH`, but a user who
/// double-clicks the icon does not, and a criterion that cannot be parsed is a
/// wrong answer rather than a missing one.
#[cfg(windows)]
fn bash_path() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;

    if let Some(value) = std::env::var_os("VIBEDESK_BASH") {
        let path = PathBuf::from(value);
        if path.is_file() {
            return Some(path);
        }
    }

    let on_path = Command::new("bash")
        .arg("-c")
        .arg("exit 0")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false);
    if on_path {
        return Some(PathBuf::from("bash"));
    }

    let mut candidates: Vec<PathBuf> = Vec::new();
    for root in ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"] {
        if let Some(dir) = std::env::var_os(root) {
            candidates.push(PathBuf::from(&dir).join("Git").join("bin").join("bash.exe"));
            candidates.push(PathBuf::from(&dir).join("Git").join("usr").join("bin").join("bash.exe"));
        }
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        candidates.push(
            PathBuf::from(local)
                .join("Programs")
                .join("Git")
                .join("bin")
                .join("bash.exe"),
        );
    }
    candidates.into_iter().find(|path| path.is_file())
}

/// Run one command and keep what it printed.
///
/// `run_in` answers "did it exit zero", which is all a failing-first proof
/// needs. The integrated suite needs more: when it fails, the run has to say
/// *which* task broke it, and the only evidence for that is the output. The
/// tail is capped because a failing suite can print megabytes and the board is
/// a file the panel reads on every tick.
pub fn run_capture(dir: &Path, command: &str, timeout: Duration) -> Result<(i32, String), String> {
    if !dir.is_dir() {
        return Err(format!("{} is not a directory", dir.display()));
    }

    let mut child = shell(command)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("could not run `{command}`: {error}"))?;

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let output = child
                    .wait_with_output()
                    .map(|out| {
                        let mut text = String::from_utf8_lossy(&out.stdout).to_string();
                        text.push_str(&String::from_utf8_lossy(&out.stderr));
                        text
                    })
                    .unwrap_or_default();
                return Ok((status.code().unwrap_or(-1), tail(&output, OUTPUT_TAIL)));
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "`{command}` did not finish within {}s",
                        timeout.as_secs()
                    ));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => return Err(format!("could not wait on `{command}`: {error}")),
        }
    }
}

/// How much of a command's output is kept.
const OUTPUT_TAIL: usize = 4000;

/// How much of the deciding line is kept.
///
/// A test runner's last line is short. A JSON dump's is not, and the brief is a
/// prompt: a thousand characters of one line is context the verifier spends
/// before it has read the task.
const LINE_TAIL: usize = 300;

/// The last line a command printed, which is the line it ended on.
///
/// The last non-empty line, because that is where a test runner puts its
/// verdict — `8 tests passed`, `1 test failed`, `AssertionError: ...`. A run
/// that printed nothing has no line, and `None` says so rather than inventing
/// one.
fn deciding_line(output: &str) -> Option<String> {
    let line = output
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())?
        .trim();
    if line.chars().count() <= LINE_TAIL {
        return Some(line.to_string());
    }
    Some(format!("…{}", tail(line, LINE_TAIL)))
}

/// The last `n` characters of a string, on a character boundary.
fn tail(text: &str, n: usize) -> String {
    let count = text.chars().count();
    if count <= n {
        return text.to_string();
    }
    text.chars().skip(count - n).collect()
}

/// Run one command in one directory, with a deadline.
///
/// Returns the exit code, or an error naming why there is none. A command that
/// runs past the deadline is killed and reported as a timeout rather than as a
/// failure: "it did not finish" and "it exited 1" are different facts, and
/// conflating them would let a hanging test read as a failing-first proof.
pub fn run_in(dir: &Path, command: &str, timeout: Duration) -> Result<i32, String> {
    if !dir.is_dir() {
        return Err(format!("{} is not a directory", dir.display()));
    }

    let mut child = shell(command)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("could not run `{command}`: {error}"))?;

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return Ok(status.code().unwrap_or(-1));
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "`{command}` did not finish within {}s",
                        timeout.as_secs()
                    ));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => return Err(format!("could not wait on `{command}`: {error}")),
        }
    }
}

/// Run the command on the base checkout and on the change, and say what it means.
///
/// The order matters for the note: a command that fails on *both* sides is a
/// broken command, not a proof, and saying so is more useful than reporting
/// `failing_first: false` and leaving the reader to guess which side was wrong.
pub fn prove(base: &Path, change: &Path, command: &str, timeout: Duration) -> Proof {
    let command = command.trim();
    if command.is_empty() {
        return Proof::unattempted("the verdict named no command to run");
    }

    // `run_capture` rather than `run_in`: the exit code is what `failing_first`
    // is computed from, but the line the run ended on is what the verifier is
    // handed instead of being asked to reproduce the run.
    let (base_exit, base_output) = match run_capture(base, command, timeout) {
        Ok(ran) => ran,
        Err(error) => return Proof::unattempted(format!("on the base checkout, {error}")),
    };
    let (change_exit, change_output) = match run_capture(change, command, timeout) {
        Ok(ran) => ran,
        Err(error) => return Proof::unattempted(format!("on the change, {error}")),
    };

    let failing_first = base_exit != 0 && change_exit == 0;
    let note = if failing_first {
        None
    } else if base_exit == 0 && change_exit == 0 {
        Some(format!(
            "`{command}` passes on the base checkout too, so it does not test the change"
        ))
    } else if base_exit != 0 && change_exit != 0 {
        Some(format!(
            "`{command}` fails on the change as well, so it proves nothing about it"
        ))
    } else {
        Some(format!(
            "`{command}` passes on the base checkout and fails on the change"
        ))
    };

    Proof {
        base_exit: Some(base_exit),
        change_exit: Some(change_exit),
        failing_first,
        note,
        base_line: deciding_line(&base_output),
        change_line: deciding_line(&change_output),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("vd-prove-{name}-{}", super::super::new_run_id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The command a test suite would use. The engine runs criteria through
    /// bash on every platform, so the tests are written in bash on every
    /// platform — a test that branched on `cfg!(windows)` would stop covering
    /// the shell the engine actually uses.
    const PASS: &str = "exit 0";
    const FAIL: &str = "exit 1";

    #[test]
    fn a_command_that_fails_on_the_base_and_passes_on_the_change_is_a_proof() {
        let base = scratch("base");
        let change = scratch("change");
        // The base has no marker file; the change does. One command, two answers.
        std::fs::write(change.join("marker"), "x").unwrap();
        let command = "test -f marker";

        let proof = prove(&base, &change, command, Duration::from_secs(20));
        assert_eq!(proof.base_exit, Some(1));
        assert_eq!(proof.change_exit, Some(0));
        assert!(proof.failing_first);
        assert!(proof.note.is_none());

        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&change);
    }

    /// The verifier is handed the engine's result instead of being asked to
    /// reproduce it, and an exit code alone is not a result it can read: `exit
    /// 1` on the base is a failing assertion or a missing file. The line the run
    /// ended on is what tells the two apart.
    #[test]
    fn a_proof_carries_the_line_each_side_ended_on() {
        let base = scratch("line-base");
        let change = scratch("line-change");
        std::fs::write(change.join("marker"), "x").unwrap();
        let command = "test -f marker || { echo 'no marker here'; exit 1; }; echo 'marker found'";

        let proof = prove(&base, &change, command, Duration::from_secs(20));
        assert_eq!(proof.base_exit, Some(1));
        assert_eq!(proof.change_exit, Some(0));
        assert_eq!(proof.base_line.as_deref(), Some("no marker here"));
        assert_eq!(proof.change_line.as_deref(), Some("marker found"));

        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&change);
    }

    /// A run that printed nothing has no deciding line, and inventing one would
    /// be the engine asserting what it did not see.
    #[test]
    fn a_silent_run_has_no_deciding_line() {
        assert_eq!(deciding_line(""), None);
        assert_eq!(deciding_line("   \n\n  \n"), None);
        assert_eq!(deciding_line("first\n\nlast\n\n"), Some("last".into()));
    }

    /// The line is capped, because a JSON dump on one line is context the
    /// verifier spends before it has read the task.
    #[test]
    fn a_very_long_deciding_line_is_cut_from_the_front() {
        let long = format!("{}the end", "x".repeat(LINE_TAIL * 2));
        let line = deciding_line(&long).unwrap();
        assert!(line.starts_with('…'), "{line}");
        assert!(line.ends_with("the end"), "the end is what decided it: {line}");
        assert_eq!(line.chars().count(), LINE_TAIL + 1);
    }

    #[test]
    fn a_command_that_passes_on_both_sides_is_not_a_proof_and_says_why() {
        let base = scratch("both-base");
        let change = scratch("both-change");
        let proof = prove(&base, &change, PASS, Duration::from_secs(20));
        assert_eq!(proof.base_exit, Some(0));
        assert_eq!(proof.change_exit, Some(0));
        assert!(!proof.failing_first);
        let note = proof.note.unwrap();
        assert!(note.contains("passes on the base checkout too"), "{note}");

        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&change);
    }

    #[test]
    fn a_command_that_fails_on_both_sides_is_a_broken_command_not_a_proof() {
        let base = scratch("fail-base");
        let change = scratch("fail-change");
        let proof = prove(&base, &change, FAIL, Duration::from_secs(20));
        assert!(!proof.failing_first);
        let note = proof.note.unwrap();
        assert!(note.contains("fails on the change as well"), "{note}");

        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&change);
    }

    #[test]
    fn a_verdict_with_no_command_is_unattempted_rather_than_failed() {
        let base = scratch("none-base");
        let change = scratch("none-change");
        let proof = prove(&base, &change, "   ", Duration::from_secs(5));
        assert_eq!(proof.base_exit, None);
        assert_eq!(proof.change_exit, None);
        assert!(!proof.failing_first);
        assert!(proof.note.unwrap().contains("named no command"));

        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&change);
    }

    #[test]
    fn a_command_that_never_finishes_is_a_timeout_not_a_failure() {
        let base = scratch("slow-base");
        let change = scratch("slow-change");
        let proof = prove(&base, &change, "sleep 30", Duration::from_millis(300));
        assert_eq!(proof.base_exit, None, "a timeout is not an exit code");
        assert!(!proof.failing_first);
        assert!(proof.note.unwrap().contains("did not finish"));

        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&change);
    }

    #[test]
    fn a_missing_directory_is_named_rather_than_run_in() {
        let missing = std::env::temp_dir().join("vd-prove-does-not-exist");
        let error = run_in(&missing, PASS, Duration::from_secs(5)).unwrap_err();
        assert!(error.contains("is not a directory"), "{error}");
    }

    /// A command with quotes in it survives the shell.
    ///
    /// A regression test for a real bug: `Command::arg` escaped the inner
    /// quotes, `cmd` passed the escape through, and every verifier command of
    /// the form `python -c "..."` failed with a syntax error instead of running.
    /// The live run found it; this keeps it found.
    #[test]
    fn a_command_with_quotes_reaches_the_program_intact() {
        let dir = scratch("quotes");
        let command = if cfg!(windows) {
            "python -c \"print('quoted')\""
        } else {
            "python3 -c \"print('quoted')\""
        };
        match run_in(&dir, command, Duration::from_secs(30)) {
            Ok(code) => assert_eq!(code, 0, "the quoted command must run"),
            Err(error) => assert!(
                !error.contains("did not finish"),
                "a quoted command must not hang: {error}"
            ),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_command_runs_in_the_directory_it_was_given() {
        let dir = scratch("cwd");
        std::fs::write(dir.join("here"), "x").unwrap();
        assert_eq!(run_in(&dir, "test -f here", Duration::from_secs(20)), Ok(0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `run_capture` is what the integrated suite runs through, and the whole
    /// point of it is the output: a red suite that reports only "exited 1"
    /// tells the user nothing about which test broke.
    #[test]
    fn run_capture_returns_the_exit_code_and_the_output_together() {
        let dir = scratch("capture");
        let (code, output) =
            run_capture(&dir, "echo suite-says-hello; exit 3", Duration::from_secs(20)).unwrap();
        assert_eq!(code, 3, "the exit code must survive: {output}");
        assert!(
            output.contains("suite-says-hello"),
            "the output must survive: {output}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The regression test for the fourteenth live run.
    ///
    /// v-t1 passed t1 with a compound bash criterion. The engine re-ran that
    /// criterion through `cmd`, where `$(...)` and `for ...; do ... done` are
    /// syntax errors, read the parse error as a failing observation, failed a
    /// green task on its second attempt, and the run died with "no task can
    /// make progress". A criterion is written for the shell the agent uses, so
    /// the engine has to use that shell.
    #[test]
    fn a_criterion_written_in_bash_is_parseable() {
        let dir = scratch("bash-syntax");
        std::fs::write(dir.join("a.txt"), "x").unwrap();
        std::fs::write(dir.join("b.txt"), "x").unwrap();

        // Command substitution, a loop, and a `&&` chain: all bash, all of it
        // a syntax error under `cmd`.
        let command = "count=$(for f in a.txt b.txt; do test -f \"$f\" && echo y; done | wc -l)             && test \"$count\" -eq 2";
        assert_eq!(
            run_in(&dir, command, Duration::from_secs(20)),
            Ok(0),
            "a bash criterion must be run by bash"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_capture_reports_a_timeout_rather_than_a_failure() {
        let dir = scratch("capture-slow");
        let error = run_capture(&dir, "sleep 30", Duration::from_millis(300)).unwrap_err();
        assert!(error.contains("did not finish"), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_capture_names_a_missing_directory_instead_of_running_in_it() {
        let missing = std::env::temp_dir().join("vd-prove-capture-does-not-exist");
        let error = run_capture(&missing, PASS, Duration::from_secs(5)).unwrap_err();
        assert!(error.contains("is not a directory"), "{error}");
    }

    /// The tail is cut by characters, not bytes.
    ///
    /// A byte slice at a fixed offset panics on a multi-byte boundary, and test
    /// output is full of them: box-drawing characters, arrows, accented names.
    /// A panic here would take down the drive loop at the exact moment a run is
    /// already failing.
    #[test]
    fn the_tail_cuts_on_a_character_boundary() {
        let text = "→→→→→→→→→→";
        assert_eq!(tail(text, 4), "→→→→");
        assert_eq!(tail(text, 100), text, "short text is returned whole");
        assert_eq!(tail("", 4), "");
        // Mixed widths, cut in the middle of the run of wide characters.
        // "abc→def→ghi" is eleven characters; the last five are "f→ghi".
        let mixed = "abc→def→ghi";
        assert_eq!(tail(mixed, 5), "f→ghi");
    }
}
