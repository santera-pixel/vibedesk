//! Mid-run plan amendments.
//!
//! A worker that breaks a file no task owns is right to stop. The sixth live run
//! stopped on `test/integration.test.js`: t1 changed the gate contract, the file
//! still called the old functions, and no task in the plan owned it. The seventh
//! stopped on the same shape one file over. Both diagnoses were correct and both
//! runs ended, which is the wrong outcome — the question has an answer, and the
//! lead is the one who has it.
//!
//! So an escalation goes to the lead first. The lead writes an amendment that
//! assigns the unowned files to tasks that already exist, and the run resumes.
//!
//! The contract is deliberately narrow. An amendment may **add files to existing
//! tasks**, and it may **rewrite the acceptance criterion of a task that has not
//! been verified**. It may not add tasks, remove tasks, change dependencies, or
//! touch a task that is already verified. Those are the changes that would
//! invalidate work the run has already paid for, and a coordinator that can
//! rewrite the plan under a running worker is a coordinator that can lose the
//! run's result. When the narrow answer is not enough, the run escalates to the
//! user instead — which is what `MAX_AMENDMENTS` is for.
//!
//! The criterion is in the contract because a criterion can be *unprovable* in a
//! way no worker can fix. The fifteenth live run's t1 was told to prove itself
//! with `node --test test/gates.test.js` while owning that test file: the base
//! side of the failing-first proof checks out the base commit, which carries the
//! base version of the test, which passes. The criterion could not distinguish
//! base from change, and the worker could not change the command it was judged
//! by. The verifier said so three times and the run died. A criterion is the
//! lead's writing, so the lead is who rewrites it — and the verifier still has
//! to prove the new one failing-first, so a lead that weakens a criterion to get
//! a pass is caught by the same adversarial check that caught the original.

use std::collections::BTreeMap;

use serde::Deserialize;

use super::{Run, TaskState};

/// How many times the lead may amend a plan before the question goes to the user.
///
/// Two is enough for the shape this exists for: a worker names the files it
/// broke, the lead assigns them, and the run continues. A lead that needs a third
/// amendment is not coordinating, it is guessing, and the user should see it.
pub const MAX_AMENDMENTS: u32 = 2;

/// What the lead writes to `amendment.json`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Amendment {
    /// Task id → the files that task now also owns.
    #[serde(default)]
    pub files: BTreeMap<String, Vec<String>>,
    /// Task id → a replacement acceptance criterion.
    ///
    /// Only for a task that has not been verified, and only when the old
    /// criterion could not be proven — the verifier still has to prove the new
    /// one failing-first, so this cannot be used to buy a pass.
    #[serde(default)]
    pub acceptance: BTreeMap<String, String>,
    /// Why, in the lead's words. Shown on the board.
    #[serde(default)]
    pub reason: String,
}

impl Amendment {
    /// Read an amendment from JSON.
    ///
    /// An amendment that changes nothing is a valid answer when it carries a
    /// reason: it is the lead saying the narrow fix is not enough and the
    /// question is the user's. The brief tells the lead to write exactly that,
    /// and the fourteenth live run's lead did — `{"files": {}, "reason": "..."}`
    /// — and the engine refused it as "changes nothing", escalated with the
    /// refusal instead of the lead's reason, and threw away the one thing the
    /// lead had produced. A channel the brief documents and the parser rejects
    /// is worse than no channel.
    pub fn parse(text: &str) -> Result<Self, String> {
        let amendment: Amendment =
            serde_json::from_str(text).map_err(|e| format!("amendment.json is not valid: {e}"))?;
        if amendment.is_ask() && amendment.reason.trim().is_empty() {
            return Err(
                "amendment.json changes nothing — write {\"files\": {\"t2\": [\"path\"]}} or {\"acceptance\": {\"t2\": \"command\"}}, or write {\"files\": {}, \"reason\": \"why the plan cannot be fixed\"} to stop and ask the user".into(),
            );
        }
        Ok(amendment)
    }

    /// Whether this amendment is the lead handing the question to the user.
    ///
    /// No files, no criteria, and a reason: the narrow fix is not enough.
    pub fn is_ask(&self) -> bool {
        self.files.is_empty() && self.acceptance.is_empty()
    }

    /// Every file the amendment assigns, in a stable order.
    pub fn paths(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .files
            .values()
            .flatten()
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .collect();
        out.sort();
        out.dedup();
        out
    }
}

/// Why an amendment was refused.
///
/// The two cases arrive at the caller as the same `Err` until the error is
/// typed: a no-op amendment is the lead saying the plan is already right, and
/// an invalid one is the lead getting the plan wrong. The first is benign —
/// the run carries on — and only the second is a refusal.
#[derive(Debug, PartialEq)]
pub enum AmendError {
    /// Well-formed, but nothing in it changes the plan.
    NoOp,
    /// The amendment itself is wrong; the string says why.
    Invalid(String),
}

/// Apply an amendment to a run, returning what changed.
///
/// All-or-nothing: one unknown task id or one verified task refuses the whole
/// amendment, because a half-applied plan is a plan nobody can reason about.
/// An amendment that changes nothing is not a refusal: it is [`AmendError::NoOp`].
pub fn apply(run: &mut Run, amendment: &Amendment) -> Result<Vec<String>, AmendError> {
    // Check every assignment before touching anything.
    for (task_id, files) in &amendment.files {
        let Some(task) = run.task(task_id) else {
            let known: Vec<&str> = run.tasks.iter().map(|t| t.id.as_str()).collect();
            return Err(AmendError::Invalid(format!(
                "amendment names task {task_id}, which is not in the plan (the plan has {})",
                known.join(", ")
            )));
        };
        if task.state == TaskState::Verified {
            return Err(AmendError::Invalid(format!(
                "amendment names task {task_id}, which is already verified — a verified task's files are settled"
            )));
        }
        if files.iter().all(|f| f.trim().is_empty()) {
            return Err(AmendError::Invalid(format!(
                "amendment assigns no files to {task_id}"
            )));
        }
    }

    for (task_id, criterion) in &amendment.acceptance {
        let Some(task) = run.task(task_id) else {
            let known: Vec<&str> = run.tasks.iter().map(|t| t.id.as_str()).collect();
            return Err(AmendError::Invalid(format!(
                "amendment names task {task_id}, which is not in the plan (the plan has {})",
                known.join(", ")
            )));
        };
        if task.state == TaskState::Verified {
            return Err(AmendError::Invalid(format!(
                "amendment rewrites the criterion of {task_id}, which is already verified — a verified task's criterion is settled"
            )));
        }
        if criterion.trim().is_empty() {
            return Err(AmendError::Invalid(format!(
                "amendment gives {task_id} an empty criterion"
            )));
        }
    }

    let mut changed = Vec::new();
    for (task_id, criterion) in &amendment.acceptance {
        let Some(task) = run.task_mut(task_id) else {
            continue;
        };
        let criterion = criterion.trim().to_string();
        if task.acceptance == criterion {
            continue;
        }
        task.acceptance = criterion.clone();
        changed.push(format!("{task_id} is now judged by `{criterion}`"));
    }
    for (task_id, files) in &amendment.files {
        let Some(task) = run.task_mut(task_id) else {
            continue;
        };
        for file in files {
            let file = file.trim().to_string();
            if file.is_empty() || task.files.contains(&file) {
                continue;
            }
            task.files.push(file.clone());
            changed.push(format!("{task_id} now owns {file}"));
        }
    }
    if changed.is_empty() {
        return Err(AmendError::NoOp);
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swarm::{Member, Role, Task};

    fn run_with(tasks: Vec<Task>) -> Run {
        let mut run = Run::new("goal", "D:/ws", Member::new("lead", Role::Lead, "m"));
        run.tasks = tasks;
        run
    }

    fn task(id: &str, files: &[&str]) -> Task {
        let mut task = Task::new(id, "title", "detail", "criterion");
        task.files = files.iter().map(|f| f.to_string()).collect();
        task
    }

    /// Unwrap an `Invalid` refusal into its message.
    fn invalid(error: AmendError) -> String {
        match error {
            AmendError::Invalid(message) => message,
            AmendError::NoOp => panic!("expected an invalid amendment, got a no-op"),
        }
    }

    #[test]
    fn an_amendment_adds_a_file_to_an_existing_task() {
        let mut run = run_with(vec![task("t1", &["src/a.js"]), task("t2", &["src/b.js"])]);
        let amendment = Amendment::parse(
            r#"{"files": {"t2": ["test/integration.test.js"]}, "reason": "t1 broke it"}"#,
        )
        .unwrap();
        let changed = apply(&mut run, &amendment).unwrap();
        assert_eq!(changed, vec!["t2 now owns test/integration.test.js"]);
        assert_eq!(
            run.task("t2").unwrap().files,
            vec!["src/b.js".to_string(), "test/integration.test.js".to_string()]
        );
        assert_eq!(amendment.reason, "t1 broke it");
    }

    #[test]
    fn an_amendment_may_assign_one_file_to_several_tasks() {
        // Two tasks that both need to touch a file is a merge conflict the run
        // cannot resolve, but it is the lead's call to make and the plan gate
        // already refuses it at planning time. Here the amendment is applied and
        // the conflict is the lead's problem, not a silent drop.
        let mut run = run_with(vec![task("t1", &[]), task("t2", &[])]);
        let amendment =
            Amendment::parse(r#"{"files": {"t1": ["x.js"], "t2": ["x.js"]}}"#).unwrap();
        let changed = apply(&mut run, &amendment).unwrap();
        assert_eq!(changed.len(), 2);
    }

    /// The criterion is the lead's writing, so the lead is who rewrites it.
    ///
    /// The fifteenth live run's t1 was judged by `node --test test/gates.test.js`
    /// while owning that test file: the base side of the proof checks out the
    /// base commit, which carries the base version of the test, which passes. No
    /// worker could satisfy that, and the run died after three attempts.
    #[test]
    fn an_amendment_may_rewrite_the_criterion_of_an_unverified_task() {
        let mut run = run_with(vec![task("t1", &["src/logic/gates.js"])]);
        let amendment = Amendment::parse(
            r#"{"acceptance": {"t1": "node -e \"process.exit(1)\""}, "reason": "the old one passed on base"}"#,
        )
        .unwrap();
        let changed = apply(&mut run, &amendment).unwrap();
        assert_eq!(changed.len(), 1);
        assert!(changed[0].contains("t1 is now judged by"), "{changed:?}");
        assert_eq!(
            run.task("t1").unwrap().acceptance,
            "node -e \"process.exit(1)\""
        );
    }

    /// A verified task's criterion is settled: rewriting it would re-open work
    /// the run has already paid for and already proved.
    #[test]
    fn an_amendment_may_not_rewrite_a_verified_tasks_criterion() {
        let mut run = run_with(vec![task("t1", &["src/a.js"])]);
        run.task_mut("t1").unwrap().state = TaskState::Verified;
        let amendment = Amendment::parse(r#"{"acceptance": {"t1": "exit 0"}}"#).unwrap();
        let error = invalid(apply(&mut run, &amendment).unwrap_err());
        assert!(error.contains("already verified"), "{error}");
        assert_eq!(run.task("t1").unwrap().acceptance, "criterion");
    }

    /// A criterion rewritten to itself changes nothing, and that is the lead
    /// confirming the plan, not failing to amend it — a `NoOp`, not a refusal.
    #[test]
    fn an_amendment_that_rewrites_a_criterion_to_itself_is_a_no_op() {
        let mut run = run_with(vec![task("t1", &["src/a.js"])]);
        let amendment = Amendment::parse(r#"{"acceptance": {"t1": "criterion"}}"#).unwrap();
        assert_eq!(apply(&mut run, &amendment).unwrap_err(), AmendError::NoOp);
    }

    #[test]
    fn an_amendment_naming_an_unknown_task_is_refused_whole() {
        let mut run = run_with(vec![task("t1", &["src/a.js"])]);
        let amendment =
            Amendment::parse(r#"{"files": {"t1": ["x.js"], "t9": ["y.js"]}}"#).unwrap();
        let error = invalid(apply(&mut run, &amendment).unwrap_err());
        assert!(error.contains("t9"), "{error}");
        assert!(
            !run.task("t1").unwrap().files.contains(&"x.js".to_string()),
            "nothing was applied"
        );
    }

    #[test]
    fn an_amendment_may_not_touch_a_verified_task() {
        // A verified task's files are settled: its branch is merged or about to
        // be, and adding a file to it would mean re-running work the run has
        // already paid for and already proved.
        let mut run = run_with(vec![task("t1", &["src/a.js"])]);
        run.task_mut("t1").unwrap().state = TaskState::Verified;
        let amendment = Amendment::parse(r#"{"files": {"t1": ["x.js"]}}"#).unwrap();
        let error = invalid(apply(&mut run, &amendment).unwrap_err());
        assert!(error.contains("already verified"), "{error}");
    }

    /// An amendment that restates the plan changes nothing: a `NoOp`, which
    /// the caller treats as benign rather than as a refusal.
    #[test]
    fn an_amendment_that_changes_nothing_is_a_no_op() {
        let mut run = run_with(vec![task("t1", &["src/a.js"])]);
        let amendment = Amendment::parse(r#"{"files": {"t1": ["src/a.js"]}}"#).unwrap();
        assert_eq!(apply(&mut run, &amendment).unwrap_err(), AmendError::NoOp);
    }

    #[test]
    fn an_amendment_with_no_files_is_refused() {
        let error = Amendment::parse(r#"{"files": {}}"#).unwrap_err();
        assert!(error.contains("changes nothing"), "{error}");
    }

    /// The brief tells the lead to write this when the narrow fix is not enough.
    /// The fourteenth live run's lead did, and the parser refused it — so the
    /// run escalated with the refusal instead of the lead's reason, and the one
    /// thing the lead produced was thrown away.
    #[test]
    fn an_amendment_that_asks_the_user_is_an_answer_not_an_error() {
        let amendment = Amendment::parse(
            r#"{"files": {}, "reason": "t2 is correct but src/player.js has to change and t4 owns it"}"#,
        )
        .expect("the documented escape has to parse");
        assert!(amendment.is_ask());
        assert!(amendment.reason.contains("src/player.js"));
    }

    #[test]
    fn an_amendment_that_changes_something_is_not_an_ask() {
        let amendment = Amendment::parse(r#"{"files": {"t2": ["src/a.js"]}}"#).unwrap();
        assert!(!amendment.is_ask());
    }

    #[test]
    fn an_empty_amendment_with_no_reason_is_still_refused() {
        let error = Amendment::parse(r#"{"files": {}, "reason": "   "}"#).unwrap_err();
        assert!(
            error.contains("stop and ask the user"),
            "the refusal has to name the escape it wants: {error}"
        );
    }

    #[test]
    fn an_amendment_that_is_not_json_says_so() {
        let error = Amendment::parse("not json").unwrap_err();
        assert!(error.contains("not valid"), "{error}");
    }

    #[test]
    fn the_paths_are_deduplicated_and_sorted() {
        let amendment = Amendment::parse(
            r#"{"files": {"t1": ["b.js", "a.js"], "t2": ["a.js"]}}"#,
        )
        .unwrap();
        assert_eq!(amendment.paths(), vec!["a.js", "b.js"]);
    }
}
