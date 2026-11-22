//! What the last runs of this workspace learned.
//!
//! The reflection pass is the swarm's only learning mechanism, and until this
//! module existed nothing consumed it: `reflect::relevant` was reachable only
//! through [`super::engine::gotchas_for`], which was called only from a test, and
//! [`super::roles::lead_brief`] was handed no history at all, so every run
//! planned as if it were the first.
//!
//! There is no per-workspace store here, and this module does not add one. A past
//! run's gotchas live on its own [`Run`], so "what this workspace learned" is a
//! read of the last few `run.json` files. §13 of the plan refuses a second memory
//! system, and a store that had to be kept in step with the runs would be exactly
//! that: a second answer to a question the runs already answer, and a second
//! thing to go stale.
//!
//! Everything here is bounded, and the briefs say what they cut. A history
//! section that grew with the number of runs would spend the lead's first turn
//! reading about runs that are not this one.

use super::{Gotcha, Role, Run};

/// How many past runs a lead is shown.
///
/// Five, because the point is a pattern rather than a record: one run is an
/// anecdote, two is a coincidence, and five is enough to see that this workspace
/// keeps producing a one-wide first rank without spending the lead's first turn
/// on a history lesson.
pub const HISTORY_RUNS: usize = 5;

/// How many failed tasks one past run names.
///
/// Three. A run that failed eight tasks has one story — the plan was wrong — and
/// the fourth verdict is the same story with different file names.
pub const HISTORY_FAILURES: usize = 3;

/// How many shape warnings one past run repeats.
pub const HISTORY_WARNINGS: usize = 3;

/// How many characters of a verdict's reason survive into the brief.
///
/// A verdict is a paragraph. The lead needs the claim, not the argument, and a
/// reason cut mid-word is still more useful than a reason that pushed the rest of
/// the history out of the brief.
pub const HISTORY_REASON_CHARS: usize = 240;

/// How many gotchas one worker is shown.
///
/// The same ceiling the reflection pass writes per run, because a brief that
/// carried forty gotchas would be a brief the worker skims.
pub const HISTORY_GOTCHAS: usize = 8;

/// The past runs of one workspace, and what they can be asked.
///
/// Loaded once per reader rather than cached: the panel already reads every
/// `run.json` on every poll, so a handful more reads at a member's spawn is not a
/// cost worth a cache that could go stale between runs.
#[derive(Debug, Clone, Default)]
pub struct History {
    /// Newest first, at most [`HISTORY_RUNS`].
    runs: Vec<Run>,
}

impl History {
    /// Nothing known: a workspace with no finished run, or no run at all.
    pub fn empty() -> Self {
        Self { runs: Vec::new() }
    }

    /// The past runs of one workspace, newest first, read from disk.
    ///
    /// `exclude` is the run being planned or started. It is passed by id rather
    /// than assumed absent: [`super::list_runs`] reads the directory, and the
    /// current run is written there before its lead is spawned, so it is always
    /// in the list and would otherwise be the newest thing the lead is told about
    /// itself.
    ///
    /// A live run is not a past run and is skipped. A run that is still going has
    /// no outcome to report, and a lead told "the last run here is running" has
    /// learned nothing it can plan against.
    ///
    /// The window is [`super::RUN_HISTORY`] runs overall, not per workspace:
    /// `list_runs` truncates before this filter sees it, so a workspace whose
    /// last five runs are older than twenty runs elsewhere gets fewer than five.
    /// That is the honest bound — the alternative is a directory scan per member
    /// spawn — and it errs toward less history rather than more.
    pub fn of(workspace: &str, exclude: Option<&str>) -> Self {
        Self::from_runs(super::list_runs(), workspace, exclude)
    }

    /// The same selection, from runs already in hand.
    ///
    /// Split from [`History::of`] because the selection and the derivation are
    /// the parts worth testing, and the app data directory is a process-wide
    /// `OnceLock` that a test cannot point somewhere else.
    ///
    /// The workspace match is exact. A run in a sibling checkout of the same
    /// repository is a different workspace with different files, and a gotcha
    /// about a path that does not exist here is worse than no gotcha.
    pub fn from_runs(runs: Vec<Run>, workspace: &str, exclude: Option<&str>) -> Self {
        let runs = runs
            .into_iter()
            .filter(|run| !run.state.is_live())
            .filter(|run| run.workspace == workspace)
            .filter(|run| Some(run.id.as_str()) != exclude)
            .take(HISTORY_RUNS)
            .collect();
        Self { runs }
    }

    pub fn is_empty(&self) -> bool {
        self.runs.is_empty()
    }

    pub fn len(&self) -> usize {
        self.runs.len()
    }

    /// Every gotcha the past runs of this workspace recorded, newest first.
    ///
    /// A run whose reflection was degraded or never ran contributes none: the
    /// field is `Option` and a cancelled run has no reflection at all, so the
    /// absence is read as "nothing was learned" rather than as an error.
    ///
    /// Deduplicated by file and text, keeping the newest. Two runs that hit the
    /// same trap write the same sentence, and a worker shown it twice learns
    /// nothing the second time.
    pub fn gotchas(&self) -> Vec<Gotcha> {
        let mut out: Vec<Gotcha> = Vec::new();
        for run in &self.runs {
            let Some(reflection) = run.reflection.as_ref() else {
                continue;
            };
            for gotcha in &reflection.gotchas {
                let seen = out
                    .iter()
                    .any(|kept| kept.file == gotcha.file && kept.text == gotcha.text);
                if !seen {
                    out.push(gotcha.clone());
                }
            }
        }
        out
    }

    /// The median seconds one task took, over the past runs of this workspace.
    ///
    /// A task's span is `ended_at - started_at`, which is the worker's turn plus
    /// its verifier's — the same unit the rehearsal multiplies by the plan's
    /// depth. A task missing either stamp is skipped rather than counted as zero:
    /// a failed task is not guaranteed to have both, and a zero would drag the
    /// median down and hand the next run a budget that is too short, which is the
    /// failure the derivation exists to prevent.
    ///
    /// `None` when nothing has been measured, which is the first run in a
    /// workspace. The caller falls back to
    /// [`super::schedule::DEFAULT_MEDIAN_TASK_SECS`] and says so.
    pub fn median_task_secs(&self) -> Option<u64> {
        let mut spans: Vec<u64> = Vec::new();
        for run in &self.runs {
            for task in &run.tasks {
                if let Some(secs) = task_span_secs(task) {
                    spans.push(secs);
                }
            }
        }
        median(&mut spans)
    }

    /// What each role's turns have actually cost here, median across runs.
    ///
    /// Only roles with at least two contributing runs are returned. One
    /// observation is not a median, and a review that showed a single run's
    /// number under that word would be presenting an anecdote as a measurement —
    /// which is the mistake this phase exists to stop making about the budget.
    pub fn role_medians(&self) -> Vec<RoleMedian> {
        let mut out = Vec::new();
        for role in [Role::Lead, Role::Worker, Role::Verifier, Role::Reflector] {
            let mut turn_secs: Vec<u64> = Vec::new();
            let mut turn_tokens: Vec<u64> = Vec::new();
            for run in &self.runs {
                let Some(usage) = run.role_usage.iter().find(|u| u.role == role) else {
                    continue;
                };
                let (Some(secs), Some(tokens)) = (usage.turn_secs(), usage.turn_tokens()) else {
                    continue;
                };
                turn_secs.push(secs);
                turn_tokens.push(tokens);
            }
            if turn_secs.len() < 2 {
                continue;
            }
            let runs = turn_secs.len();
            let (Some(secs), Some(tokens)) = (median(&mut turn_secs), median(&mut turn_tokens))
            else {
                continue;
            };
            out.push(RoleMedian {
                role,
                turn_secs: secs,
                turn_tokens: tokens,
                runs,
            });
        }
        out
    }

    /// The lead's history section, or nothing when this workspace has no past.
    ///
    /// Three facts per run, which are the three the plan names: the goal, the
    /// plan's shape, and which tasks failed and the verdict that failed them.
    /// That is the data that would have told the observed run's lead that a
    /// 22-file first task is a rank of one.
    pub fn lead_section(&self) -> String {
        if self.runs.is_empty() {
            return String::new();
        }
        let mut out = String::from(
            "## What the last runs of this workspace learned\n\
             The finished runs here before this one, newest first. Plan against this, not\n\
             against the hope that this one is different.\n\n",
        );
        for (index, run) in self.runs.iter().enumerate() {
            out.push_str(&self.run_block(index + 1, run));
        }
        out.push_str(&format!(
            "Bounded on purpose: the {HISTORY_RUNS} most recent finished runs, at most \
             {HISTORY_FAILURES} failures and {HISTORY_WARNINGS} shape warnings per run, and \
             each verdict cut at {HISTORY_REASON_CHARS} characters.\n\n"
        ));
        out
    }

    /// One past run, as the lead reads it.
    fn run_block(&self, index: usize, run: &Run) -> String {
        let shape = super::plan::shape(&super::plan::Plan {
            tasks: run.tasks.clone(),
            ..Default::default()
        });
        let mut out = format!(
            "### {index}. {} — {}\n{} task{}, {} deep, first rank {} wide, widest {}, {} files.\n",
            one_line(&run.goal),
            run.state.label(),
            run.tasks.len(),
            if run.tasks.len() == 1 { "" } else { "s" },
            shape.critical_path.len(),
            shape.first_rank_width,
            shape.max_parallel,
            shape.files,
        );

        let failed: Vec<&super::Task> = run
            .tasks
            .iter()
            .filter(|task| task.state == super::TaskState::Failed)
            .collect();
        for task in failed.iter().take(HISTORY_FAILURES) {
            out.push_str(&format!(
                "- {} failed after {} attempt{}: {}\n",
                task.id,
                task.attempts,
                if task.attempts == 1 { "" } else { "s" },
                failure_reason(task),
            ));
        }
        if failed.len() > HISTORY_FAILURES {
            out.push_str(&format!(
                "- and {} more failed task{}.\n",
                failed.len() - HISTORY_FAILURES,
                if failed.len() - HISTORY_FAILURES == 1 {
                    ""
                } else {
                    "s"
                },
            ));
        }

        for warning in run.shape_warnings.iter().take(HISTORY_WARNINGS) {
            out.push_str(&format!("- shape: {}\n", warning.message()));
        }

        if let Some(failure) = run.failure.as_deref() {
            out.push_str(&format!("- the run stopped: {}\n", cut(failure)));
        }
        out.push('\n');
        out
    }
}

/// One role's measured turn, as the plan review shows it.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoleMedian {
    pub role: Role,
    /// Median seconds one turn of this role took.
    pub turn_secs: u64,
    /// Median tokens one turn of this role cost, in and out.
    pub turn_tokens: u64,
    /// How many runs contributed. Never below two — see [`History::role_medians`].
    pub runs: usize,
}

/// How long one task took, from the stamps the scheduler writes.
///
/// `started_at` is set when the task takes a worker slot and `ended_at` when a
/// verdict lands, so the span is the whole step: the worker's turn and the
/// verifier's. A retry resets `started_at`, so the span is the last attempt
/// rather than the sum — which is the number the budget multiplies, because the
/// retry allowance is already the factor of two in
/// [`super::schedule::budget_wall_secs`].
fn task_span_secs(task: &super::Task) -> Option<u64> {
    let started = chrono::DateTime::parse_from_rfc3339(task.started_at.as_deref()?).ok()?;
    let ended = chrono::DateTime::parse_from_rfc3339(task.ended_at.as_deref()?).ok()?;
    let secs = (ended - started).num_seconds();
    // A span of zero is a clock that did not move, not a task that was free.
    if secs <= 0 {
        return None;
    }
    Some(secs as u64)
}

/// The middle of a sorted list, averaging the two middles when the count is even.
///
/// Sorts in place and returns `None` for an empty list. Integer arithmetic
/// throughout: a median of seconds is a number the budget is multiplied by, and a
/// fractional one would only be rounded somewhere less visible.
fn median(values: &mut [u64]) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    let middle = values.len() / 2;
    if values.len() % 2 == 1 {
        Some(values[middle])
    } else {
        Some(values[middle - 1] / 2 + values[middle] / 2)
    }
}

/// Why a task failed, in the words that failed it.
///
/// The verdict's reason when there is one, because that is the sentence the next
/// lead can act on. A task the engine failed without a verdict says so rather
/// than being given a reason it never had.
fn failure_reason(task: &super::Task) -> String {
    if let Some(verdict) = task.verdict.as_ref() {
        return cut(&verdict.reason);
    }
    if let Some(conflict) = task.conflict.as_deref() {
        return cut(conflict);
    }
    "the engine failed it without a verdict".to_string()
}

/// One line, cut at [`HISTORY_REASON_CHARS`], with the cut marked.
fn cut(text: &str) -> String {
    let flat = one_line(text);
    if flat.chars().count() <= HISTORY_REASON_CHARS {
        return flat;
    }
    let kept: String = flat.chars().take(HISTORY_REASON_CHARS).collect();
    format!("{}…", kept.trim_end())
}

/// A paragraph as one line, because a brief is read as a list.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swarm::{Member, Reflection, Task, TaskState, Verdict};

    fn run(id: &str, workspace: &str, state: super::super::RunState) -> Run {
        let mut run = Run::new("add rate limiting", workspace, Member::new("lead", Role::Lead, "m"));
        run.id = id.to_string();
        run.state = state;
        run
    }

    fn task(id: &str, state: TaskState) -> Task {
        let mut task = Task::new(id, format!("task {id}"), "do it", "cargo test");
        task.state = state;
        task
    }

    fn at(secs: i64) -> String {
        chrono::DateTime::from_timestamp(1_700_000_000 + secs, 0)
            .unwrap()
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    }

    fn gotcha(file: &str, text: &str) -> Gotcha {
        Gotcha {
            file: file.into(),
            text: text.into(),
            source: "verdict t1".into(),
        }
    }

    /* -------------------------------------------------------------- *
     * Which runs count
     * -------------------------------------------------------------- */

    #[test]
    fn only_finished_runs_of_this_workspace_are_history() {
        let mut live = run("r-live", "/ws", super::super::RunState::Running);
        live.goal = "still going".into();
        let mut other = run("r-other", "/elsewhere", super::super::RunState::Done);
        other.goal = "a different repository".into();
        let mut done = run("r-done", "/ws", super::super::RunState::Done);
        done.goal = "the one that counts".into();

        let history = History::from_runs(vec![live, other, done], "/ws", None);
        assert_eq!(history.len(), 1);
        let section = history.lead_section();
        assert!(section.contains("the one that counts"), "{section}");
        assert!(!section.contains("still going"), "{section}");
        assert!(!section.contains("a different repository"), "{section}");
    }

    #[test]
    fn the_run_being_planned_is_not_its_own_history() {
        // `list_runs` reads the directory, and the current run is written there
        // before its lead is spawned. Excluding it by id is the only thing that
        // stops a lead being told about itself.
        let mut current = run("r-now", "/ws", super::super::RunState::Done);
        current.goal = "this very run".into();
        let history = History::from_runs(vec![current], "/ws", Some("r-now"));
        assert!(history.is_empty());
        assert_eq!(history.lead_section(), "");
    }

    #[test]
    fn history_is_bounded_to_five_runs() {
        let runs: Vec<Run> = (0..9)
            .map(|index| {
                let mut run = run(&format!("r{index}"), "/ws", super::super::RunState::Done);
                run.goal = format!("goal {index}");
                run
            })
            .collect();
        let history = History::from_runs(runs, "/ws", None);
        assert_eq!(history.len(), HISTORY_RUNS);
        let section = history.lead_section();
        assert!(section.contains("goal 0"), "newest first, so the first five");
        assert!(!section.contains("goal 5"), "{section}");
        // The brief says what it cut, so a lead cannot read five as all of them.
        assert!(section.contains("5 most recent finished runs"), "{section}");
    }

    /* -------------------------------------------------------------- *
     * The lead's history
     * -------------------------------------------------------------- */

    #[test]
    fn a_past_run_reports_its_goal_shape_and_the_verdict_that_failed_it() {
        let mut past = run("r1", "/ws", super::super::RunState::Failed);
        past.goal = "add rate limiting to the http client".into();
        let mut t1 = task("t1", TaskState::Failed);
        t1.attempts = 3;
        t1.files = (0..22).map(|i| format!("src/f{i}.rs")).collect();
        t1.verdict = Some(Verdict {
            passed: false,
            reason: "the middleware does not handle a burst".into(),
            evidence: Vec::new(),
            command: None,
            base_exit: Some(1),
            change_exit: Some(0),
            failing_first: true,
            base_dirtied: false,
            at: at(0),
        });
        past.tasks = vec![t1, {
            // t2 depends on t1, so the first rank is one wide — which is the
            // fact the observed run's lead needed and did not have.
            let mut t2 = task("t2", TaskState::Verified);
            t2.deps = vec!["t1".into()];
            t2
        }];
        past.failure = Some("no task can make progress".into());

        let section = History::from_runs(vec![past], "/ws", None).lead_section();
        assert!(section.contains("add rate limiting to the http client"), "{section}");
        assert!(section.contains("first rank 1 wide"), "{section}");
        assert!(section.contains("22 files"), "{section}");
        assert!(section.contains("t1 failed after 3 attempts"), "{section}");
        assert!(
            section.contains("the middleware does not handle a burst"),
            "the verdict that failed it is the point: {section}"
        );
        assert!(section.contains("the run stopped: no task can make progress"), "{section}");
    }

    #[test]
    fn a_long_verdict_is_cut_and_the_cut_is_marked() {
        let mut past = run("r1", "/ws", super::super::RunState::Failed);
        let mut t1 = task("t1", TaskState::Failed);
        t1.attempts = 1;
        t1.verdict = Some(Verdict {
            passed: false,
            reason: "x".repeat(HISTORY_REASON_CHARS * 2),
            evidence: Vec::new(),
            command: None,
            base_exit: None,
            change_exit: None,
            failing_first: false,
            base_dirtied: false,
            at: at(0),
        });
        past.tasks = vec![t1];
        let section = History::from_runs(vec![past], "/ws", None).lead_section();
        assert!(section.contains('…'), "a cut reason says it was cut: {section}");
        assert!(!section.contains(&"x".repeat(HISTORY_REASON_CHARS + 1)), "{section}");
    }

    #[test]
    fn a_run_with_no_reflection_contributes_no_gotchas() {
        // A degraded or cancelled run has no reflection at all. Reading that as
        // an error would make the first cancelled run poison the workspace.
        let mut past = run("r1", "/ws", super::super::RunState::Cancelled);
        past.reflection = None;
        assert!(History::from_runs(vec![past], "/ws", None).gotchas().is_empty());
    }

    /* -------------------------------------------------------------- *
     * The worker's gotchas
     * -------------------------------------------------------------- */

    #[test]
    fn gotchas_are_collected_newest_first_and_deduplicated() {
        let mut older = run("r1", "/ws", super::super::RunState::Done);
        older.reflection = Some(Reflection {
            at: at(0),
            summary: String::new(),
            gotchas: vec![gotcha("src/rate.rs", "the limiter lives here")],
            degraded: None,
        });
        let mut newer = run("r2", "/ws", super::super::RunState::Done);
        newer.reflection = Some(Reflection {
            at: at(10),
            summary: String::new(),
            gotchas: vec![
                gotcha("src/rate.rs", "the limiter lives here"),
                gotcha("src/http.rs", "the retry loop swallows 429s"),
            ],
            degraded: None,
        });

        // Newest first, as `list_runs` returns them.
        let gotchas = History::from_runs(vec![newer, older], "/ws", None).gotchas();
        assert_eq!(gotchas.len(), 2, "the repeated sentence is kept once");
        assert_eq!(gotchas[0].file, "src/rate.rs");
        assert_eq!(gotchas[1].file, "src/http.rs");
    }

    /* -------------------------------------------------------------- *
     * The measured median
     * -------------------------------------------------------------- */

    #[test]
    fn the_median_task_span_comes_from_the_stamps_the_scheduler_writes() {
        let mut past = run("r1", "/ws", super::super::RunState::Done);
        let mut fast = task("t1", TaskState::Verified);
        fast.started_at = Some(at(0));
        fast.ended_at = Some(at(300));
        let mut slow = task("t2", TaskState::Verified);
        slow.started_at = Some(at(0));
        slow.ended_at = Some(at(900));
        let mut unmeasured = task("t3", TaskState::Failed);
        unmeasured.started_at = Some(at(0));
        // No `ended_at`: a failed task is not guaranteed to have one, and a zero
        // would drag the median down and hand the next run a budget too short.
        past.tasks = vec![fast, slow, unmeasured];

        let history = History::from_runs(vec![past], "/ws", None);
        assert_eq!(history.median_task_secs(), Some(600));
    }

    #[test]
    fn a_workspace_with_no_measured_task_has_no_median() {
        let past = run("r1", "/ws", super::super::RunState::Done);
        assert_eq!(History::from_runs(vec![past], "/ws", None).median_task_secs(), None);
        assert_eq!(History::empty().median_task_secs(), None);
    }

    #[test]
    fn a_role_median_needs_two_runs_and_one_does_not_make_it() {
        let with = |id: &str, secs: f64, turns: u32| {
            let mut run = run(id, "/ws", super::super::RunState::Done);
            run.role_usage = vec![super::super::RoleUsage {
                role: Role::Verifier,
                turns,
                secs,
                tokens_in: 1000,
                tokens_out: 0,
            }];
            run
        };

        // One run is an anecdote, and the review must not call it a median.
        let one = History::from_runs(vec![with("r1", 1242.0, 1)], "/ws", None);
        assert!(one.role_medians().is_empty());

        // Two runs: the verifier took 20.7 minutes in one and 6 in the other.
        let two = History::from_runs(
            vec![with("r1", 1242.0, 1), with("r2", 360.0, 1)],
            "/ws",
            None,
        );
        let medians = two.role_medians();
        assert_eq!(medians.len(), 1);
        assert_eq!(medians[0].role, Role::Verifier);
        assert_eq!(medians[0].runs, 2);
        assert_eq!(medians[0].turn_secs, 801, "the middle of 360 and 1242");
        assert_eq!(medians[0].turn_tokens, 1000);
    }

    #[test]
    fn a_role_median_is_per_turn_not_per_run() {
        // The observed run's verifier spent 20.7 minutes over one turn and its
        // worker 6 over one; a run with two worker turns of 6 minutes each is
        // still a 6-minute turn, not a 12-minute one.
        let mut past = run("r1", "/ws", super::super::RunState::Done);
        past.role_usage = vec![super::super::RoleUsage {
            role: Role::Worker,
            turns: 2,
            secs: 720.0,
            tokens_in: 0,
            tokens_out: 0,
        }];
        let mut second = run("r2", "/ws", super::super::RunState::Done);
        second.role_usage = vec![super::super::RoleUsage {
            role: Role::Worker,
            turns: 1,
            secs: 360.0,
            tokens_in: 0,
            tokens_out: 0,
        }];
        let medians = History::from_runs(vec![past, second], "/ws", None).role_medians();
        assert_eq!(medians[0].turn_secs, 360);
    }
}
