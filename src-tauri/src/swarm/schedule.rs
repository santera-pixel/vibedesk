//! The scheduler: a ready-queue, not waves.
//!
//! A wave scheduler waits for every task in a batch before starting the next
//! batch, so one slow task stalls the whole run. This one starts a task the
//! moment its dependencies have committed, and verification overlaps
//! implementation — a task being verified does not hold a worker slot.
//!
//! "Committed", not "verified". A worker posts its evidence and stops, so the
//! commit exists before the verdict does, and waiting for the verdict put
//! verification between every link of a chain. The cost of starting early is that
//! a refused verdict can invalidate work that was already under way, which is what
//! [`invalidate_dependents`] is for.
//!
//! Everything here is pure and deterministic. Given the same task states and the
//! same concurrency it produces the same transitions, which is what makes both
//! the rehearsal and the tests mean anything.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use super::{Role, Task, TaskState, Verdict};

/// Something the engine must do next.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Transition {
    /// Dependencies are satisfied; the task is now a candidate for a slot.
    Unblock { task: String },
    /// Give this task to a worker.
    Start { task: String },
    /// Hand this task to a verifier.
    Verify { task: String },
    /// The task passed and may be merged.
    Complete { task: String },
    /// The task failed for good.
    Fail { task: String, reason: String },
    /// The merge conflicted; the run pauses.
    Conflict { task: String },
}

impl Transition {
    pub fn task(&self) -> &str {
        match self {
            Transition::Unblock { task }
            | Transition::Start { task }
            | Transition::Verify { task }
            | Transition::Complete { task }
            | Transition::Fail { task, .. }
            | Transition::Conflict { task } => task,
        }
    }
}

/// How many tasks hold a worker slot.
pub fn running_count(tasks: &[Task]) -> usize {
    tasks.iter().filter(|t| t.state.holds_worker()).count()
}

/// How many tasks hold a verifier slot.
pub fn verifying_count(tasks: &[Task]) -> usize {
    tasks.iter().filter(|t| t.state.holds_verifier()).count()
}

/// How many tasks hold a live member process: a worker slot or a verifier slot.
///
/// `concurrency` bounds the first of these, not this — a `Verifying` task holds
/// a live verifier process and does not count against the ceiling, which is why
/// the panel shows this number beside the ceiling rather than inferring it.
pub fn live_count(tasks: &[Task]) -> usize {
    running_count(tasks) + verifying_count(tasks)
}

/// How many tasks are waiting for a free worker.
pub fn ready_count(tasks: &[Task]) -> usize {
    tasks.iter().filter(|t| t.state == TaskState::Ready).count()
}

/// Whether every dependency of a task has committed its work.
///
/// `Verifying` counts as well as `Verified`. A worker posts its evidence and
/// stops, so the commit exists from that moment; the verdict is a judgement about
/// work that is already on a branch. Requiring the verdict put verification
/// between every link of a chain — t1 work, t1 verify, t2 work, t2 verify — and
/// the observed five-link run spent roughly 95 minutes on 30 minutes of work.
///
/// The trade is deliberate and it is not free: a dependent that starts here may
/// have to be thrown away when the verdict lands. [`invalidate_dependents`] is
/// the other half of it.
fn deps_satisfied(task: &Task, by_id: &BTreeMap<&str, &Task>) -> bool {
    task.deps.iter().all(|dep| {
        by_id
            .get(dep.as_str())
            .map(|t| t.state.is_committed())
            .unwrap_or(false)
    })
}

/// Roll back every task that was built on a task whose verdict failed.
///
/// Transitive, and that is not a detail. t3 built on t2 built on t1: when t1's
/// verdict fails, t2's branch contains work that has just been refused, so t3's
/// branch contains it too. Invalidating one level would leave t3 `Verified`
/// against a base that no longer exists, and the run would integrate it.
///
/// A `Verified` dependent is invalidated like any other. Its verdict was earned
/// against a base that has now failed, which makes the verdict a statement about
/// a tree that is gone.
///
/// A `Ready` dependent is invalidated too, and it has no `built_on` to be found
/// by — it was unblocked while the dependency was `Verifying` and is waiting for
/// a worker slot. `advance` starts a `Ready` task without re-checking its
/// dependencies, so leaving it there would start it on the refused commit one
/// tick later. It has no branch and no attempt to lose, so the rollback is free.
///
/// `attempts` is refunded, because a dependency's failure is not the dependent's
/// attempt. The dependent did the work it was given and the work was thrown away
/// for a reason it cannot act on; charging it would spend its retry budget on
/// somebody else's verdict, and one flaky dependency would exhaust every
/// dependent's ceiling without any of them failing on its own merits.
///
/// The refund is one attempt, and only when the discarded attempt was not the
/// task's own failure. A `Ready` task carrying a failed verdict already spent
/// that attempt on work its own verifier refused, and that one stays spent: a
/// task that cannot pass its verifier must not be rescued indefinitely by a
/// dependency that keeps retrying.
///
/// Returns the ids invalidated, in the order they were found, so the caller can
/// discard the branches and stop the members. The state changes here are the ones
/// that need no filesystem: the caller owns the git.
pub fn invalidate_dependents(tasks: &mut [Task], failed: &str) -> Vec<String> {
    let mut invalidated: Vec<String> = Vec::new();
    // A worklist rather than a recursion, because the graph is a DAG and a
    // recursive walk over a deep plan is a stack the plan's size decides.
    let mut frontier: Vec<String> = vec![failed.to_string()];
    while let Some(cause) = frontier.pop() {
        let built_on_it: Vec<String> = tasks
            .iter()
            .filter(|t| {
                // A task that already failed on its own merits stays failed. It
                // has spent its attempts, and putting it back in the queue would
                // spend a worker slot and a verifier turn on a task whose next
                // verdict fails it again — and would hide the failure from
                // `is_stuck`, which is what ends the run with a reason.
                //
                // A conflicted task is waiting on the lead, and its branch was
                // already discarded when the merge failed.
                if matches!(t.state, TaskState::Failed | TaskState::Conflict) {
                    return false;
                }
                t.built_on.contains_key(&cause)
                    || (t.state == TaskState::Ready && t.deps.iter().any(|d| d == &cause))
            })
            .map(|t| t.id.clone())
            .collect();
        for id in built_on_it {
            if invalidated.contains(&id) {
                continue;
            }
            if let Some(task) = tasks.iter_mut().find(|t| t.id == id) {
                // Whether the attempt being discarded ended in this task's own
                // failure. A `Ready` task with a failed verdict already spent
                // that attempt on work its own verifier refused. Every other
                // state — `Running`, `Verifying`, `Verified` — holds an attempt
                // that was discarded because the base was refused, not because
                // the task failed, so the attempt goes back.
                //
                // Read before the clears below, which drop the verdict.
                let own_failure = task.state == TaskState::Ready
                    && task.verdict.as_ref().is_some_and(|v| !v.is_earned());
                if !own_failure {
                    task.attempts = task.attempts.saturating_sub(1);
                }
                task.state = TaskState::Blocked;
                task.built_on.clear();
                // The branch is about to be deleted, so the record of it goes
                // too. `prepare_worker` regenerates the same name from the run
                // and task ids, so nothing is lost by clearing it.
                task.branch = None;
                // The base the task was proven against was the dependency's
                // tree. It is stale by definition now, and a stale base is worse
                // than none: the re-queued task would prove its criterion against
                // the work that was just refused.
                task.base_commit = None;
                // The verdict, if it had one, was earned against that base.
                task.verdict = None;
                task.owner = None;
                task.verifier = None;
                task.started_at = None;
                task.ended_at = None;
                task.out_of_grant.clear();
                task.reconcile.clear();
                // The same repair `amend::apply` makes when it puts a conflicted
                // task back in the queue: the reason it could not start is about
                // a tree that no longer exists. `conflict_repairs` is not
                // refunded — the budget is spent, not returned.
                task.conflict = None;
            }
            invalidated.push(id.clone());
            frontier.push(id);
        }
    }
    invalidated
}

/// Move the run forward by one scheduling step.
///
/// Two passes, in this order: unblock everything that can be unblocked, then
/// fill the free worker slots from the ready queue in declaration order. Doing
/// it in one pass would start a task in the same step that unblocked it, which
/// makes the transition list lie about what happened.
pub fn advance(tasks: &mut [Task], concurrency: usize) -> Vec<Transition> {
    let mut transitions = Vec::new();

    // Pass 1 — unblock.
    {
        let by_id: BTreeMap<&str, &Task> = tasks.iter().map(|t| (t.id.as_str(), t)).collect();
        let unblockable: Vec<String> = tasks
            .iter()
            .filter(|t| t.state == TaskState::Blocked && deps_satisfied(t, &by_id))
            .map(|t| t.id.clone())
            .collect();
        for id in unblockable {
            if let Some(task) = tasks.iter_mut().find(|t| t.id == id) {
                task.state = TaskState::Ready;
                transitions.push(Transition::Unblock { task: id });
            }
        }
    }

    // Pass 2 — fill the slots.
    let busy = running_count(tasks);
    let free = concurrency.saturating_sub(busy);
    if free > 0 {
        let startable: Vec<String> = tasks
            .iter()
            .filter(|t| t.state == TaskState::Ready)
            .take(free)
            .map(|t| t.id.clone())
            .collect();
        for id in startable {
            if let Some(task) = tasks.iter_mut().find(|t| t.id == id) {
                task.state = TaskState::Running;
                task.attempts += 1;
                task.started_at = Some(super::now());
                transitions.push(Transition::Start { task: id });
            }
        }
    }

    transitions
}

/// Read a verifier's ruling from the first word of its verdict.
///
/// Not `text.contains("pass")`. The first live run had a verifier write
/// "FAILED. The mechanical criterion passes on the change and fails on base, so
/// it is a real test — but it is too weak to catch X", and the substring test
/// read that as a pass: the engine marked a task verified on a verdict that said
/// FAILED. A ruling is the first word, because that is what the brief asks for
/// ("passed or failed, and why"), and a parser that looks anywhere in the text
/// will find the word it wants in a sentence that says the opposite.
///
/// `None` means the verdict did not start with a ruling. The caller treats that
/// as a failure rather than a pass: a malformed verdict must never be the reason
/// a task is accepted.
pub fn ruling(text: &str) -> Option<bool> {
    let first = text
        .trim_start()
        // Markdown and quoting are how a model decorates a heading.
        .trim_start_matches(|c: char| {
            c.is_whitespace() || matches!(c, '#' | '*' | '_' | '>' | '-' | '"' | '\'' | '`')
        })
        .split(|c: char| c.is_whitespace() || matches!(c, ':' | ',' | '.' | '!' | ';'))
        .next()?
        .to_ascii_lowercase();

    if first.starts_with("pass") || first.starts_with("approv") {
        Some(true)
    } else if first.starts_with("fail") || first.starts_with("reject") {
        Some(false)
    } else {
        None
    }
}

/// Apply a verifier's ruling to a task.
///
/// A verdict that is not *earned* — passing, failing-first, and with a clean
/// base — is a failure even when `passed` is true. That is the whole point of
/// the adversarial role: an approving read is not a test.
pub fn apply_verdict(task: &mut Task, verdict: Verdict, max_attempts: u32) -> Transition {
    let earned = verdict.is_earned();
    let reason = verdict.reason.clone();
    task.verdict = Some(verdict);
    task.ended_at = Some(super::now());

    if earned {
        task.state = TaskState::Verified;
        return Transition::Complete {
            task: task.id.clone(),
        };
    }

    if task.attempts >= max_attempts {
        task.state = TaskState::Failed;
        return Transition::Fail {
            task: task.id.clone(),
            reason,
        };
    }

    // Retry: back to the queue, keeping the attempt count so the ceiling holds.
    task.state = TaskState::Ready;
    Transition::Fail {
        task: task.id.clone(),
        reason,
    }
}

/// Whether the run can make no further progress without a human.
///
/// A valid DAG cannot deadlock on dependencies, but a *failed* task can: its
/// dependents stay blocked forever. This is the check that turns that into a
/// named state instead of a spinner.
///
/// A task returning from `Running` to `Blocked` is not a dead run. That is what
/// [`invalidate_dependents`] does, and it happens while the dependency it was
/// built on is retrying — so the dependency is `Ready` or `Verifying` and the
/// `moving` check below already answers. The only way an invalidated task is the
/// last word is when its dependency failed for good, and then the run really is
/// stuck: nothing will ever verify that dependency again.
pub fn is_stuck(tasks: &[Task]) -> bool {
    if tasks.is_empty() {
        return false;
    }
    if tasks.iter().all(|t| t.state.is_verified()) {
        return false;
    }
    let moving = tasks.iter().any(|t| {
        matches!(
            t.state,
            TaskState::Running | TaskState::Verifying | TaskState::Ready
        )
    });
    if moving {
        return false;
    }
    // Nothing is moving. If a task failed or conflicted, the run is stuck on it.
    tasks
        .iter()
        .any(|t| matches!(t.state, TaskState::Failed | TaskState::Conflict))
}

/* ------------------------------------------------------------------ *
 * The rehearsal
 * ------------------------------------------------------------------ */

/// One batch of tasks that could run together.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Wave {
    pub index: usize,
    pub tasks: Vec<String>,
}

/// What a plan would cost, without spending anything.
///
/// This is the feature that makes the swarm honest before it is expensive: the
/// user sees the shape, the wall-clock floor and the token estimate, and decides
/// whether 15× is worth it for *this* plan. It is also what makes the panel
/// demonstrable without a live agent.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Rehearsal {
    pub waves: Vec<Wave>,
    /// The longest chain of dependent tasks. No concurrency beats this.
    pub critical_path: Vec<String>,
    /// The most tasks that could ever run at once.
    pub max_parallel: usize,
    /// Wall-clock in task-units when run serially.
    pub serial_steps: usize,
    /// Wall-clock in task-units at the given concurrency.
    pub parallel_steps: usize,
    /// `serial_steps / parallel_steps`. Below 1.5 the swarm is not worth it.
    pub speedup: f64,
    /// The plan's own wall-clock projection, in seconds.
    ///
    /// Shown beside the budget so a plan that cannot finish says so before it
    /// starts: a projection longer than the budget is a run that will be failed
    /// mid-task with every worktree discarded.
    pub projected_wall_secs: u64,
    /// The wall clock a run of this plan is given. See [`budget_wall_secs`].
    pub budget_wall_secs: u64,
    /// The seconds one step was costed at, and whether that number was measured.
    ///
    /// `measured` is false when no run of this workspace has finished a task yet,
    /// and the figure is [`DEFAULT_MEDIAN_TASK_SECS`]. The review says which it
    /// is: a projection built on a guess and a projection built on this machine's
    /// own history are different claims, and the user is deciding whether to
    /// spend a run on the second one.
    pub median_task_secs: u64,
    pub measured: bool,
    /// How many past runs of this workspace the token figures are the median of,
    /// and `None` when they are the constants.
    ///
    /// A projection built on this machine's own history and one built on a guess
    /// are different claims, and the user is deciding whether to spend a run on
    /// the second one.
    pub tokens_measured_from: Option<usize>,
    /// A rough token figure. An estimate, labelled as one.
    pub estimated_tokens: u64,
    /// The same plan run as a single agent, for comparison.
    pub single_agent_tokens: u64,
}

/// Per-turn token estimates for a workspace with no history.
///
/// These are not measurements. They are the shape of the cost, and they were the
/// only figures the rehearsal had — 60,000 for a worker turn, when the audited
/// run's w-t2 spent 39,151,829 input tokens on one task: a factor of 652 out, and
/// the scheduler planned eighteen tasks against the smaller number. They survive
/// only for the first run in a workspace, which has nothing to measure. See
/// [`Estimates::of`].
pub const ESTIMATE_WORKER_TOKENS: u64 = 60_000;
pub const ESTIMATE_VERIFIER_TOKENS: u64 = 25_000;
pub const ESTIMATE_LEAD_TOKENS: u64 = 40_000;
pub const ESTIMATE_REFLECTION_TOKENS: u64 = 15_000;

/// What one turn of each role has cost here, or the constants when nothing has
/// been measured yet.
///
/// The audit's fourth fault in one sentence: the rehearsal promised a run of
/// eighteen tasks on four workers would cost about 45 minutes, and the run
/// produced one verified artifact in thirty. A cost model that never reads its
/// own history is not an estimate, it is a slogan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Estimates {
    pub worker: u64,
    pub verifier: u64,
    pub lead: u64,
    pub reflection: u64,
    /// How many past runs the figures are the median of, and `None` when they are
    /// the constants.
    pub runs: Option<usize>,
}

impl Default for Estimates {
    fn default() -> Self {
        Self {
            worker: ESTIMATE_WORKER_TOKENS,
            verifier: ESTIMATE_VERIFIER_TOKENS,
            lead: ESTIMATE_LEAD_TOKENS,
            reflection: ESTIMATE_REFLECTION_TOKENS,
            runs: None,
        }
    }
}

impl Estimates {
    /// The measured medians where this workspace has them, the constants where it
    /// does not.
    ///
    /// Per role, not per run: the lead, a worker and a verifier spend very
    /// different amounts, and `history::role_medians` only reports a role it has
    /// at least two runs for — one observation is not a median. A role without a
    /// measurement keeps its constant and the rehearsal says the figure is not
    /// measured, because a mixed estimate presented as a measurement is the same
    /// mistake as a guess presented as one.
    pub fn of(history: Option<&super::history::History>) -> Self {
        let Some(history) = history else {
            return Self::default();
        };
        let mut out = Self::default();
        let mut observed: Option<usize> = None;
        for median in history.role_medians() {
            match median.role {
                Role::Worker => out.worker = median.turn_tokens,
                Role::Verifier => out.verifier = median.turn_tokens,
                Role::Lead => out.lead = median.turn_tokens,
                Role::Reflector => out.reflection = median.turn_tokens,
            }
            // The worker figure is the one that dominates the estimate, so it is
            // the one that decides whether the total is labelled measured.
            if median.role == Role::Worker {
                observed = Some(median.runs);
            }
        }
        out.runs = observed;
        out
    }
}

/// How long one step of a plan is assumed to take, in seconds, when nothing has
/// been measured.
///
/// The fallback, not the number. Phase 13 derived the budget from this constant;
/// Phase 15 replaced it with the median of what tasks actually took in this
/// workspace — see [`super::history::History::median_task_secs`] — and the
/// constant survives only because a first run has no history to measure, and a
/// budget of zero is worse than a budget of a guess.
///
/// Ten minutes is the observed cost of a worker turn plus its verifier on this
/// machine, and it is deliberately generous: a budget that is too short fails a
/// run mid-task and discards every worktree, which is the failure this whole
/// derivation exists to prevent. A budget that is too long costs nothing but the
/// option to wait. It is also, on the evidence, more than half too low: the
/// observed run's worker turn was 6 minutes and its verifier's 20.7, so a real
/// task-pair is about 27 minutes. That is the gap the measured median closes.
pub const DEFAULT_MEDIAN_TASK_SECS: u64 = 600;

/// The floor under a run's wall clock, in seconds.
///
/// One hour, which is what the budget used to be unconditionally. A short plan
/// should not get a shorter clock than it always had: the derivation exists to
/// stop a long plan being failed by a fixed number, not to make small runs
/// tighter.
pub const MIN_WALL_SECS: u64 = 60 * 60;

/// How long the plan's critical path is projected to take, in seconds.
///
/// `parallel_steps` is the rehearsal's wall clock in task-units, so this is the
/// plan's own estimate of itself. It is a projection, not a promise: it assumes
/// every step takes `median_task_secs` and nothing retries.
pub fn projected_wall_secs(parallel_steps: usize, median_task_secs: u64) -> u64 {
    (parallel_steps as u64).saturating_mul(median_task_secs)
}

/// The wall clock a run of this plan is given, in seconds.
///
/// `max(3600, parallel_steps × median_task_secs × 2)`. The factor of two is the
/// retry allowance: a task that fails once and passes on its second attempt
/// doubles its step, and a budget that only covered the happy path would fail
/// exactly the runs that were recovering. The observed run was a five-deep chain
/// given a flat hour, so its own projection was longer than its budget and the
/// engine would have failed it mid-task with every worktree discarded.
///
/// `median_task_secs` is measured from the past runs of the workspace rather than
/// assumed, so the budget stops being a guess. It is a parameter rather than a
/// read because this is a pure function of two integers and the caller is the one
/// that knows which workspace it is costing.
pub fn budget_wall_secs(parallel_steps: usize, median_task_secs: u64) -> u64 {
    projected_wall_secs(parallel_steps, median_task_secs)
        .saturating_mul(2)
        .max(MIN_WALL_SECS)
}

/// Simulate a plan at a given concurrency.
///
/// The simulation is the scheduler itself, run against copies of the task
/// states, so the rehearsal cannot disagree with what the engine will do.
///
/// `measured` is the median seconds one task took in this workspace, from
/// [`super::history::History::median_task_secs`]. `None` means nothing has been
/// measured here yet, and the projection falls back to
/// [`DEFAULT_MEDIAN_TASK_SECS`] and says so on the result. It is an `Option`
/// rather than a plain number so the fallback lives in one place: a caller that
/// passed the constant itself would be a second place the default is decided, and
/// the review could not tell a measured 600 from an assumed one.
pub fn rehearse(
    tasks: &[Task],
    concurrency: usize,
    history: Option<&super::history::History>,
) -> Rehearsal {
    let concurrency = concurrency.max(1);
    let measured = history.and_then(|h| h.median_task_secs());
    let median_task_secs = measured.unwrap_or(DEFAULT_MEDIAN_TASK_SECS);
    let estimates = Estimates::of(history);
    let mut states: Vec<Task> = tasks.to_vec();
    for task in &mut states {
        task.state = TaskState::Blocked;
    }

    let mut waves: Vec<Wave> = Vec::new();
    let mut guard = 0usize;
    let ceiling = tasks.len().saturating_mul(4).max(8);

    loop {
        guard += 1;
        if guard > ceiling {
            break;
        }
        let transitions = advance(&mut states, concurrency);
        let started: Vec<String> = transitions
            .iter()
            .filter_map(|t| match t {
                Transition::Start { task } => Some(task.clone()),
                _ => None,
            })
            .collect();

        if started.is_empty() {
            // Nothing started, so nothing can start: either every task is done
            // or the plan is stuck. Either way the rehearsal is over.
            break;
        }

        waves.push(Wave {
            index: waves.len(),
            tasks: started.clone(),
        });

        // In the rehearsal a started task completes immediately: the point is
        // the shape of the schedule, not the duration of a turn.
        for id in started {
            if let Some(task) = states.iter_mut().find(|t| t.id == id) {
                task.state = TaskState::Verified;
            }
        }
    }

    let shape = super::plan::shape(&super::plan::Plan {
        tasks: tasks.to_vec(),
        ..Default::default()
    });

    let serial_steps = tasks.len();
    let parallel_steps = waves.len().max(1);
    let speedup = serial_steps as f64 / parallel_steps as f64;

    let per_task = estimates.worker.saturating_add(estimates.verifier);
    let estimated_tokens = estimates
        .lead
        .saturating_add(estimates.reflection)
        .saturating_add(per_task.saturating_mul(tasks.len() as u64));
    // A single agent does the same work in one context, so it pays for the work
    // but not for the coordination: roughly one worker-turn per task plus the
    // integration, with no verifier and no re-sends.
    let single_agent_tokens = estimates
        .lead
        .saturating_add(estimates.worker.saturating_mul(tasks.len() as u64));

    Rehearsal {
        waves,
        critical_path: shape.critical_path,
        max_parallel: shape.max_parallel,
        serial_steps,
        parallel_steps,
        speedup,
        projected_wall_secs: projected_wall_secs(parallel_steps, median_task_secs),
        budget_wall_secs: budget_wall_secs(parallel_steps, median_task_secs),
        median_task_secs,
        measured: measured.is_some(),
        tokens_measured_from: estimates.runs,
        estimated_tokens,
        single_agent_tokens,
    }
}

/// The concurrency a plan actually benefits from.
///
/// Never more workers than the widest rank: extra slots would sit idle and the
/// panel would show a roster of parked agents pretending to be a swarm.
pub fn useful_concurrency(tasks: &[Task], ceiling: usize) -> usize {
    let shape = super::plan::shape(&super::plan::Plan {
        tasks: tasks.to_vec(),
        ..Default::default()
    });
    shape.max_parallel.clamp(1, ceiling.max(1))
}

/// The task ids that are ready right now, in declaration order.
pub fn ready_queue(tasks: &[Task]) -> Vec<String> {
    tasks
        .iter()
        .filter(|t| t.state == TaskState::Ready)
        .map(|t| t.id.clone())
        .collect()
}

/// The set of tasks that are neither verified nor failed.
pub fn outstanding(tasks: &[Task]) -> BTreeSet<String> {
    tasks
        .iter()
        .filter(|t| !matches!(t.state, TaskState::Verified | TaskState::Failed))
        .map(|t| t.id.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str, deps: &[&str]) -> Task {
        let file = format!("src/{id}.rs");
        Task::new(id, format!("task {id}"), "do it", "cargo test passes")
            .with_files(&[file.as_str()])
            .with_deps(deps)
    }

    /* -------------------------------------------------------------- *
     * Reading a ruling
     * -------------------------------------------------------------- */

    #[test]
    fn a_verdict_that_starts_with_failed_is_a_failure_even_when_it_says_passes() {
        // The exact sentence that marked a task verified on a FAILED verdict in
        // the first live run. `contains("pass")` reads it as a pass.
        let text = "FAILED. The mechanical criterion passes on the change and fails on base, so it is a real test -- but it is too weak to catch X.";
        assert_eq!(ruling(text), Some(false));
    }

    #[test]
    fn a_verdict_that_starts_with_passed_is_a_pass() {
        assert_eq!(ruling("passed. Both halves fail on base and pass on the change."), Some(true));
        assert_eq!(ruling("PASSED"), Some(true));
        assert_eq!(ruling("Pass."), Some(true));
    }

    #[test]
    fn markdown_and_quotes_do_not_hide_the_ruling() {
        assert_eq!(ruling("**FAILED** -- the test does not run"), Some(false));
        assert_eq!(ruling("## passed"), Some(true));
        assert_eq!(ruling("> failed: no evidence"), Some(false));
        assert_eq!(ruling("  - passed  "), Some(true));
        assert_eq!(ruling("`failed`"), Some(false));
    }

    #[test]
    fn a_verdict_with_no_ruling_is_not_a_pass() {
        // A malformed verdict must never be the reason a task is accepted.
        assert_eq!(ruling("The criterion is a real test."), None);
        assert_eq!(ruling(""), None);
        assert_eq!(ruling("   "), None);
        assert_eq!(ruling("I could not decide."), None);
    }

    #[test]
    fn a_ruling_is_read_from_the_first_word_only() {
        // "not passed" is not a ruling, and a parser that scans the whole
        // sentence would find "passed" and accept it.
        assert_eq!(ruling("not passed: the base already passes"), None);
        assert_eq!(ruling("failed, because it passes on base"), Some(false));
    }

    fn verdict(passed: bool, failing_first: bool) -> Verdict {
        Verdict {
            passed,
            reason: if passed { "ok".into() } else { "broken".into() },
            evidence: vec!["cargo test → ok".into()],
            command: Some("cargo test".into()),
            base_exit: if failing_first { Some(1) } else { Some(0) },
            change_exit: Some(0),
            failing_first,
            base_dirtied: false,
            at: super::super::now(),
        }
    }

    #[test]
    fn a_task_with_no_dependencies_starts_immediately() {
        let mut tasks = vec![task("t1", &[]), task("t2", &[])];
        let transitions = advance(&mut tasks, 2);
        assert_eq!(tasks[0].state, TaskState::Running);
        assert_eq!(tasks[1].state, TaskState::Running);
        assert_eq!(transitions.len(), 4, "two unblocks and two starts");
        assert_eq!(tasks[0].attempts, 1);
    }

    #[test]
    fn concurrency_is_respected() {
        let mut tasks = vec![task("t1", &[]), task("t2", &[]), task("t3", &[])];
        advance(&mut tasks, 2);
        assert_eq!(running_count(&tasks), 2);
        assert_eq!(ready_count(&tasks), 1);
        assert_eq!(tasks[2].state, TaskState::Ready);
    }

    #[test]
    fn a_verifying_task_holds_a_live_process_the_ceiling_does_not_count() {
        // Documents the semantics the panel now reports: with three tasks
        // `Verifying` and a ceiling of three, `advance` starts three new
        // workers and six processes are live. This is not a test of a fix —
        // the ceiling deliberately bounds workers, not verifiers, and
        // serialising verifications would make the run slower.
        let mut tasks = vec![
            task("t1", &[]),
            task("t2", &[]),
            task("t3", &[]),
            task("t4", &[]),
            task("t5", &[]),
            task("t6", &[]),
        ];
        for t in &mut tasks[..3] {
            t.state = TaskState::Verifying;
        }
        for t in &mut tasks[3..] {
            t.state = TaskState::Ready;
        }
        let transitions = advance(&mut tasks, 3);
        assert_eq!(
            transitions
                .iter()
                .filter(|t| matches!(t, Transition::Start { .. }))
                .count(),
            3,
            "the ceiling bounds workers, so all three ready tasks start"
        );
        assert_eq!(live_count(&tasks), 6, "three workers plus three verifiers are live");
    }

    #[test]
    fn a_dependent_task_stays_blocked_while_its_dependency_is_still_running() {
        // `Running` is the one state that does not satisfy a dependency: the
        // worker has not posted evidence, so there is no commit to build on.
        let mut tasks = vec![task("t1", &[]), task("t2", &["t1"])];
        advance(&mut tasks, 2);
        assert_eq!(tasks[0].state, TaskState::Running);
        assert_eq!(tasks[1].state, TaskState::Blocked);

        // t1 finishes and is verified.
        tasks[0].state = TaskState::Verified;
        let transitions = advance(&mut tasks, 2);
        assert_eq!(tasks[1].state, TaskState::Running);
        assert!(transitions
            .iter()
            .any(|t| matches!(t, Transition::Start { task } if task == "t2")));
    }

    /// The speculative start, and the whole of it.
    ///
    /// t1's worker has posted its evidence and stopped, so t1's commit exists and
    /// t2 can build on it. Waiting for the verdict put verification between every
    /// link of a chain: t1 work, t1 verify, t2 work, t2 verify. The observed
    /// five-link run spent roughly 95 minutes on 30 minutes of work that way.
    ///
    /// The timestamps are the assertion, not the states: t2's start is recorded
    /// while t1 has no verdict at all, which is what "started while t1 was still
    /// verifying" means.
    #[test]
    fn a_dependent_starts_while_its_dependency_is_still_verifying() {
        let mut tasks = vec![task("t1", &[]), task("t2", &["t1"])];
        tasks[0].state = TaskState::Verifying;
        tasks[0].started_at = Some(super::super::now());
        tasks[0].ended_at = None;

        let transitions = advance(&mut tasks, 2);

        assert_eq!(tasks[1].state, TaskState::Running, "t2 starts on t1's commit");
        assert!(
            transitions
                .iter()
                .any(|t| matches!(t, Transition::Start { task } if task == "t2")),
            "and the transition says so"
        );
        assert_eq!(
            tasks[0].state,
            TaskState::Verifying,
            "t1 is still waiting for its verdict"
        );
        assert!(
            tasks[0].ended_at.is_none(),
            "no verdict has landed, so t1 has no end time"
        );
        assert!(
            tasks[1].started_at.is_some(),
            "t2's start is stamped while t1 is unverified"
        );
        assert_eq!(
            tasks[1].attempts, 1,
            "a speculative start is a real attempt"
        );
    }

    /// A refused verdict takes the dependent's branch with it.
    ///
    /// The cost of starting early, paid in the uncommon case: t2 built on work
    /// that has just been refused, so t2's branch is not a candidate for
    /// integration and its base is not a base for anything.
    #[test]
    fn a_failed_verdict_invalidates_the_task_that_built_on_it() {
        let mut tasks = vec![task("t1", &[]), task("t2", &["t1"])];
        tasks[0].state = TaskState::Verifying;
        tasks[0].attempts = 1;
        tasks[1].state = TaskState::Running;
        tasks[1].attempts = 1;
        tasks[1].branch = Some("swarm/r-t2".into());
        tasks[1].base_commit = Some("c1".into());
        tasks[1].built_on = [("t1".to_string(), "c1".to_string())].into();

        let transition = apply_verdict(&mut tasks[0], verdict(false, true), 3);
        assert!(matches!(transition, Transition::Fail { .. }));
        assert_eq!(tasks[0].state, TaskState::Ready, "t1 retries");

        let rolled = invalidate_dependents(&mut tasks, "t1");

        assert_eq!(rolled, vec!["t2".to_string()]);
        assert_eq!(tasks[1].state, TaskState::Blocked, "back in the queue");
        assert!(tasks[1].built_on.is_empty(), "it is built on nothing now");
        assert_eq!(tasks[1].branch, None, "the branch is discarded");
        assert_eq!(
            tasks[1].base_commit, None,
            "and the base it was proven against, which was t1's refused tree"
        );
        assert_eq!(
            tasks[1].attempts, 0,
            "the attempt is refunded: a dependency's failure is not the dependent's"
        );
    }

    /// Invalidation is transitive, and a `Verified` dependent is not exempt.
    ///
    /// t3 built on t2 built on t1. When t1's verdict fails, t2's branch contains
    /// refused work, so t3's branch contains it too — and t3's own verdict was
    /// earned against a base that no longer exists. Invalidating one level would
    /// leave t3 `Verified` and the run would integrate it.
    #[test]
    fn invalidation_reaches_a_task_two_links_down_and_a_verified_one() {
        let mut tasks = vec![task("t1", &[]), task("t2", &["t1"]), task("t3", &["t2"])];
        tasks[0].state = TaskState::Verifying;
        tasks[1].state = TaskState::Verifying;
        tasks[1].built_on = [("t1".to_string(), "c1".to_string())].into();
        tasks[2].state = TaskState::Verified;
        tasks[2].built_on = [("t2".to_string(), "c2".to_string())].into();

        let rolled = invalidate_dependents(&mut tasks, "t1");

        assert_eq!(rolled, vec!["t2".to_string(), "t3".to_string()]);
        assert_eq!(tasks[1].state, TaskState::Blocked);
        assert_eq!(
            tasks[2].state,
            TaskState::Blocked,
            "a verdict earned against a failed base is a statement about a tree that is gone"
        );
        assert!(tasks[2].built_on.is_empty());
    }

    /// A dependent that was unblocked but had not been given a slot yet.
    ///
    /// It has no `built_on` — nothing was prepared — so a rollback keyed only on
    /// the record would miss it, and `advance` starts a `Ready` task without
    /// re-checking its dependencies. One tick later it would be building on the
    /// commit that was just refused.
    #[test]
    fn a_ready_dependent_is_rolled_back_even_though_it_never_started() {
        let mut tasks = vec![task("t1", &[]), task("t2", &["t1"]), task("t3", &[])];
        tasks[0].state = TaskState::Verifying;
        tasks[1].state = TaskState::Ready;
        tasks[2].state = TaskState::Ready;

        let rolled = invalidate_dependents(&mut tasks, "t1");

        assert_eq!(rolled, vec!["t2".to_string()]);
        assert_eq!(tasks[1].state, TaskState::Blocked);
        assert_eq!(
            tasks[2].state,
            TaskState::Ready,
            "a task that does not depend on t1 is untouched"
        );
    }

    /// A dependent that already failed on its own merits is not resurrected.
    ///
    /// It has spent its attempts. Putting it back in the queue would spend a
    /// worker slot and a verifier turn on a task whose next verdict fails it
    /// again, and it would hide the failure from `is_stuck` — which is the check
    /// that ends the run with a reason instead of a spinner.
    #[test]
    fn a_dependent_that_already_failed_is_not_resurrected_by_a_rollback() {
        let mut tasks = vec![task("t1", &[]), task("t2", &["t1"])];
        tasks[0].state = TaskState::Verifying;
        tasks[1].state = TaskState::Failed;
        tasks[1].attempts = 3;
        tasks[1].built_on = [("t1".to_string(), "c1".to_string())].into();

        let rolled = invalidate_dependents(&mut tasks, "t1");

        assert!(rolled.is_empty(), "nothing to roll back");
        assert_eq!(tasks[1].state, TaskState::Failed);
        assert_eq!(tasks[1].attempts, 3);
    }

    /// A dependency that fails twice does not spend its dependent's attempts.
    ///
    /// The dependent did the work it was given and ran it; the invalidation is
    /// somebody else's verdict, so the attempt goes back. Without the refund a
    /// plan with one flaky dependency would exhaust every dependent's ceiling
    /// without any of them failing on its own merits.
    #[test]
    fn a_dependency_that_fails_twice_does_not_push_its_dependent_past_the_ceiling() {
        let max_attempts = 3;
        let mut tasks = vec![task("t1", &[]), task("t2", &["t1"])];

        // t1 works, posts evidence, and is refused. Twice.
        for round in 1..=2 {
            advance(&mut tasks, 2);
            assert_eq!(tasks[0].state, TaskState::Running, "round {round}");
            tasks[0].state = TaskState::Verifying;
            advance(&mut tasks, 2);
            assert_eq!(
                tasks[1].state,
                TaskState::Running,
                "t2 starts on t1's commit in round {round}"
            );
            tasks[1].built_on = [(("t1").to_string(), format!("c{round}"))].into();

            apply_verdict(&mut tasks[0], verdict(false, true), max_attempts);
            invalidate_dependents(&mut tasks, "t1");
            assert_eq!(tasks[1].state, TaskState::Blocked, "round {round}");
            assert_eq!(
                tasks[1].attempts, 0,
                "round {round}: the discarded attempt is refunded, so t2 keeps its budget"
            );
        }

        // t1 passes on its third attempt, and t2 runs again with all three of
        // its own attempts still available.
        advance(&mut tasks, 2);
        tasks[0].state = TaskState::Verifying;
        advance(&mut tasks, 2);

        assert_eq!(tasks[1].state, TaskState::Running);
        assert_eq!(tasks[1].attempts, 1);

        // And it can now fail its own verifier twice more before the ceiling
        // stops it — the budget the two invalidations would have eaten.
        for round in 1..=2 {
            apply_verdict(&mut tasks[1], verdict(false, true), max_attempts);
            assert_eq!(
                tasks[1].state,
                TaskState::Ready,
                "own failure {round} still has budget"
            );
            advance(&mut tasks, 2);
            assert_eq!(tasks[1].state, TaskState::Running, "own failure {round}");
        }
        apply_verdict(&mut tasks[1], verdict(false, true), max_attempts);
        assert_eq!(
            tasks[1].state,
            TaskState::Failed,
            "the third own failure is the ceiling, and the invalidations did not spend it"
        );
    }

    /// A task that failed its own verifier keeps that attempt through a rollback.
    ///
    /// The refund exists so a dependency's failure does not spend the dependent's
    /// budget. It must not become a way for a task that cannot pass its own
    /// verifier to be rescued indefinitely: the attempt it spent on its own
    /// refusal is its own, and it stays spent.
    #[test]
    fn a_task_that_failed_its_own_verifier_keeps_that_attempt_through_a_rollback() {
        let mut tasks = vec![task("t1", &[]), task("t2", &["t1"])];
        tasks[0].state = TaskState::Verifying;
        tasks[1].state = TaskState::Running;
        tasks[1].attempts = 1;
        tasks[1].built_on = [("t1".to_string(), "c1".to_string())].into();

        // t2's own verifier refuses it, so it is back in the queue with one
        // attempt spent.
        apply_verdict(&mut tasks[1], verdict(false, true), 3);
        assert_eq!(tasks[1].state, TaskState::Ready);
        assert_eq!(tasks[1].attempts, 1);

        // Then t1's verdict fails, and t2 is rolled back.
        apply_verdict(&mut tasks[0], verdict(false, true), 3);
        invalidate_dependents(&mut tasks, "t1");

        assert_eq!(tasks[1].state, TaskState::Blocked);
        assert_eq!(
            tasks[1].attempts, 1,
            "its own refusal is its own attempt, and the rollback does not refund it"
        );
    }

    /// A task returning from `Running` to `Blocked` is not a dead run.
    ///
    /// That is what invalidation does, and it happens while the dependency it was
    /// built on is retrying — so something is still moving and the run is not
    /// stuck. Reading the rollback as a wedge would fail a run that is recovering.
    #[test]
    fn a_task_returning_to_blocked_while_its_dependency_retries_is_not_stuck() {
        let mut tasks = vec![task("t1", &[]), task("t2", &["t1"])];
        tasks[0].state = TaskState::Verifying;
        tasks[1].state = TaskState::Running;
        tasks[1].built_on = [("t1".to_string(), "c1".to_string())].into();

        apply_verdict(&mut tasks[0], verdict(false, true), 3);
        invalidate_dependents(&mut tasks, "t1");

        assert_eq!(tasks[0].state, TaskState::Ready, "t1 is retrying");
        assert_eq!(tasks[1].state, TaskState::Blocked, "t2 was rolled back");
        assert!(
            !is_stuck(&tasks),
            "a rollback while the dependency retries is recovery, not a dead run"
        );
    }

    /// And when the dependency fails for good, the rollback really is the end.
    ///
    /// The distinction matters: the run has to stop with a reason rather than
    /// spin, and the reason is that nothing will ever verify t1 again.
    #[test]
    fn a_task_rolled_back_by_a_dependency_that_failed_for_good_is_stuck() {
        let mut tasks = vec![task("t1", &[]), task("t2", &["t1"])];
        tasks[0].state = TaskState::Verifying;
        tasks[0].attempts = 3;
        tasks[1].state = TaskState::Running;
        tasks[1].built_on = [("t1".to_string(), "c1".to_string())].into();

        apply_verdict(&mut tasks[0], verdict(false, true), 3);
        invalidate_dependents(&mut tasks, "t1");

        assert_eq!(tasks[0].state, TaskState::Failed);
        assert_eq!(tasks[1].state, TaskState::Blocked);
        assert!(is_stuck(&tasks));
    }

    #[test]
    fn verification_overlaps_implementation() {
        // t1 is verifying, which must not consume a worker slot, so t2 starts.
        let mut tasks = vec![task("t1", &[]), task("t2", &[])];
        tasks[0].state = TaskState::Verifying;
        advance(&mut tasks, 1);
        assert_eq!(tasks[1].state, TaskState::Running);
        assert_eq!(verifying_count(&tasks), 1);
        assert_eq!(running_count(&tasks), 1);
    }

    #[test]
    fn a_ready_task_waits_for_a_slot_rather_than_starting_over_the_limit() {
        let mut tasks = vec![task("t1", &[]), task("t2", &[])];
        tasks[0].state = TaskState::Running;
        advance(&mut tasks, 1);
        assert_eq!(tasks[1].state, TaskState::Ready);
        assert_eq!(running_count(&tasks), 1);
    }

    #[test]
    fn unblocking_and_starting_are_separate_transitions() {
        let mut tasks = vec![task("t1", &[]), task("t2", &["t1"])];
        tasks[0].state = TaskState::Verified;
        let transitions = advance(&mut tasks, 2);
        let unblock = transitions
            .iter()
            .position(|t| matches!(t, Transition::Unblock { task } if task == "t2"));
        let start = transitions
            .iter()
            .position(|t| matches!(t, Transition::Start { task } if task == "t2"));
        assert!(unblock.is_some() && start.is_some());
        assert!(unblock < start, "unblock must precede start");
    }

    #[test]
    fn an_earned_verdict_completes_the_task() {
        let mut t = task("t1", &[]);
        t.state = TaskState::Verifying;
        t.attempts = 1;
        let transition = apply_verdict(&mut t, verdict(true, true), 3);
        assert_eq!(t.state, TaskState::Verified);
        assert!(matches!(transition, Transition::Complete { .. }));
    }

    #[test]
    fn an_approving_verdict_is_a_failure() {
        let mut t = task("t1", &[]);
        t.state = TaskState::Verifying;
        t.attempts = 1;
        // passed = true, but the verifier never saw it fail.
        let transition = apply_verdict(&mut t, verdict(true, false), 3);
        assert_eq!(t.state, TaskState::Ready, "retry, not verified");
        assert!(matches!(transition, Transition::Fail { .. }));
    }

    /// The criterion passing is not the whole ruling.
    ///
    /// The engine's proof is failing-first — the criterion fails on the base and
    /// passes on the change — and the verifier still fails the task, because the
    /// change breaks a case the criterion does not cover. That is the one thing
    /// the engine cannot see, and it is the reason the verifier exists: a change
    /// that satisfies the test and breaks the behaviour is not a pass.
    #[test]
    fn a_change_that_passes_the_criterion_but_breaks_an_uncovered_case_is_failed() {
        let mut t = task("t1", &[]);
        t.state = TaskState::Verifying;
        t.attempts = 1;
        let mut v = verdict(false, true);
        v.reason = "the criterion passes, but an empty input panics where the base returned 0".into();

        let transition = apply_verdict(&mut t, v, 3);
        assert_eq!(
            t.state,
            TaskState::Ready,
            "an uncovered case is a fail, not a note"
        );
        assert!(matches!(transition, Transition::Fail { .. }));
        assert!(
            t.verdict
                .as_ref()
                .unwrap()
                .reason
                .contains("empty input"),
            "the case the verifier found is what the retry is told"
        );
    }

    #[test]
    fn a_failing_verdict_retries_until_the_ceiling_then_fails() {
        let mut t = task("t1", &[]);
        t.state = TaskState::Verifying;
        t.attempts = 1;
        apply_verdict(&mut t, verdict(false, true), 2);
        assert_eq!(t.state, TaskState::Ready);

        t.state = TaskState::Verifying;
        t.attempts = 2;
        apply_verdict(&mut t, verdict(false, true), 2);
        assert_eq!(t.state, TaskState::Failed);
    }

    #[test]
    fn a_dirty_base_is_a_failure_even_when_the_test_passed() {
        let mut t = task("t1", &[]);
        t.state = TaskState::Verifying;
        t.attempts = 1;
        let mut v = verdict(true, true);
        v.base_dirtied = true;
        apply_verdict(&mut t, v, 3);
        assert_eq!(t.state, TaskState::Ready);
    }

    #[test]
    fn a_run_with_a_failed_task_and_nothing_moving_is_stuck() {
        let mut tasks = vec![task("t1", &[]), task("t2", &["t1"])];
        tasks[0].state = TaskState::Failed;
        assert!(is_stuck(&tasks));
    }

    #[test]
    fn a_run_with_work_in_flight_is_not_stuck() {
        let mut tasks = vec![task("t1", &[]), task("t2", &["t1"])];
        tasks[0].state = TaskState::Running;
        assert!(!is_stuck(&tasks));
    }

    #[test]
    fn a_fully_verified_run_is_not_stuck() {
        let mut tasks = vec![task("t1", &[])];
        tasks[0].state = TaskState::Verified;
        assert!(!is_stuck(&tasks));
    }

    #[test]
    fn an_empty_run_is_not_stuck() {
        assert!(!is_stuck(&[]));
    }

    #[test]
    fn the_rehearsal_batches_independent_tasks_into_one_wave() {
        let tasks = vec![task("t1", &[]), task("t2", &[]), task("t3", &[])];
        let rehearsal = rehearse(&tasks, 3, None);
        assert_eq!(rehearsal.waves.len(), 1);
        assert_eq!(rehearsal.waves[0].tasks.len(), 3);
        assert_eq!(rehearsal.parallel_steps, 1);
        assert_eq!(rehearsal.serial_steps, 3);
        assert!((rehearsal.speedup - 3.0).abs() < 0.001);
    }

    #[test]
    fn the_rehearsal_serialises_a_chain() {
        let tasks = vec![task("t1", &[]), task("t2", &["t1"]), task("t3", &["t2"])];
        let rehearsal = rehearse(&tasks, 3, None);
        assert_eq!(rehearsal.waves.len(), 3);
        assert!((rehearsal.speedup - 1.0).abs() < 0.001);
        assert_eq!(rehearsal.critical_path.len(), 3);
    }

    #[test]
    fn the_rehearsal_respects_the_concurrency_ceiling() {
        let tasks = vec![task("t1", &[]), task("t2", &[]), task("t3", &[])];
        let rehearsal = rehearse(&tasks, 1, None);
        assert_eq!(rehearsal.waves.len(), 3);
        assert!((rehearsal.speedup - 1.0).abs() < 0.001);
    }

    #[test]
    fn the_rehearsal_terminates_on_a_cycle() {
        let tasks = vec![task("t1", &["t2"]), task("t2", &["t1"])];
        let rehearsal = rehearse(&tasks, 2, None);
        // No wave can start, so the rehearsal reports no progress rather than
        // spinning.
        assert!(rehearsal.waves.is_empty());
    }

    #[test]
    fn the_rehearsal_estimates_more_than_a_single_agent() {
        let tasks = vec![task("t1", &[]), task("t2", &[]), task("t3", &[])];
        let rehearsal = rehearse(&tasks, 3, None);
        assert!(
            rehearsal.estimated_tokens > rehearsal.single_agent_tokens,
            "a swarm costs more: {} vs {}",
            rehearsal.estimated_tokens,
            rehearsal.single_agent_tokens
        );
    }

    #[test]
    fn useful_concurrency_never_exceeds_the_widest_rank() {
        let chain = vec![task("t1", &[]), task("t2", &["t1"]), task("t3", &["t2"])];
        assert_eq!(useful_concurrency(&chain, 6), 1);

        let wide = vec![task("t1", &[]), task("t2", &[]), task("t3", &[])];
        assert_eq!(useful_concurrency(&wide, 6), 3);
        assert_eq!(useful_concurrency(&wide, 2), 2);
    }

    #[test]
    fn the_ready_queue_is_in_declaration_order() {
        let mut tasks = vec![task("t1", &[]), task("t2", &[]), task("t3", &[])];
        tasks[0].state = TaskState::Ready;
        tasks[2].state = TaskState::Ready;
        assert_eq!(ready_queue(&tasks), vec!["t1", "t3"]);
    }

    #[test]
    fn outstanding_excludes_verified_and_failed() {
        let mut tasks = vec![task("t1", &[]), task("t2", &[]), task("t3", &[])];
        tasks[0].state = TaskState::Verified;
        tasks[1].state = TaskState::Failed;
        let left = outstanding(&tasks);
        assert_eq!(left.len(), 1);
        assert!(left.contains("t3"));
    }

    /* -------------------------------------------------------------- *
     * The budget the plan implies
     * -------------------------------------------------------------- */

    #[test]
    fn a_five_deep_plan_gets_a_budget_above_the_flat_hour() {
        // The observed run was a five-deep chain given a flat 3600s, so its own
        // projection was longer than its budget and the engine would have failed
        // it mid-task with every worktree discarded.
        let assumed = DEFAULT_MEDIAN_TASK_SECS;
        assert_eq!(projected_wall_secs(5, assumed), 5 * assumed);
        assert_eq!(budget_wall_secs(5, assumed), 6000);
        assert!(
            budget_wall_secs(5, assumed) > MIN_WALL_SECS,
            "a five-deep plan must get more than the flat hour"
        );
    }

    #[test]
    fn a_short_plan_keeps_the_hour_it_always_had() {
        // The derivation exists to stop a long plan being failed by a fixed
        // number, not to make small runs tighter.
        let assumed = DEFAULT_MEDIAN_TASK_SECS;
        assert_eq!(budget_wall_secs(1, assumed), MIN_WALL_SECS);
        assert_eq!(budget_wall_secs(3, assumed), MIN_WALL_SECS);
        assert_eq!(budget_wall_secs(4, assumed), 4800);
    }

    #[test]
    fn the_projection_crosses_the_flat_hour_at_seven_steps() {
        // The panel warns when a plan's own projection is longer than the hour
        // every run used to get, so the boundary is a number the review depends
        // on. Six steps is exactly the hour; seven is the first that exceeds it.
        let assumed = DEFAULT_MEDIAN_TASK_SECS;
        assert_eq!(projected_wall_secs(6, assumed), MIN_WALL_SECS);
        assert!(projected_wall_secs(6, assumed) <= MIN_WALL_SECS);
        assert_eq!(projected_wall_secs(7, assumed), 4200);
        assert!(projected_wall_secs(7, assumed) > MIN_WALL_SECS);

        // The observed ten-task plan is five steps deep, so at the assumed 600s
        // it projects at 50 minutes and does not cross. That is a fact about the
        // fallback, not about the run: see the note on
        // `DEFAULT_MEDIAN_TASK_SECS`, and `a_measured_median_moves_the_budget`
        // for what the same plan costs once this workspace has been measured.
        assert_eq!(projected_wall_secs(5, assumed), 3000);
        assert!(projected_wall_secs(5, assumed) < MIN_WALL_SECS);
    }

    /// Two past runs of one workspace whose tasks each took `secs` seconds.
    ///
    /// Two, because `History::role_medians` refuses to report a median from one
    /// observation, and the rehearsal must not present an anecdote as a
    /// measurement.
    fn measured_history(secs: u64) -> super::super::history::History {
        let mut runs = Vec::new();
        for id in ["r1", "r2"] {
            let mut run = super::super::Run::new(
                "goal",
                "/ws",
                super::super::Member::new("lead", Role::Lead, "mistral-large"),
            );
            run.id = id.to_string();
            run.state = super::super::RunState::Done;
            // The record the history reads is `role_usage`, stamped when a run
            // stops — not the roster, which is live and gone by then.
            run.role_usage.push(super::super::RoleUsage {
                role: Role::Worker,
                turns: 1,
                secs: secs as f64,
                tokens_in: 39_000_000,
                tokens_out: 1_000_000,
            });
            let mut task = super::Task::new("t1", "One", "Do it", "node --test a.test.js");
            task.started_at = Some("2026-01-01T00:00:00.000Z".into());
            task.ended_at = Some(
                (chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00.000Z")
                    .unwrap()
                    + chrono::Duration::seconds(secs as i64))
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            );
            run.tasks.push(task);
            runs.push(run);
        }
        super::super::history::History::from_runs(runs, "/ws", None)
    }

    /// The token estimate follows this workspace's own history, and says so.
    ///
    /// The constants promised 85,000 tokens for one task — a worker turn and a
    /// verifier turn — when a worker turn on this machine cost 40M. The rehearsal
    /// is what the user decides on, so it is the number that has to be measured.
    #[test]
    fn the_token_estimate_follows_the_measured_worker_turn() {
        let tasks = vec![task("t1", &[]), task("t2", &["t1"])];
        let assumed = rehearse(&tasks, 2, None);
        assert_eq!(assumed.tokens_measured_from, None, "a guess says it is one");
        assert_eq!(
            assumed.estimated_tokens,
            ESTIMATE_LEAD_TOKENS
                + ESTIMATE_REFLECTION_TOKENS
                + (ESTIMATE_WORKER_TOKENS + ESTIMATE_VERIFIER_TOKENS) * 2
        );

        let history = measured_history(1620);
        let measured = rehearse(&tasks, 2, Some(&history));
        assert_eq!(measured.tokens_measured_from, Some(2));
        // The worker's turn was 40M tokens a turn, so two tasks cost far more than
        // the constant claimed — and the verifier has no measurement, so it keeps
        // its constant rather than borrowing the worker's.
        assert_eq!(
            measured.estimated_tokens,
            ESTIMATE_LEAD_TOKENS
                + ESTIMATE_REFLECTION_TOKENS
                + (40_000_000 + ESTIMATE_VERIFIER_TOKENS) * 2
        );
        assert!(measured.estimated_tokens > assumed.estimated_tokens * 100);
    }

    /// The fourth proof of Phase 15: the derived budget moves when the median
    /// moves.
    ///
    /// The observed run's worker turn was 6 minutes and its verifier's 20.7, so a
    /// task-pair is about 27 minutes — 1620 seconds, not the 600 the constant
    /// assumed. At that figure the same five-deep plan projects at 135 minutes
    /// instead of 50, and the warning that was silent on the constant fires.
    #[test]
    fn a_measured_median_moves_the_budget() {
        let assumed = DEFAULT_MEDIAN_TASK_SECS;
        let measured = 1620;

        assert_eq!(projected_wall_secs(5, assumed), 3000);
        assert_eq!(projected_wall_secs(5, measured), 8100);
        assert_eq!(budget_wall_secs(5, assumed), 6000);
        assert_eq!(budget_wall_secs(5, measured), 16200);
        assert!(
            budget_wall_secs(5, measured) > budget_wall_secs(5, assumed),
            "a slower workspace must get a longer clock"
        );

        // The same plan, rehearsed twice: the only difference is the median, and
        // the budget follows it.
        let tasks = vec![
            task("t1", &[]),
            task("t2", &["t1"]),
            task("t3", &["t2"]),
            task("t4", &["t3"]),
            task("t5", &["t4"]),
        ];
        let on_the_constant = rehearse(&tasks, 4, None);
        let measured_history = measured_history(measured);
        let on_the_measurement = rehearse(&tasks, 4, Some(&measured_history));
        assert_eq!(on_the_constant.parallel_steps, on_the_measurement.parallel_steps);
        assert_eq!(on_the_constant.budget_wall_secs, 6000);
        assert_eq!(on_the_measurement.budget_wall_secs, 16200);
        assert_eq!(on_the_measurement.median_task_secs, measured);
        assert!(!on_the_constant.measured, "the fallback says it is a guess");
        assert!(on_the_measurement.measured, "a measurement says so");
    }

    #[test]
    fn a_seven_deep_chain_projects_past_the_flat_hour() {
        // Seven links is an ordinary plan, and it is the first depth whose
        // projection exceeds the hour every run used to be given. The review
        // warns on that number, so the rehearsal has to report it.
        let mut tasks = vec![task("t1", &[])];
        for index in 2..=7 {
            let id = format!("t{index}");
            let dep = format!("t{}", index - 1);
            tasks.push(task(&id, &[dep.as_str()]));
        }
        let rehearsal = rehearse(&tasks, 4, None);
        assert_eq!(rehearsal.parallel_steps, 7);
        assert_eq!(rehearsal.projected_wall_secs, 4200);
        assert!(rehearsal.projected_wall_secs > MIN_WALL_SECS);
        assert_eq!(rehearsal.budget_wall_secs, 8400);
    }

    #[test]
    fn the_rehearsal_reports_the_projection_and_the_budget_it_implies() {
        // The review shows both numbers side by side, so both have to be on the
        // rehearsal rather than derived again by the panel.
        let tasks = vec![
            task("t1", &[]),
            task("t2", &["t1"]),
            task("t3", &["t2"]),
            task("t4", &["t3"]),
            task("t5", &["t4"]),
        ];
        let rehearsal = rehearse(&tasks, 4, None);
        assert_eq!(rehearsal.parallel_steps, 5);
        assert_eq!(rehearsal.projected_wall_secs, 3000);
        assert_eq!(rehearsal.budget_wall_secs, 6000);

        // The panel reads these two by name, so the names are part of the
        // contract rather than an implementation detail.
        let body = serde_json::to_string(&rehearsal).unwrap();
        assert!(body.contains("\"projectedWallSecs\":3000"), "{body}");
        assert!(body.contains("\"budgetWallSecs\":6000"), "{body}");
    }
}
