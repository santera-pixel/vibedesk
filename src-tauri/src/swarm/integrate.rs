//! Integration: merging verified branches in dependency order.
//!
//! This is the part that is usually faked. A swarm that stops at "every task
//! passed its verifier" has not produced a working result — it has produced N
//! working fragments and an untested combination. The integration branch and the
//! integrated suite are what turn fragments into a result.
//!
//! The merge order is the topological order, not the completion order. Merging in
//! completion order would apply a dependent task's change before the change it
//! was written against, which is how a clean merge produces a broken tree.

use std::path::Path;

use super::plan::topological_order;
use super::worktree::{self, Git, MergeOutcome};
use super::{Conflict, IntegrationState, Run, TaskState};

/// The order to merge in: verified tasks, dependencies first.
///
/// A task that is not verified is not in the list. A task whose dependency
/// failed is not in the list either, because merging it would apply a change
/// written against a base that never landed.
pub fn merge_order(run: &Run) -> Vec<String> {
    let order = topological_order(&run.tasks).unwrap_or_default();
    let verified: std::collections::BTreeSet<&str> = run
        .tasks
        .iter()
        .filter(|t| t.state.is_verified())
        .map(|t| t.id.as_str())
        .collect();
    order
        .into_iter()
        .filter(|id| verified.contains(id.as_str()))
        .collect()
}

/// What happened when the engine tried to integrate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntegrationOutcome {
    /// Every verified branch merged.
    Merged { order: Vec<String> },
    /// One merge conflicted. The run pauses and the lead is addressed.
    Conflict {
        task: String,
        files: Vec<String>,
        merged: Vec<String>,
    },
    /// Git refused for a reason that is not a conflict.
    Error(String),
}

/// Merge every verified branch into the integration branch.
///
/// Stops at the first conflict rather than continuing: a later merge on top of a
/// conflicted tree produces a result nobody can attribute, and attribution is the
/// only thing that makes a conflict actionable.
pub fn integrate(git: &dyn Git, repo: &Path, run: &mut Run) -> IntegrationOutcome {
    let order = merge_order(run);
    if order.is_empty() {
        return IntegrationOutcome::Error("no verified task to merge".into());
    }

    run.integration.state = IntegrationState::Merging;
    run.integration.merged.clear();
    run.integration.conflict = None;

    for task_id in &order {
        let Some(task) = run.task(task_id) else {
            continue;
        };
        let Some(branch) = task.branch.clone() else {
            // A verified task with no branch is a bookkeeping bug, not a merge
            // failure. Say so rather than silently skipping it.
            return IntegrationOutcome::Error(format!(
                "{task_id} is verified but has no branch"
            ));
        };

        // Merge the commit the verdict was earned on, not the branch tip. The
        // two differ only when the branch moved after the verdict, and that case
        // is already refused when the verdict arrives — this is the second
        // boundary, because integration is where a wrong tree becomes the
        // result. A task adopted from disk before the field existed has `None`,
        // so the branch remains the fallback rather than a hard failure.
        let target = task
            .verified_commit
            .clone()
            .unwrap_or_else(|| branch.clone());

        // Authorship: every commit in the judged range has to belong to the
        // task's worker. Without this the engine cannot tell a verifier's commit
        // from its worker's, which is exactly how the audited run's `t2` branch
        // ended at a commit the verifier made.
        if let (Some(owner), Some(base)) = (task.owner.clone(), task.base_commit.clone()) {
            let expected = worktree::identity(&owner).1;
            match worktree::authors(git, repo, &base, &target) {
                Ok(authors) => {
                    if let Some(foreign) = authors.iter().find(|author| **author != expected) {
                        run.integration.state = IntegrationState::Failed;
                        return IntegrationOutcome::Error(format!(
                            "{task_id}: a commit in {base}..{target} is authored by {foreign}, not {owner} ({expected}) — the verdict was earned on that tree, and this commit is not the worker's, so the engine will not integrate it"
                        ));
                    }
                }
                Err(error) => {
                    run.integration.state = IntegrationState::Failed;
                    return IntegrationOutcome::Error(format!(
                        "{task_id}: could not read the authorship of {base}..{target}: {error}"
                    ));
                }
            }
        }

        match worktree::merge(git, repo, &target) {
            Ok(MergeOutcome::Clean) => {
                run.integration.merged.push(task_id.clone());
            }
            Ok(MergeOutcome::Conflict { files }) => {
                // Leave the tree usable: an aborted merge is a clean tree, and
                // the user needs one to look at the conflict.
                let _ = worktree::abort_merge(git, repo);
                run.integration.state = IntegrationState::Conflict;
                run.integration.conflict = Some(Conflict {
                    task: task_id.clone(),
                    files: files.clone(),
                    detail: format!(
                        "merging {target} conflicted on {}",
                        files.join(", ")
                    ),
                });
                if let Some(task) = run.task_mut(task_id) {
                    task.state = TaskState::Conflict;
                }
                return IntegrationOutcome::Conflict {
                    task: task_id.clone(),
                    files,
                    merged: run.integration.merged.clone(),
                };
            }
            Err(error) => {
                run.integration.state = IntegrationState::Failed;
                return IntegrationOutcome::Error(error);
            }
        }
    }

    run.integration.state = IntegrationState::Merged;
    IntegrationOutcome::Merged { order }
}

/// Record the integrated suite's outcome.
///
/// Separate from the merge because the suite runs after every merge, and because
/// a failing suite is the failure mode a swarm is most likely to produce: every
/// task green, the combination red.
pub fn record_suite(run: &mut Run, command: &str, passed: bool) {
    run.integration.suite = Some(command.to_string());
    run.integration.suite_passed = Some(passed);
    if !passed {
        run.integration.state = IntegrationState::Failed;
    }
}

/// The task a failing integrated suite should be attributed to.
///
/// The honest answer is usually "the one whose files the failure names". This
/// looks for a task that owns a file mentioned in the output, preferring the
/// latest in merge order — the change most recently applied is the most likely
/// cause. Returns `None` rather than guessing when nothing matches, because a
/// wrong attribution is worse than an unattributed failure.
pub fn attribute_failure(run: &Run, output: &str) -> Option<String> {
    let order = merge_order(run);
    let mut best: Option<(usize, String)> = None;
    for (rank, task_id) in order.iter().enumerate() {
        let Some(task) = run.task(task_id) else {
            continue;
        };
        let hit = task
            .files
            .iter()
            .any(|file| output.contains(file.as_str()));
        if hit {
            // Later in merge order wins.
            if best.as_ref().map(|(r, _)| rank >= *r).unwrap_or(true) {
                best = Some((rank, task_id.clone()));
            }
        }
    }
    best.map(|(_, id)| id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swarm::worktree::Git;
    use crate::swarm::{Member, Role, Task};
    use std::path::PathBuf;
    use std::sync::Mutex;

    struct FakeGit {
        calls: Mutex<Vec<String>>,
        responses: Mutex<Vec<(String, Result<String, String>)>>,
    }

    impl FakeGit {
        fn new() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                responses: Mutex::new(Vec::new()),
            }
        }
        fn on(&self, prefix: &str, result: Result<&str, &str>) -> &Self {
            self.responses.lock().unwrap().push((
                prefix.to_string(),
                result.map(str::to_string).map_err(str::to_string),
            ));
            self
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl Git for FakeGit {
        fn run(&self, args: &[&str], _cwd: &Path) -> Result<String, String> {
            let joined = args.join(" ");
            self.calls.lock().unwrap().push(joined.clone());
            let mut responses = self.responses.lock().unwrap();
            if let Some(index) = responses
                .iter()
                .position(|(prefix, _)| joined.starts_with(prefix.as_str()))
            {
                return responses.remove(index).1;
            }
            Ok(String::new())
        }
    }

    fn run_with(tasks: Vec<Task>) -> Run {
        let mut run = Run::new("g", "/tmp/repo", Member::new("lead", Role::Lead, "m"));
        run.tasks = tasks;
        run
    }

    fn verified(id: &str, deps: &[&str], files: &[&str]) -> Task {
        let mut task = Task::new(id, id, "d", "c").with_files(files).with_deps(deps);
        task.state = TaskState::Verified;
        task.branch = Some(format!("swarm/run-1/{id}"));
        task
    }

    #[test]
    fn the_merge_order_is_topological_not_completion_order() {
        // t2 was verified first, but t1 must merge first.
        let run = run_with(vec![
            verified("t2", &["t1"], &["b"]),
            verified("t1", &[], &["a"]),
        ]);
        assert_eq!(merge_order(&run), vec!["t1", "t2"]);
    }

    #[test]
    fn an_unverified_task_is_not_merged() {
        let mut t2 = Task::new("t2", "t2", "d", "c").with_files(&["b"]);
        t2.state = TaskState::Running;
        let run = run_with(vec![verified("t1", &[], &["a"]), t2]);
        assert_eq!(merge_order(&run), vec!["t1"]);
    }

    #[test]
    fn a_clean_integration_merges_every_verified_branch_in_order() {
        let git = FakeGit::new();
        git.on("merge --no-ff", Ok("ok"));
        let mut run = run_with(vec![
            verified("t1", &[], &["a"]),
            verified("t2", &["t1"], &["b"]),
        ]);
        let outcome = integrate(&git, &PathBuf::from("/tmp/repo"), &mut run);
        assert_eq!(
            outcome,
            IntegrationOutcome::Merged {
                order: vec!["t1".into(), "t2".into()]
            }
        );
        assert_eq!(run.integration.state, IntegrationState::Merged);
        assert_eq!(run.integration.merged, vec!["t1", "t2"]);

        let merges: Vec<String> = git
            .calls()
            .into_iter()
            .filter(|c| c.starts_with("merge --no-ff"))
            .collect();
        assert_eq!(merges.len(), 2);
        assert!(merges[0].contains("swarm/run-1/t1"), "{}", merges[0]);
        assert!(merges[1].contains("swarm/run-1/t2"), "{}", merges[1]);
    }

    #[test]
    fn a_commit_the_worker_did_not_author_is_refused() {
        // The audited run's t2 branch ended at `e18875b`, a commit its verifier
        // made. A path-based check cannot see that; authorship can.
        let git = FakeGit::new();
        git.on("merge --no-ff", Ok("ok"));
        git.on("log --no-merges --format=%ae", Ok("v-t2@swarm.local\n"));
        let mut t1 = verified("t1", &[], &["a"]);
        t1.owner = Some("w-t1".into());
        t1.base_commit = Some("base".into());
        t1.verified_commit = Some("e18875b".into());
        let mut run = run_with(vec![t1]);
        let outcome = integrate(&git, &PathBuf::from("/tmp/repo"), &mut run);
        let why = match outcome {
            IntegrationOutcome::Error(why) => why,
            _ => panic!("a foreign author must not integrate"),
        };
        assert!(why.contains("v-t2@swarm.local"), "{why}");
        assert_eq!(run.integration.state, IntegrationState::Failed);
    }

    #[test]
    fn the_workers_own_commits_integrate() {
        // The rule must not refuse the work it exists to protect.
        let git = FakeGit::new();
        git.on("merge --no-ff", Ok("ok"));
        git.on(
            "log --no-merges --format=%ae",
            Ok("w-t1@swarm.local\nw-t1@swarm.local\n"),
        );
        let mut t1 = verified("t1", &[], &["a"]);
        t1.owner = Some("w-t1".into());
        t1.base_commit = Some("base".into());
        t1.verified_commit = Some("abc123".into());
        let mut run = run_with(vec![t1]);
        let outcome = integrate(&git, &PathBuf::from("/tmp/repo"), &mut run);
        assert!(matches!(outcome, IntegrationOutcome::Merged { .. }));
    }

    #[test]
    fn integration_merges_the_commit_the_verdict_was_earned_on() {
        // The branch can move after the verdict; the tree that was judged is the
        // one that gets integrated. Without the pin, this merges the tip.
        let git = FakeGit::new();
        git.on("merge --no-ff", Ok("ok"));
        let mut t1 = verified("t1", &[], &["a"]);
        t1.verified_commit = Some("e18875b".into());
        let mut run = run_with(vec![t1]);
        let outcome = integrate(&git, &PathBuf::from("/tmp/repo"), &mut run);
        assert!(matches!(outcome, IntegrationOutcome::Merged { .. }));

        let merges: Vec<String> = git
            .calls()
            .into_iter()
            .filter(|c| c.starts_with("merge --no-ff"))
            .collect();
        assert_eq!(merges.len(), 1);
        assert!(merges[0].contains("e18875b"), "{}", merges[0]);
        assert!(
            !merges[0].contains("swarm/run-1/t1"),
            "the branch tip must not be what gets merged: {}",
            merges[0]
        );
    }

    #[test]
    fn a_conflict_stops_the_run_and_names_the_task_and_files() {
        let git = FakeGit::new();
        git.on("merge --no-ff", Err("CONFLICT"));
        git.on("diff --name-only --diff-filter=U", Ok("src/a.rs\n"));
        git.on("merge --abort", Ok(""));
        let mut run = run_with(vec![
            verified("t1", &[], &["a"]),
            verified("t2", &["t1"], &["b"]),
        ]);
        let outcome = integrate(&git, &PathBuf::from("/tmp/repo"), &mut run);
        assert_eq!(
            outcome,
            IntegrationOutcome::Conflict {
                task: "t1".into(),
                files: vec!["src/a.rs".into()],
                merged: vec![],
            }
        );
        assert_eq!(run.integration.state, IntegrationState::Conflict);
        assert_eq!(run.task("t1").unwrap().state, TaskState::Conflict);
        assert!(run.integration.conflict.is_some());
        // The tree is left usable.
        assert!(git.calls().iter().any(|c| c.starts_with("merge --abort")));
    }

    #[test]
    fn a_conflict_after_a_clean_merge_keeps_the_clean_one() {
        let git = FakeGit::new();
        git.on("merge --no-ff", Ok("ok"));
        git.on("merge --no-ff", Err("CONFLICT"));
        git.on("diff --name-only --diff-filter=U", Ok("src/b.rs\n"));
        git.on("merge --abort", Ok(""));
        let mut run = run_with(vec![
            verified("t1", &[], &["a"]),
            verified("t2", &["t1"], &["b"]),
        ]);
        let outcome = integrate(&git, &PathBuf::from("/tmp/repo"), &mut run);
        match outcome {
            IntegrationOutcome::Conflict { merged, .. } => assert_eq!(merged, vec!["t1"]),
            other => panic!("expected a conflict, got {other:?}"),
        }
    }

    #[test]
    fn a_verified_task_with_no_branch_is_an_error_not_a_silent_skip() {
        let git = FakeGit::new();
        let mut task = Task::new("t1", "t1", "d", "c").with_files(&["a"]);
        task.state = TaskState::Verified;
        let mut run = run_with(vec![task]);
        let outcome = integrate(&git, &PathBuf::from("/tmp/repo"), &mut run);
        assert!(matches!(outcome, IntegrationOutcome::Error(_)));
    }

    #[test]
    fn integrating_nothing_is_an_error() {
        let git = FakeGit::new();
        let mut run = run_with(vec![]);
        let outcome = integrate(&git, &PathBuf::from("/tmp/repo"), &mut run);
        assert!(matches!(outcome, IntegrationOutcome::Error(_)));
    }

    #[test]
    fn a_failing_suite_marks_the_integration_failed() {
        let mut run = run_with(vec![verified("t1", &[], &["a"])]);
        record_suite(&mut run, "cargo test", false);
        assert_eq!(run.integration.state, IntegrationState::Failed);
        assert_eq!(run.integration.suite_passed, Some(false));
        assert_eq!(run.integration.suite.as_deref(), Some("cargo test"));
    }

    #[test]
    fn a_passing_suite_leaves_the_integration_merged() {
        let mut run = run_with(vec![verified("t1", &[], &["a"])]);
        run.integration.state = IntegrationState::Merged;
        record_suite(&mut run, "cargo test", true);
        assert_eq!(run.integration.state, IntegrationState::Merged);
        assert_eq!(run.integration.suite_passed, Some(true));
    }

    #[test]
    fn a_failure_is_attributed_to_the_task_whose_file_it_names() {
        let run = run_with(vec![
            verified("t1", &[], &["src/rate.rs"]),
            verified("t2", &["t1"], &["src/middleware.rs"]),
        ]);
        let output = "error[E0308]: mismatched types\n  --> src/middleware.rs:42:5";
        assert_eq!(attribute_failure(&run, output), Some("t2".into()));
    }

    #[test]
    fn an_unattributable_failure_returns_none_rather_than_guessing() {
        let run = run_with(vec![verified("t1", &[], &["src/rate.rs"])]);
        assert_eq!(attribute_failure(&run, "error: linker failed"), None);
    }

    #[test]
    fn the_latest_merge_wins_when_two_tasks_share_a_file() {
        // t2 depends on t1 and both name src/a.rs, which the overlap rule allows
        // precisely because they are ordered.
        let run = run_with(vec![
            verified("t1", &[], &["src/a.rs"]),
            verified("t2", &["t1"], &["src/a.rs"]),
        ]);
        let output = "error in src/a.rs";
        assert_eq!(attribute_failure(&run, output), Some("t2".into()));
    }
}
