//! The swarm.
//!
//! A lead that plans and integrates, workers that implement in isolated git
//! worktrees, and verifiers that try to break what the workers produced. The
//! engine here is substrate — persistence, scheduling, isolation, budget — not
//! a router. Members talk to each other through the board, and the engine only
//! decides *when* a member may run, never *what* it says.
//!
//! | Module | Owns |
//! | --- | --- |
//! | [`plan`] | the plan schema, and every reason a plan is refused |
//! | [`board`] | the append-only inter-agent log and the decision digests |
//! | [`schedule`] | the ready-queue, retries, and the rehearsal |
//! | [`worktree`] | the git lifecycle, behind a trait so it is testable |
//! | [`roles`] | what each role is told, and which model it runs on |
//! | [`catalog`] | which models and reasoning levels the agent will accept |
//! | [`integrate`] | DAG-order merge and conflict attribution |
//! | [`permissions`] | who answers a member's permission request, and why |
//! | [`reflect`] | the post-run pass that writes gotchas into memory |
//! | [`history`] | what the last runs of a workspace learned, for the next one |
//! | [`engine`] | the orchestrator that wires all of it to real ACP sessions |
//!
//! The one thing this module does not own is the agent process. A member is a
//! real `vibe-acp` session with its own worktree, because ACP exposes no
//! subagent observability: a member that is not a session has no roster row, no
//! transcript, and no model of its own.

pub mod amend;
pub mod board;
pub mod catalog;
pub mod commands;
pub mod engine;
pub mod history;
pub mod integrate;
pub mod permissions;
pub mod plan;
pub mod prove;
pub mod rates;
pub mod reflect;
pub mod roles;
pub mod schedule;
pub mod skills;
pub mod watch;
pub mod worktree;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

/// A timestamp in the shape the rest of the app uses.
pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// How many board entries the panel is handed.
///
/// The board on disk is unbounded — it is the record. The panel shows the tail,
/// the same way a capability panel shows the last twenty events. A run that
/// produces four hundred entries must not serialise four hundred entries on
/// every poll.
pub const BOARD_TAIL: usize = 120;

/// How many finished runs the panel remembers.
pub const RUN_HISTORY: usize = 20;

/// How many of a member's turns the timeline keeps.
///
/// Twelve, because the timeline draws one row per member at fifteen pixels in a
/// panel a few hundred pixels wide, and twelve spans is the most that can each
/// stay a mark you can see and click. A member past twelve turns has been nudged
/// or restarted — the roster counts both — and at that point the row's question
/// is "was it working" rather than "how many times", which the shape answers
/// whether or not every turn is in it.
pub const SPANS_KEPT: usize = 12;

/// How many token readings a member's rate series keeps.
///
/// Forty, which is exactly the sparkline's width in pixels: one reading per
/// pixel column, so the drawing is the data rather than a resampling of it. The
/// series is the current turn's readings and nothing else, so a member between
/// turns carries none — which is what makes a flat line mean "not spending"
/// rather than "spending nothing lately".
pub const RATE_KEPT: usize = 40;

/// The default worker slots.
///
/// Four, not eight. Each slot is a process with its own model session, and the
/// useful ceiling is the plan's width, not the machine's core count — a plan with
/// two independent branches gains nothing from eight workers and pays for six
/// idle sessions.
pub fn default_concurrency() -> usize {
    4
}

/// How many times a task may be sent back to its worker before it is failed.
///
/// Three, not two. The ceiling used to be a literal `2` at the call site, which
/// is one retry: a worker that lost its first attempt to something outside its
/// control — a criterion the engine could not parse, a flaky suite — had no
/// second chance, and the run died over work that was nearly done. The number
/// lives on the run so the panel can show it and a resumed run keeps the
/// ceiling it started with.
pub fn default_max_attempts() -> u32 {
    3
}

/// How many times the integrated suite may be repaired before the run fails.
///
/// Two. A repair round is a whole worker and verifier cycle on the merged tree,
/// so it is the most expensive thing the run can do; two is enough to absorb a
/// real integration bug and few enough that a suite which is simply wrong — a
/// command that cannot pass in this repo — fails the run instead of burning the
/// budget on it.
pub const MAX_REPAIR_ROUNDS: u32 = 2;

/* ------------------------------------------------------------------ *
 * Identifiers
 * ------------------------------------------------------------------ */

/// A run id is a path component, so it is checked before it is joined.
///
/// Same rule as `chats::valid_id`: short, and made only of the characters a
/// generated id uses. This is the one place a string from the webview becomes a
/// directory name.
pub fn valid_run_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Mint a run id. The counter keeps two runs started in the same second apart.
pub fn new_run_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    format!(
        "run-{}-{:03}",
        chrono::Utc::now().format("%Y%m%d-%H%M%S"),
        n % 1000
    )
}

/* ------------------------------------------------------------------ *
 * States
 * ------------------------------------------------------------------ */

/// Where a whole run is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunState {
    /// The lead is writing the plan. Nothing has been spawned but the lead.
    Planning,
    /// The plan validated and is waiting for the user's one decision.
    AwaitingApproval,
    /// Tasks are being dispatched, implemented and verified.
    Running,
    /// Every task is verified; the lead is merging in DAG order.
    Integrating,
    /// The integrated suite passed; the reflection pass is running.
    Reflecting,
    Done,
    /// A named failure. `Run::failure` says which.
    Failed,
    /// A member stopped because the plan does not cover something it broke, and
    /// the lead is being asked to amend the plan. The run resumes on its own.
    Replanning,
    /// A member asked for a decision only the user can make. `Run::failure`
    /// carries the member's question verbatim.
    ///
    /// This is not `Failed`: nothing went wrong, the run simply reached a point
    /// where the plan and the code disagreed and the member that noticed was
    /// right to stop. Before this state existed the escalation was drained onto
    /// the board and then ignored, so the run carried on and died minutes later
    /// with "no task can make progress" — a reason that named nothing.
    Escalated,
    /// The user stopped it.
    Cancelled,
    /// The app closed mid-run. Members are `lost` until resumed.
    Interrupted,
}

impl RunState {
    pub fn label(self) -> &'static str {
        match self {
            RunState::Planning => "planning",
            RunState::AwaitingApproval => "awaiting approval",
            RunState::Running => "running",
            RunState::Integrating => "integrating",
            RunState::Reflecting => "reflecting",
            RunState::Done => "done",
            RunState::Failed => "failed",
            RunState::Replanning => "replanning",
            RunState::Escalated => "escalated",
            RunState::Cancelled => "cancelled",
            RunState::Interrupted => "interrupted",
        }
    }

    /// Whether the run has stopped for good. A terminal run holds no processes.
    ///
    /// `Escalated` is terminal because the driver stops: it parks every member
    /// and returns. There is no in-place answer path yet, so the honest reading
    /// is "this run is over, here is the question it stopped on".
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            RunState::Done | RunState::Failed | RunState::Escalated | RunState::Cancelled
        )
    }

    /// Whether the run is doing work right now.
    pub fn is_live(self) -> bool {
        matches!(
            self,
            RunState::Planning
                | RunState::AwaitingApproval
                | RunState::Running
                | RunState::Replanning
                | RunState::Integrating
                | RunState::Reflecting
        )
    }
}

/// Where one task is.
///
/// `Ready` and `Blocked` are deliberately distinct. A task whose dependencies
/// are not verified is `Blocked` and is not a candidate for a slot; a task whose
/// dependencies are verified but which has no free worker is `Ready` and is
/// first in line. Collapsing the two is how a scheduler ends up reporting
/// "waiting" for a task that can never start.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskState {
    /// The default, because a task the lead has just written has not started.
    #[default]
    Blocked,
    Ready,
    Running,
    Verifying,
    Verified,
    Failed,
    /// The merge of this task's branch conflicted. The run pauses.
    Conflict,
}

impl TaskState {
    pub fn label(self) -> &'static str {
        match self {
            TaskState::Blocked => "blocked",
            TaskState::Ready => "ready",
            TaskState::Running => "running",
            TaskState::Verifying => "verifying",
            TaskState::Verified => "verified",
            TaskState::Failed => "failed",
            TaskState::Conflict => "conflict",
        }
    }

    /// Whether this task's output may be merged.
    pub fn is_verified(self) -> bool {
        matches!(self, TaskState::Verified)
    }

    /// Whether this task's work is on its branch, verdict or no verdict.
    ///
    /// `Verifying` counts, and that is the whole of the speculative start: a
    /// worker posts its evidence and stops, so from that moment the commit
    /// exists and a dependent can build on it. Waiting for the verdict put
    /// verification between every link of a chain — t1 work, t1 verify, t2 work,
    /// t2 verify — and the observed five-link run spent most of its wall clock
    /// waiting rather than working.
    ///
    /// `Running` does not count. A worker that is still working has no commit to
    /// build on, and a dependent that started on one would be building on the
    /// run's base while believing it had its dependency's files.
    pub fn is_committed(self) -> bool {
        matches!(self, TaskState::Verifying | TaskState::Verified)
    }

    /// Whether this task occupies a worker slot.
    pub fn holds_worker(self) -> bool {
        matches!(self, TaskState::Running)
    }

    /// Whether this task occupies a verifier slot.
    pub fn holds_verifier(self) -> bool {
        matches!(self, TaskState::Verifying)
    }
}

/// What a member is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Role {
    /// Plans, integrates, audits. Owns the run's acceptance criterion.
    Lead,
    /// Implements one task in its own worktree.
    Worker,
    /// Tries to break one task's change. Never sees the worker's transcript.
    Verifier,
    /// Reviews the finished run and writes what a future one should know.
    ///
    /// A role rather than a post-processing step: the pass reads the board and
    /// the repository, which is a session, and a session is a member.
    Reflector,
}

impl Role {
    pub fn label(self) -> &'static str {
        match self {
            Role::Lead => "lead",
            Role::Worker => "worker",
            Role::Verifier => "verifier",
            Role::Reflector => "reflector",
        }
    }
}

/// Where one member is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MemberState {
    /// Waiting for a slot: the member exists on the roster and has no process.
    ///
    /// The state a member is created in, and the one that makes the roster
    /// countable. Without it every member a plan creates read `starting` from
    /// the moment the plan was accepted, so a live run showed twenty-one
    /// members starting while three `vibe-acp` processes existed — the roster
    /// was not lying about a state, it was reporting a state that did not exist.
    Queued,
    /// The process is launching: it exists, the session is not open yet.
    Starting,
    /// The turn is over: the process is alive, nothing is being spent, and the
    /// member is not expected to say anything until it is asked.
    ///
    /// Distinct from [`MemberState::Parked`], which is the engine's deliberate
    /// suspension of a member it will wake again — a parked member is holding a
    /// session it is not spending on, and the panel says so because waking it
    /// costs the context re-send. An idle member is simply between turns: a
    /// worker whose evidence has landed and is waiting for a verdict is idle, not
    /// working, and not parked.
    Idle,
    Working,
    /// Suspended: the process is alive, the turn is over, nothing is being
    /// spent. Waking it costs the context re-send, which is why the panel says
    /// so rather than claiming the wait was free.
    Parked,
    /// Blocked on an approval or an elicitation.
    Waiting,
    Done,
    Failed,
    /// The app closed while this member was live.
    Lost,
}

impl MemberState {
    pub fn label(self) -> &'static str {
        match self {
            MemberState::Queued => "queued",
            MemberState::Starting => "starting",
            MemberState::Idle => "idle",
            MemberState::Working => "working",
            MemberState::Parked => "parked",
            MemberState::Waiting => "waiting",
            MemberState::Done => "done",
            MemberState::Failed => "failed",
            MemberState::Lost => "lost",
        }
    }

    /// Whether this member is part of the live run: it holds a turn, or is
    /// between turns with a process still alive.
    ///
    /// `Idle` counts. Its turn is over, but its process is not: a stop or a dead
    /// driver has to park or lose it like any other, and leaving it `idle` on a
    /// cancelled run would be the roster claiming a process that has just been
    /// killed. A member blocked on a request counts for the same reason — it is
    /// not spending this second, but it is holding a turn open and will spend the
    /// moment the answer lands.
    ///
    /// `Queued` does not: it has no process at all. `Parked` does not either,
    /// because the engine has already said what it is — suspended, holding a
    /// session it is not spending on — and a second park would only move its
    /// `parked_at` forward.
    pub fn is_live(self) -> bool {
        matches!(
            self,
            MemberState::Starting
                | MemberState::Working
                | MemberState::Waiting
                | MemberState::Idle
        )
    }

    /// Whether a turn is in flight, or about to be.
    ///
    /// Narrower than [`MemberState::is_live`] by exactly `Idle`, and the
    /// difference is the question: this one asks whether the member is expected
    /// to say something, which an idle member is not.
    pub fn is_turning(self) -> bool {
        matches!(
            self,
            MemberState::Starting | MemberState::Working | MemberState::Waiting
        )
    }
}

/* ------------------------------------------------------------------ *
 * The run document
 * ------------------------------------------------------------------ */

/// One task in the plan.
///
/// Two halves, and the split is load-bearing. The first six fields are the
/// *plan*: what the lead writes. Everything after them is the *run*: what the
/// engine records as it works. The run half is `#[serde(default)]` because a
/// lead cannot know it — and without that, `read_plan` failed to parse every plan
/// a real lead ever wrote, silently, and the run sat in `planning` until its
/// timeout. A plan is the lead's document; the engine fills in the rest.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub id: String,
    pub title: String,
    /// What to do, in the worker's own brief.
    pub detail: String,
    /// The files this task expects to touch. The plan is refused when two
    /// tasks that are not ordered by the DAG claim the same file.
    pub files: Vec<String>,
    /// Task ids that must be `Verified` before this one may start.
    pub deps: Vec<String>,
    /// The observable criterion a verifier tests. Not "it works" — a command,
    /// or a behaviour with a stated input and output.
    pub acceptance: String,
    #[serde(default)]
    pub state: TaskState,
    /// The member implementing it.
    #[serde(default)]
    pub owner: Option<String>,
    /// The member verifying it.
    #[serde(default)]
    pub verifier: Option<String>,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default)]
    pub verdict: Option<Verdict>,
    #[serde(default)]
    pub branch: Option<String>,
    /// The commit the verifier's verdict is earned on, pinned when the task is
    /// handed over and asserted unchanged when the verdict arrives.
    ///
    /// Integration merges this, not the branch. A branch is a pointer anything
    /// with a shell can move: the audited run's v-t2 committed the worker's
    /// missing files as `e18875b` and repointed `swarm/…-t2` at it *after*
    /// returning `passed: false`, so the tip integration would have merged was a
    /// tree no verdict ever judged. `None` on a run adopted from disk before
    /// this field existed, which is why integration falls back to the branch.
    #[serde(default)]
    pub verified_commit: Option<String>,
    /// The commit this task's work started from: the run's base, with every
    /// dependency already merged in.
    ///
    /// Not the run's base commit. A task that depends on t1 must be able to run
    /// its own acceptance criterion, and that criterion almost always needs t1's
    /// files; branching from the base makes a dependent task's test unrunnable.
    /// The first live run showed the cost: two workers merged their dependency
    /// by hand and a third failed its verifier twice for not doing so.
    ///
    /// It is also the base side of the failing-first proof. Proving a dependent
    /// task against the run's base would credit the dependency's files to the
    /// task — the test would fail on base only because the dependency is absent.
    #[serde(default)]
    pub base_commit: Option<String>,
    #[serde(default)]
    pub worktree: Option<String>,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub ended_at: Option<String>,
    /// Why this task could not start, when its dependencies would not merge.
    ///
    /// A dependency conflict is not the task's fault and not the worker's: two
    /// branches that both verified still disagree about a file, and only the
    /// lead can decide which one owns it. The twelfth live run failed on exactly
    /// this — t1 and t2 both verified, t3 could not merge t2's branch over t1's
    /// on `src/player.js`, and the run died with "no task can make progress"
    /// while the lead sat parked with amendment budget unspent.
    #[serde(default)]
    pub conflict: Option<String>,
    /// How many times the lead has been asked to resolve a conflict on this
    /// task. The ceiling is what stops a lead that keeps re-planning the same
    /// collision from looping the run.
    #[serde(default)]
    pub conflict_repairs: u32,
    /// Files this task's worker committed that the plan did not grant it.
    ///
    /// Computed by the engine from the branch, not reported by the worker: a
    /// grant that nothing checks is a suggestion, and the twelfth live run's t1
    /// committed four files outside its three-file grant, which is what made t3
    /// unstartable. The verifier reads this list and fails the change for it.
    #[serde(default)]
    pub out_of_grant: Vec<String>,
    /// What the branch actually holds, recorded when the worker handed it over.
    ///
    /// `state: working` is not evidence of progress. The audited run's eight
    /// worktrees reported `working` for thirty minutes while five of them held
    /// nothing but `README.md` and `.git`, and the only way to tell was to open
    /// them by hand afterwards.
    #[serde(default)]
    pub files_changed: usize,
    /// Non-merge commits the worker made on its own branch.
    #[serde(default)]
    pub commits: u32,
    /// When the last of those commits was written.
    #[serde(default)]
    pub last_commit_at: Option<String>,
    /// Whether the engine has already said this task is stalled.
    ///
    /// One note per task, not one per tick: a run that posted a warning every
    /// 400ms would bury the board it is trying to explain.
    #[serde(default)]
    pub stalled: bool,
    /// Files this task owns that its dependencies disagreed about.
    ///
    /// The engine resolved the merge in favour of the incoming dependency, so
    /// the tree starts from that version and making it right is this task's
    /// work. Without the field the worker sees a file that already looks
    /// finished and leaves it alone.
    #[serde(default)]
    pub reconcile: Vec<String>,
    /// The skills this task's worker is told to load before it writes.
    ///
    /// Named by the lead in the plan, or filled by the engine from the ranking
    /// when the lead named none. Validated against the installed catalogue: a
    /// name the machine does not have is a violation, because a brief that tells
    /// a worker to load a skill that does not exist is a brief that wastes the
    /// worker's first turn discovering it.
    #[serde(default)]
    pub skills: Vec<String>,
    /// The commit each dependency contributed to this task's base.
    ///
    /// Keyed by dependency task id, valued by the commit that dependency's branch
    /// pointed at when this task's worktree was prepared. It is the record that
    /// makes a speculative start reversible: a dependent may begin while its
    /// dependency is still `Verifying`, and if that verdict fails, this map is
    /// what says which tasks were built on the work that was just refused.
    ///
    /// A commit rather than a flag, because the re-queued task has to build on
    /// the dependency's *new* tip. The map is cleared when the task is
    /// invalidated, so a stale commit cannot be mistaken for a current one.
    #[serde(default)]
    pub built_on: BTreeMap<String, String>,
}

impl Task {
    pub fn new(
        id: impl Into<String>,
        title: impl Into<String>,
        detail: impl Into<String>,
        acceptance: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            detail: detail.into(),
            files: Vec::new(),
            deps: Vec::new(),
            acceptance: acceptance.into(),
            state: TaskState::Blocked,
            owner: None,
            verifier: None,
            attempts: 0,
            verdict: None,
            branch: None,
            verified_commit: None,
            base_commit: None,
            worktree: None,
            started_at: None,
            ended_at: None,
            conflict: None,
            conflict_repairs: 0,
            out_of_grant: Vec::new(),
            files_changed: 0,
            commits: 0,
            last_commit_at: None,
            stalled: false,
            reconcile: Vec::new(),
            skills: Vec::new(),
            built_on: BTreeMap::new(),
        }
    }

    pub fn with_files(mut self, files: &[&str]) -> Self {
        self.files = files.iter().map(|f| f.to_string()).collect();
        self
    }

    pub fn with_deps(mut self, deps: &[&str]) -> Self {
        self.deps = deps.iter().map(|d| d.to_string()).collect();
        self
    }
}

/// A verifier's ruling on one task.
///
/// `failing_first` is the field that makes this adversarial rather than
/// approving: a verifier that never saw the change fail has not tested it, it
/// has read it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Verdict {
    pub passed: bool,
    pub reason: String,
    /// The commands run and the output line that settled each one.
    pub evidence: Vec<String>,
    /// The one command the verifier named as its failing-first test.
    ///
    /// Named rather than inferred from `evidence`, because the engine runs it
    /// and a command it cannot find is a command it cannot run.
    #[serde(default)]
    pub command: Option<String>,
    /// What that command exited with on the base checkout.
    ///
    /// Non-zero is the point: a test that passes without the change is not
    /// testing the change. `None` means it was never run.
    #[serde(default)]
    pub base_exit: Option<i32>,
    /// What it exited with on the change. Zero is the point.
    #[serde(default)]
    pub change_exit: Option<i32>,
    /// Whether the engine observed the command fail on the base and pass on the
    /// change. Observed, not claimed: the verifier's own words are in `reason`.
    pub failing_first: bool,
    /// Whether the verifier wrote to the base checkout. "Read-only" is not
    /// enforceable, so it is detected instead.
    pub base_dirtied: bool,
    pub at: String,
}

impl Verdict {
    /// A verdict only counts when it is both passing and earned.
    pub fn is_earned(&self) -> bool {
        self.passed && self.failing_first && !self.base_dirtied
    }
}

/// What one member has spent, and how long it spent it.
///
/// The agent reports *session-cumulative* counters, not per-turn deltas: the
/// second turn of a session reports the first turn's tokens again. Summing the
/// readings would multiply the cost by the number of turns, so [`Usage::observe`]
/// turns each reading into a delta against the last one. A reading that goes
/// down is a new session — a retry spawns a fresh member with a fresh session —
/// and the whole reading is the delta.
///
/// `cached` is the number that says whether a run is cheap. A cached input token
/// costs a fraction of a fresh one, so a run with a high cache rate is doing the
/// same work for less, and a run with a low one is paying full price to re-read
/// the same files.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    /// Input tokens, summed over every turn of every session this member ran.
    pub input: u64,
    /// Output tokens, likewise.
    pub output: u64,
    /// Input tokens the provider served from its prompt cache.
    pub cached: u64,
    /// Turns that reported usage.
    pub turns: u32,
    /// The context window in use at the last reading, and its size.
    pub context_used: u64,
    pub context_size: u64,
    /// Seconds spent inside turns, summed. Wall clock, not model time.
    pub active_secs: f64,
    /// The last cumulative reading per counter, so the next one can be a delta.
    /// Persisted: a restart mid-run must not re-count the session from zero.
    #[serde(default)]
    pub last_input: u64,
    #[serde(default)]
    pub last_output: u64,
    #[serde(default)]
    pub last_cached: u64,
}

impl Usage {
    /// Fold one cumulative reading into the totals.
    pub fn observe(&mut self, input: u64, output: u64, cached: u64) {
        fn delta(now: u64, last: u64) -> u64 {
            if now >= last {
                now - last
            } else {
                // A new session. Its counters start at zero, so the whole
                // reading is new work rather than a correction.
                now
            }
        }
        self.input = self.input.saturating_add(delta(input, self.last_input));
        self.output = self.output.saturating_add(delta(output, self.last_output));
        self.cached = self.cached.saturating_add(delta(cached, self.last_cached));
        self.last_input = input;
        self.last_output = output;
        self.last_cached = cached;
    }

    /// Record the live context gauge. Not a cost: it is how full the window is.
    pub fn observe_context(&mut self, used: u64, size: u64) {
        self.context_used = used;
        self.context_size = size;
    }

    /// Count a finished turn and the wall clock it took.
    pub fn finish_turn(&mut self, secs: f64) {
        self.turns = self.turns.saturating_add(1);
        if secs.is_finite() && secs > 0.0 {
            self.active_secs += secs;
        }
    }

    /// Tokens in and out, the number a bill is computed from.
    pub fn total(&self) -> u64 {
        self.input.saturating_add(self.output)
    }

    /// The share of input tokens that came from cache, 0.0 to 1.0.
    pub fn cache_rate(&self) -> f64 {
        if self.input == 0 {
            return 0.0;
        }
        (self.cached as f64 / self.input as f64).clamp(0.0, 1.0)
    }

    /// Whether anything has been spent yet.
    pub fn is_empty(&self) -> bool {
        self.input == 0 && self.output == 0 && self.turns == 0
    }

    /// Add another member's usage into this one.
    pub fn merge(&mut self, other: &Usage) {
        self.input = self.input.saturating_add(other.input);
        self.output = self.output.saturating_add(other.output);
        self.cached = self.cached.saturating_add(other.cached);
        self.turns = self.turns.saturating_add(other.turns);
        self.active_secs += other.active_secs;
        self.context_used = self.context_used.max(other.context_used);
        self.context_size = self.context_size.max(other.context_size);
    }
}

/// One role's share of a run: how long its turns took and what they cost.
///
/// Stamped on the run when it stops, rather than derived by the reader from
/// `members`. The roster is the live record and the drive loop folds each
/// member's spend onto it; a run read back from disk after its processes are gone
/// still carries those numbers, but nothing recomputes them, and a reader that
/// summed `members` would be a second answer to a question the run has already
/// answered. The plan review is that reader: it shows a per-role median across
/// past runs, and a median over a field that only exists while a run is live is a
/// median over nothing.
///
/// The observed run is why this exists. Its verifier model took 20.7 minutes
/// against the worker's 6, and nothing anywhere said so — a user choosing a
/// verifier model should see that number before the run rather than after it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoleUsage {
    pub role: Role,
    /// Turns this role took, summed over its members.
    pub turns: u32,
    /// Seconds spent inside those turns, summed.
    pub secs: f64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    /// Input tokens the provider served from its prompt cache, a subset of
    /// `tokens_in`.
    ///
    /// Recorded because it is the difference between the number the budget is
    /// charged for and the number a turn moved through the context window: the
    /// audited run's worker reported 39,151,829 input tokens of which 38,344,704
    /// were cache reads, so a median of `tokens_in` would plan the next run at
    /// 94.7% noise. Runs recorded before this field existed read as zero cached
    /// and are therefore indistinguishable from uncached ones; that is a floor on
    /// the fresh figure, not a claim that nothing was cached.
    #[serde(default)]
    pub cached: u64,
}

impl RoleUsage {
    /// Seconds one turn of this role took, or `None` when it took none.
    ///
    /// Per turn rather than per run, because that is the unit a user is choosing
    /// between: a verifier model that takes 20 minutes a turn is a different
    /// decision from one that takes 6, whatever the run's total.
    pub fn turn_secs(&self) -> Option<u64> {
        if self.turns == 0 {
            return None;
        }
        Some((self.secs / self.turns as f64).round().max(0.0) as u64)
    }

    /// Tokens one turn of this role moved, in and out, cache reads included.
    ///
    /// Diagnostics only. This is the figure the audited run's status payload
    /// showed as if it were spend, and 94.7% of it was a cache hit.
    pub fn turn_tokens(&self) -> Option<u64> {
        if self.turns == 0 {
            return None;
        }
        Some(
            self.tokens_in
                .saturating_add(self.tokens_out)
                .saturating_div(self.turns as u64),
        )
    }

    /// Tokens one turn of this role was charged for, in and out.
    ///
    /// Cache reads are subtracted because they are a subset of `tokens_in` and
    /// bill at a fraction of it. This is the figure a next-run estimate is
    /// derived from: a plan built on [`Self::turn_tokens`] would forecast the
    /// cache hit, not the work.
    pub fn turn_fresh_tokens(&self) -> Option<u64> {
        if self.turns == 0 {
            return None;
        }
        let fresh = self
            .tokens_in
            .saturating_sub(self.cached.min(self.tokens_in));
        Some(
            fresh
                .saturating_add(self.tokens_out)
                .saturating_div(self.turns as u64),
        )
    }
}

/// What one line of a member's feed is.
///
/// The four kinds are the four notifications the watch already reads, named the
/// way the document names everything else. A feed that only carried tool calls
/// would answer "what did it run" and not "is it doing anything", which is the
/// question a member that has been quiet for four minutes raises.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ActivityKind {
    /// A tool call was announced.
    ToolCall,
    /// A tool call reported a new status.
    ToolUpdate,
    /// The member said something.
    Message,
    /// The gate is holding, or has answered, a request for it.
    Permission,
}

/// One thing a member did, as its own stream reported it.
///
/// Observed, not reported: every field here comes from a notification the member
/// emitted, which is the same evidence the skill ledger is built from. A member
/// that claims in its transcript to be running the suite and one that is running
/// it look identical from the board; they do not look identical here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityItem {
    /// When it happened, in the clock the panel reads.
    pub at: String,
    pub kind: ActivityKind,
    /// The tool call's name, the member's opening words, or what was asked for.
    pub title: String,
    /// `in-progress`, `completed`, `failed`, `said`, `pending` or `answered`.
    pub status: String,
    /// The tool call this line is about, when it is about one.
    ///
    /// Kept so the pane can tell a finished call from a running one without
    /// matching on titles, which are not unique — a member that runs `bash`
    /// eleven times has eleven lines called `bash`.
    #[serde(default)]
    pub id: Option<String>,
    /// Tokens this member had spent when the line was recorded.
    ///
    /// A reading per line, not a total. The difference between two lines over
    /// the time between them is a rate, and a rate is what says whether a member
    /// is doing anything; the total says only what it has cost, and the roster
    /// already shows that.
    pub tokens: u64,
}

/// What a member had spent at one moment of its current turn.
///
/// A reading, not a total. The difference between two readings over the time
/// between them is a rate, and a rate is what says whether a member is doing
/// anything; a cumulative total says only what it has cost, and the roster
/// already shows that.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Reading {
    /// When the reading was taken, in the clock the panel reads.
    pub at: String,
    /// Tokens this member had spent by then, both directions.
    pub tokens: u64,
}

/// What one member is doing, and — for the selected member — what it just did.
///
/// The split is the whole design. `current` is one item and every member carries
/// it, because the roster's question is "what is each of them doing" and a state
/// word cannot answer it. `recent` is two hundred items and only the selected
/// member carries it, because the panel polls every two seconds and a
/// twenty-one member run would otherwise carry four thousand items per poll for
/// a pane that can only ever show one member.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Activity {
    /// The tool call running right now, if any.
    #[serde(default)]
    pub current: Option<ActivityItem>,
    /// The feed, newest first. Empty for every member but the selected one.
    #[serde(default)]
    pub recent: Vec<ActivityItem>,
    /// The token readings of the current turn, oldest first.
    ///
    /// `recent` is the selected member's alone, so a roster card has no series to
    /// draw a rate from — and a card is where the question "is this one doing
    /// anything" is asked, twenty-one times at once. This is that series, and it
    /// is bounded at [`RATE_KEPT`] because it is drawn forty pixels wide.
    ///
    /// Empty between turns, deliberately. A parked member has no turn, so it has
    /// no series, so its card draws a flat line — which is the honest answer.
    /// Keeping the last turn's readings would draw a busy sparkline for a member
    /// that has spent nothing for ten minutes.
    #[serde(default)]
    pub series: Vec<Reading>,
}

impl Activity {
    /// Whether there is nothing to say about this member.
    ///
    /// Used to leave the field out of the document entirely. `run.json` is the
    /// record of what a run did, and the feed is live data that is never saved —
    /// so a field that is always empty in the record is a field a reader has to
    /// check before trusting. A member with no watch, which is a member that
    /// never started, has nothing to say and says nothing.
    pub fn is_empty(&self) -> bool {
        self.current.is_none() && self.recent.is_empty() && self.series.is_empty()
    }
}

/// One turn a member ran, from the moment it was asked for to the moment it
/// ended.
///
/// The timeline draws a member's turns as spans on a wall clock, and
/// `turn_started_at` cannot supply them: it is cleared when a turn ends, so a
/// finished run has no record of when a member's turns ran at all. This is that
/// record, and it is the only one — nothing else in the document says a verifier
/// spent twenty minutes where its worker spent six.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnSpan {
    /// When the turn was asked for.
    pub start: String,
    /// When the watch stopped reporting it.
    pub end: String,
}

/// One member of the run.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Member {
    pub id: String,
    pub role: Role,
    pub model: String,
    /// The reasoning level the session was switched to, when one matched.
    ///
    /// Recorded rather than requested, for the same reason `mode` is: the agent
    /// validates the level against the model, and a level that is legal for one
    /// model is refused by another. A roster that showed the level the engine
    /// asked for would describe the intent, not the run.
    #[serde(default)]
    pub thinking: Option<String>,
    /// The agent mode the session was switched to, when one matched the role.
    /// The agent this member runs as, as the session reported it —
    /// `vibedesk-swarm-worker`, not a permission mode.
    ///
    /// `vibe-acp` 3.1.4 exposes two separate config options: `mode` is the
    /// permission mode (`ask`, `accept-edits`, `auto-approve`) and `agent` is the
    /// agent the session runs. The engine used to set the role on `mode`, so a
    /// real run failed at the first spawn with "the session does not offer it …
    /// Offered: ask, accept-edits, auto-approve". Old run files wrote the field as
    /// `mode`, so that name is an alias rather than a break.
    #[serde(default, alias = "mode")]
    pub agent: Option<String>,
    pub state: MemberState,
    pub pid: Option<u32>,
    pub session_id: Option<String>,
    pub worktree: Option<String>,
    /// The worktree HEAD the engine last saw.
    ///
    /// `last_at` only moves when a member writes to the board, so a worker that
    /// spends ten minutes reading and then commits looks exactly like one that
    /// died. The worktree is the second signal: a HEAD that moved is work, and
    /// the engine records it here so the same commit is not counted twice.
    #[serde(default)]
    pub head: Option<String>,
    /// When the worktree HEAD was last read.
    ///
    /// Reading it costs a `git rev-parse`, which is 46ms on this machine, and the
    /// drive loop ticks every 400ms. Probing on every tick meant a member that
    /// was quietly reading files for five minutes spent most of the loop's budget
    /// spawning git. The probe is a liveness check, not a hot path: once every
    /// `HEAD_PROBE_SECS` is enough to notice a commit and cheap enough to ignore.
    #[serde(default)]
    pub head_at: Option<String>,
    /// The task this member is on, when it is on one.
    pub task: Option<String>,
    pub turns: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    /// Input tokens served from the provider's prompt cache.
    #[serde(default)]
    pub cached_tokens: u64,
    /// Restarts charged because the agent process was gone, not because the model
    /// was slow.
    ///
    /// The audited run could not tell these apart: a worker doing a long think and
    /// a worker whose process had died both arrived as "restarted it on t3". They
    /// are different problems. A dead process is not going to answer however long
    /// it is given, and restarting it repeatedly is how a run spends its budget
    /// learning nothing.
    #[serde(default)]
    pub restarts_infra: u32,
    /// Restarts charged because the member was silent past
    /// [`super::engine::MEMBER_IDLE_LIMIT_SECS`].
    #[serde(default)]
    pub restarts_timeout: u32,
    /// Restarts charged because a provider or transport refused the call.
    #[serde(default)]
    pub restarts_provider: u32,
    /// Seconds this member spent inside turns.
    #[serde(default)]
    pub active_secs: f64,
    /// The context window in use at the last reading, and its size.
    #[serde(default)]
    pub context_used: u64,
    #[serde(default)]
    pub context_size: u64,
    pub parked_at: Option<String>,
    /// When the current turn was asked for, and `None` between turns.
    ///
    /// The panel needs a live elapsed time for a member that is working, and the
    /// only honest source for it is the moment the turn began. Deriving it from
    /// `last_at` would be a second clock measuring a different thing: `last_at`
    /// moves when the member writes to the board, which is not when it started.
    ///
    /// Cleared at the end of the turn, so a member that has stopped working does
    /// not go on reporting a turn that is over.
    #[serde(default)]
    pub turn_started_at: Option<String>,
    /// The turns this member has finished, oldest first.
    ///
    /// Bounded at [`SPANS_KEPT`], and the newest are kept: a member's recent
    /// turns are the ones that explain its current state, and the row is read
    /// against the current moment rather than against the start of the run.
    ///
    /// Appended by the drive loop when the watch stops reporting a turn, because
    /// the loop is the only object that saves the run — a span written from the
    /// watch would be overwritten by the next tick's snapshot.
    #[serde(default)]
    pub spans: Vec<TurnSpan>,
    pub last_at: Option<String>,
    /// Why it failed, when it did.
    pub error: Option<String>,
    /// How many times the engine has asked this member to finish its turn.
    ///
    /// A turn can end with nothing written — a provider that drops its tool
    /// calls, a model that answers in prose — and the run then waits for a file
    /// that is never coming. The budget is per member and on disk, because a
    /// nudge that resets on restart is not a budget.
    #[serde(default)]
    pub nudges: u32,
    /// How many times the engine has restarted this member's process.
    ///
    /// A stalled turn ends with the process killed, and the worktree survives it.
    /// Restarting is the recovery; the ceiling is what stops a member that dies
    /// on spawn from looping.
    #[serde(default)]
    pub respawns: u32,
    /// The skills this member was told to load, in the order the brief named them.
    #[serde(default)]
    pub skills_assigned: Vec<String>,
    /// The skills this member's own tool calls loaded.
    ///
    /// Observed, not reported. The engine reads the member's `tool_call`
    /// notifications, so this is a fact about the session rather than a claim in
    /// a transcript — which is the only way "the run used its skills" can be
    /// checked instead of believed.
    #[serde(default)]
    pub skills_loaded: Vec<String>,
    /// What this member is doing right now, and what it just did.
    ///
    /// Filled by `Engine::snapshot` from the member's watch, never saved: the
    /// ring is live data, and a `run.json` carrying two hundred items per member
    /// would be a record of what each member was doing at the moment of the last
    /// save rather than of what it did. The snapshot is the only reader, so it is
    /// the only place this belongs — and it is left out of the document entirely
    /// when there is nothing to say, which is the case for every member on disk.
    #[serde(default, skip_serializing_if = "Activity::is_empty")]
    pub activity: Activity,
}

impl Member {
    pub fn new(id: impl Into<String>, role: Role, model: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            role,
            model: model.into(),
            thinking: None,
            agent: None,
            // Queued, not starting. A member is created when the plan is
            // accepted and spawned when the scheduler has a slot for it, and the
            // gap between the two is most of the roster for most of the run.
            state: MemberState::Queued,
            pid: None,
            session_id: None,
            worktree: None,
            head: None,
            head_at: None,
            task: None,
            turns: 0,
            tokens_in: 0,
            tokens_out: 0,
            cached_tokens: 0,
            restarts_infra: 0,
            restarts_timeout: 0,
            restarts_provider: 0,
            active_secs: 0.0,
            context_used: 0,
            context_size: 0,
            parked_at: None,
            turn_started_at: None,
            spans: Vec::new(),
            last_at: None,
            error: None,
            nudges: 0,
            respawns: 0,
            skills_assigned: Vec::new(),
            skills_loaded: Vec::new(),
            activity: Activity::default(),
        }
    }

    /// Record a finished turn, keeping the ring bounded.
    ///
    /// The oldest goes when the ring is full, so the row a reader sees is the
    /// member's recent work rather than the first twelve turns of a long run.
    pub fn push_span(&mut self, start: String, end: String) {
        if self.spans.len() >= SPANS_KEPT {
            self.spans.remove(0);
        }
        self.spans.push(TurnSpan { start, end });
    }

    /// Tokens this member has spent, both directions.
    pub fn tokens(&self) -> u64 {
        self.tokens_in.saturating_add(self.tokens_out)
    }

    /// This member's usage, in the shape the ledger accumulates.
    pub fn usage(&self) -> Usage {
        Usage {
            input: self.tokens_in,
            output: self.tokens_out,
            cached: self.cached_tokens,
            turns: self.turns.min(u32::MAX as u64) as u32,
            context_used: self.context_used,
            context_size: self.context_size,
            active_secs: self.active_secs,
            last_input: 0,
            last_output: 0,
            last_cached: 0,
        }
    }

    /// Copy a ledger reading onto this member's record.
    pub fn set_usage(&mut self, usage: &Usage) {
        self.tokens_in = usage.input;
        self.tokens_out = usage.output;
        self.cached_tokens = usage.cached;
        self.turns = usage.turns as u64;
        self.active_secs = usage.active_secs;
        self.context_used = usage.context_used;
        self.context_size = usage.context_size;
    }
}

/// The run's token and wall-clock ceiling.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Budget {
    pub max_tokens: u64,
    pub used_tokens: u64,
    /// Input tokens served from cache across the whole run.
    #[serde(default)]
    pub cached_tokens: u64,
    /// Uncached input plus output, which is what a ceiling should count.
    ///
    /// Computed by [`Run::recompute_spend`] and carried here so no reader has to
    /// subtract by hand. The audited run was read as "56M against a 25M cap"
    /// because the payload offered only the traffic figure — 94.7% of which was
    /// cache reads — and the reader had nothing to subtract.
    #[serde(default)]
    pub spent_tokens: u64,
    /// What this run has cost, in millionths of a dollar, priced at
    /// [`rates::RATES`].
    ///
    /// `None` when any member's model is missing from the table: a run that cannot
    /// be priced must not report a number, because zero reads as free and a partial
    /// sum reads as the bill. The models that stopped it being answered are named
    /// in `unpriced_models`.
    #[serde(default)]
    pub spend_micros: Option<u64>,
    /// The ceiling in money, when the caller set one. Zero or absent means the
    /// token ceiling is the only one.
    #[serde(default)]
    pub max_spend_micros: Option<u64>,
    /// The share of the money ceiling spent, or `None` when the run cannot be
    /// priced. Never a token ratio: that number used to live here and read as money.
    #[serde(default)]
    pub spend_pct: Option<f64>,
    /// The share of the token ceiling spent, counting the quantity `exceeded()`
    /// compares so the percentage and the stop condition cannot disagree.
    #[serde(default)]
    pub token_pct: f64,
    /// The share of input tokens the provider served from its cache, 0..1.
    #[serde(default)]
    pub hit_rate: f64,
    /// The models this run used that [`rates::RATES`] cannot price.
    #[serde(default)]
    pub unpriced_models: Vec<String>,
    /// The rates version the spend was computed with.
    #[serde(default)]
    pub rates_version: u32,
    pub max_wall_secs: u64,
    pub started_at: Option<String>,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            max_tokens: 2_000_000,
            used_tokens: 0,
            cached_tokens: 0,
            spent_tokens: 0,
            spend_micros: None,
            max_spend_micros: None,
            spend_pct: None,
            token_pct: 0.0,
            hit_rate: 0.0,
            unpriced_models: Vec::new(),
            rates_version: rates::RATES_VERSION,
            max_wall_secs: 60 * 60,
            started_at: None,
        }
    }
}

/// Uncached input plus output: the tokens a ceiling should count.
///
/// `used_tokens` is every token that crossed the wire, and most of them are cache
/// reads — a member re-sends its whole context every turn — so counting traffic
/// against a ceiling reported a five-cent run as a blown budget.
pub fn billed_tokens(used: u64, cached: u64) -> u64 {
    used.saturating_sub(cached)
}

impl Budget {
    /// Whether the run has spent its allowance.
    ///
    /// A budget that counts only workers is a budget that lies, so `used_tokens`
    /// is the sum over every member including the lead and the reflection pass.
    ///
    /// Two ceilings, because they answer different questions. The token ceiling is
    /// a proxy that costs nothing to check; the money ceiling is the real one and
    /// only applies when the caller set it and the models are priced. A run whose
    /// models cannot be priced is not "free" — it is unbounded in money, which is
    /// why `unpriced_models` is reported rather than defaulted.
    pub fn exceeded(&self) -> bool {
        if self.max_tokens > 0 && self.spent_tokens >= self.max_tokens {
            return true;
        }
        match (self.max_spend_micros, self.spend_micros) {
            (Some(ceiling), Some(spent)) => ceiling > 0 && spent >= ceiling,
            _ => false,
        }
    }

    /// Seconds since the run started, or zero before it has.
    pub fn elapsed_secs(&self) -> u64 {
        let Some(started) = self.started_at.as_deref() else {
            return 0;
        };
        let Ok(start) = chrono::DateTime::parse_from_rfc3339(started) else {
            return 0;
        };
        let now = chrono::Utc::now();
        (now - start.with_timezone(&chrono::Utc))
            .num_seconds()
            .max(0) as u64
    }

    pub fn out_of_time(&self) -> bool {
        self.max_wall_secs > 0 && self.elapsed_secs() >= self.max_wall_secs
    }
}

/// Where the integration branch is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IntegrationState {
    Pending,
    Merging,
    Merged,
    Conflict,
    /// The integrated suite failed after every task passed its own verifier.
    Failed,
}

impl IntegrationState {
    pub fn label(self) -> &'static str {
        match self {
            IntegrationState::Pending => "pending",
            IntegrationState::Merging => "merging",
            IntegrationState::Merged => "merged",
            IntegrationState::Conflict => "conflict",
            IntegrationState::Failed => "failed",
        }
    }
}

/// A merge that could not be applied cleanly.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Conflict {
    pub task: String,
    pub files: Vec<String>,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Integration {
    pub branch: String,
    pub base: String,
    pub state: IntegrationState,
    /// Task ids merged so far, in merge order.
    pub merged: Vec<String>,
    pub conflict: Option<Conflict>,
    /// The command that proves the integrated result, and its outcome.
    pub suite: Option<String>,
    pub suite_passed: Option<bool>,
    /// How many repair rounds the integrated suite has cost.
    ///
    /// Every task green and the combination red is the failure a swarm is most
    /// likely to produce, and it used to be terminal: the suite ran, failed, and
    /// the run died with the work merged and nothing left to do about it. A
    /// repair round appends a task that starts from the merged tree and owns the
    /// suite as its criterion, so the same machinery that fixed the parts fixes
    /// the whole. The count is on the run because a ceiling that resets is not a
    /// ceiling.
    #[serde(default)]
    pub repairs: u32,
}

impl Integration {
    pub fn new(branch: impl Into<String>, base: impl Into<String>) -> Self {
        Self {
            branch: branch.into(),
            base: base.into(),
            state: IntegrationState::Pending,
            merged: Vec::new(),
            conflict: None,
            suite: None,
            suite_passed: None,
            repairs: 0,
        }
    }
}

/// One repo gotcha the reflection pass found.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Gotcha {
    pub file: String,
    pub text: String,
    /// What backs it: a verdict, a conflict, an escalation. A gotcha with no
    /// source is not written.
    pub source: String,
}

/// The post-run pass.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Reflection {
    pub at: String,
    pub summary: String,
    pub gotchas: Vec<Gotcha>,
    /// Set when the pass ran in a degraded mode — no session logs, so it read
    /// the board only.
    pub degraded: Option<String>,
}

/// A precondition, computed before a run starts.
///
/// The six capabilities carry `ready`/`blocked` and render the reason in the
/// danger colour. The swarm's preconditions are the same shape, and they are
/// computed when the workstation opens rather than discovered at run start,
/// because that is the only point at which a refusal is cheap.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Precondition {
    pub key: String,
    pub label: String,
    pub ready: bool,
    /// Why it is not ready. Empty when it is.
    pub detail: String,
}

impl Precondition {
    pub fn ready(key: &str, label: &str) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            ready: true,
            detail: String::new(),
        }
    }

    pub fn blocked(key: &str, label: &str, detail: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            ready: false,
            detail: detail.into(),
        }
    }
}

/// The whole run, as it is persisted.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    pub id: String,
    /// The user's prompt, verbatim. The lead's brief starts from this.
    pub goal: String,
    pub workspace: String,
    pub state: RunState,
    pub created_at: String,
    pub updated_at: String,
    pub lead: Member,
    pub members: Vec<Member>,
    pub tasks: Vec<Task>,
    pub budget: Budget,
    pub integration: Integration,
    /// How many tasks may hold a worker slot at once.
    ///
    /// Stored on the run rather than passed to the loop, because the panel shows
    /// it and a number the user set must be the number the scheduler used.
    #[serde(default = "default_concurrency")]
    pub concurrency: usize,
    /// How many attempts a task gets before it is failed. See
    /// [`default_max_attempts`].
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
    pub reflection: Option<Reflection>,
    /// The named failure, when `state` is `Failed`.
    pub failure: Option<String>,
    /// Whether this run was a rehearsal: scheduled and costed, nothing spawned.
    pub rehearsal: bool,
    /// How many times the lead has revised a plan the gate refused.
    ///
    /// On the run, not in the loop, because the loop is restarted by `approve`
    /// and by a resumed window, and a revision budget that resets is not a budget.
    #[serde(default)]
    pub plan_attempts: u32,
    /// How many times the lead has amended the plan mid-run.
    ///
    /// A worker that breaks a file no task owns is right to stop, and the lead is
    /// the one who can answer it: the file belongs to a task, and the plan simply
    /// did not say so. That is a coordination question, not a human one, so the
    /// run asks the lead first. The budget is what stops a lead that answers
    /// badly from looping: after `MAX_AMENDMENTS` the question goes to the user.
    #[serde(default)]
    pub amendments: u32,
    /// When the run reached a terminal state.
    ///
    /// `updated_at` moves on every save, so it cannot answer "how long did this
    /// take". This is stamped once, when the run stops, and never again.
    #[serde(default)]
    pub ended_at: Option<String>,
    /// The skill catalogue this run planned against.
    ///
    /// Stored on the run rather than recomputed by the panel, because the panel
    /// must show what the run actually used. A catalogue re-read later would
    /// drift as skills are installed, and the ledger would describe a run that
    /// did not happen. It is also the authority the plan's `skills` names are
    /// validated against, so the lead is never refused for naming something it
    /// was shown.
    #[serde(default)]
    pub skills: Vec<skills::Skill>,
    /// Whether this run was handed the machine's method at all.
    ///
    /// Recorded rather than re-read, because the switch can be flipped while a
    /// run is in flight and the panel would then describe a run that did not
    /// happen. It is also what lets the panel tell the two empty cases apart: a
    /// run with no skills because the switch was off, and a run with no skills
    /// because the machine has none installed. Those are different problems and
    /// the same empty list.
    #[serde(default = "on_by_default")]
    pub skills_enabled: bool,
    /// The skills that looked relevant to the goal, in rank order, with the
    /// terms that matched.
    ///
    /// The lead is given the whole catalogue; this is what the panel highlights
    /// and what the ledger reports as the run's method.
    #[serde(default)]
    pub skill_shortlist: Vec<skills::Match>,
    /// Skills assigned to a task that no member ever loaded.
    ///
    /// Computed from the members' own tool calls, so it is a fact about the run.
    /// A gap is not a failure — a task can be small enough to need no method —
    /// but it is the difference between a run that used its skills and a run that
    /// was handed them.
    #[serde(default)]
    pub skill_gaps: Vec<String>,
    /// Skills the reflection pass decided this machine is missing.
    ///
    /// Written into the run's own directory. Nothing reaches the user's skill
    /// store without them accepting it: a run that could write there could change
    /// how every future run behaves.
    #[serde(default)]
    pub skill_proposals: Vec<skills::Proposal>,
    /// Why this plan is accepted but should not be.
    ///
    /// Not a violation, and deliberately so: a violation refuses the plan and
    /// sends it back to the lead, and a lead cannot make a genuinely sequential
    /// plan parallel without lying about its dependencies. The shape is a warning
    /// the plan is accepted with, and the plan review shows it beside the approve
    /// button — which is the approve-anyway, and the decision is the user's.
    ///
    /// On the run rather than recomputed by the panel, because the panel must
    /// show what the gate saw. A plan re-shaped later would describe a run that
    /// did not happen.
    #[serde(default)]
    pub shape_warnings: Vec<plan::ShapeWarning>,
    /// What each role cost, stamped once when the run stops.
    ///
    /// See [`RoleUsage`] for why this is recorded rather than derived. Empty on a
    /// run that is still going, and on a run written before this field existed.
    #[serde(default)]
    pub role_usage: Vec<RoleUsage>,
}

impl Run {
    /// A fresh run, in `Planning`, with only the lead.
    pub fn new(goal: impl Into<String>, workspace: impl Into<String>, lead: Member) -> Self {
        let at = now();
        let id = new_run_id();
        Self {
            integration: Integration::new(format!("swarm/{id}"), "HEAD"),
            id,
            goal: goal.into(),
            workspace: workspace.into(),
            state: RunState::Planning,
            created_at: at.clone(),
            updated_at: at,
            lead,
            members: Vec::new(),
            tasks: Vec::new(),
            budget: Budget::default(),
            concurrency: default_concurrency(),
            max_attempts: default_max_attempts(),
            reflection: None,
            failure: None,
            rehearsal: false,
            plan_attempts: 0,
            amendments: 0,
            ended_at: None,
            skills: Vec::new(),
            skills_enabled: true,
            skill_shortlist: Vec::new(),
            skill_gaps: Vec::new(),
            skill_proposals: Vec::new(),
            shape_warnings: Vec::new(),
            role_usage: Vec::new(),
        }
    }

    /// Every member, lead first.
    pub fn roster(&self) -> impl Iterator<Item = &Member> {
        std::iter::once(&self.lead).chain(self.members.iter())
    }

    pub fn member(&self, id: &str) -> Option<&Member> {
        self.roster().find(|m| m.id == id)
    }

    pub fn member_mut(&mut self, id: &str) -> Option<&mut Member> {
        if self.lead.id == id {
            return Some(&mut self.lead);
        }
        self.members.iter_mut().find(|m| m.id == id)
    }

    pub fn task(&self, id: &str) -> Option<&Task> {
        self.tasks.iter().find(|t| t.id == id)
    }

    pub fn task_mut(&mut self, id: &str) -> Option<&mut Task> {
        self.tasks.iter_mut().find(|t| t.id == id)
    }

    /// Recompute the run's token total from its members.
    ///
    /// Derived rather than accumulated: a member that is cancelled mid-turn
    /// must not leave its spend uncounted, and a single source of truth cannot
    /// drift from the roster the panel shows.
    ///
    /// Everything a reader needs is computed here, because the payload that
    /// carried only `usedTokens` and `maxTokens` is how a run at 17.9% of its
    /// ceiling was read as a blowout. Traffic, the billed part of it, what that
    /// part cost, the share the cache served, and the percentage of whichever
    /// ceiling actually bounds the run.
    pub fn recompute_spend(&mut self) {
        self.budget.used_tokens = self.roster().map(Member::tokens).sum();
        self.budget.cached_tokens = self.roster().map(|m| m.cached_tokens).sum();
        self.budget.spent_tokens =
            billed_tokens(self.budget.used_tokens, self.budget.cached_tokens);
        self.budget.hit_rate = if self.budget.used_tokens > 0 {
            self.budget.cached_tokens as f64 / self.budget.used_tokens as f64
        } else {
            0.0
        };

        // Priced per member: the lead and the workers can be on different models,
        // and one rate for the run would be a guess about which.
        let members: Vec<(String, u64, u64, u64)> = self
            .roster()
            .map(|m| (m.model.clone(), m.tokens_in, m.cached_tokens, m.tokens_out))
            .collect();
        let mut micros: u64 = 0;
        let mut priced = 0usize;
        let mut unpriced: Vec<String> = Vec::new();
        for (model, input, cached, output) in members {
            match rates::price(&model, input, cached, output) {
                Some(cost) => {
                    micros = micros.saturating_add(cost);
                    priced += 1;
                }
                None => {
                    let model = model.trim().to_string();
                    if !model.is_empty() && !unpriced.contains(&model) {
                        unpriced.push(model);
                    }
                }
            }
        }
        self.budget.unpriced_models = unpriced.clone();
        // `None`, not zero, and `None` whenever any member could not be priced:
        // a partial sum is a floor, and a floor presented as "the spend" is the
        // same lie as zero. The models that stopped it being answered are named,
        // so the fix is a row in `rates::RATES` rather than a guess.
        self.budget.spend_micros = (priced > 0 && unpriced.is_empty()).then_some(micros);
        self.budget.rates_version = rates::RATES_VERSION;
        self.budget.spend_pct = match (self.budget.max_spend_micros, self.budget.spend_micros) {
            (Some(ceiling), Some(spent)) if ceiling > 0 => {
                Some(spent as f64 / ceiling as f64 * 100.0)
            }
            _ => None,
        };
        self.budget.token_pct = if self.budget.max_tokens > 0 {
            self.budget.spent_tokens as f64 / self.budget.max_tokens as f64 * 100.0
        } else {
            0.0
        };
    }

    /// The whole run's usage, every member summed.
    pub fn usage(&self) -> Usage {
        let mut total = Usage::default();
        for member in self.roster() {
            total.merge(&member.usage());
        }
        total
    }

    /// Seconds the run has been alive: to `ended_at` once it has stopped, to
    /// now while it is still going.
    pub fn elapsed_secs(&self) -> u64 {
        let Ok(start) = chrono::DateTime::parse_from_rfc3339(&self.created_at) else {
            return 0;
        };
        let start = start.with_timezone(&chrono::Utc);
        let end = self
            .ended_at
            .as_deref()
            .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
            .map(|at| at.with_timezone(&chrono::Utc))
            .unwrap_or_else(chrono::Utc::now);
        (end - start).num_seconds().max(0) as u64
    }

    /// Stamp the end of the run, once.
    ///
    /// Idempotent on purpose: the drive loop reaches a terminal state from
    /// several branches, and the first one to arrive is the one that counts.
    ///
    /// The per-role record is stamped here for the same reason the end time is:
    /// there are eleven places the loop reaches a terminal state, and a branch
    /// added later cannot forget a stamp that lives in the one function they all
    /// go through.
    pub fn finish(&mut self) {
        if self.ended_at.is_none() {
            self.ended_at = Some(now());
            self.stamp_role_usage();
        }
    }

    /// Record what each role cost, from the roster as it stands.
    ///
    /// A role with no turns is left out rather than recorded as zero: a run whose
    /// reflection never ran has no reflector, and a zero would enter the median
    /// as a measurement of a turn that did not happen.
    ///
    /// The roster is as fresh as the tick that reached the terminal state, which
    /// is the same reading `budget.used_tokens` is computed from — the drive loop
    /// folds each member's ledger at the top of a tick and the terminal branches
    /// work from that snapshot. A turn that ended inside the last 400 ms is
    /// therefore not in the stamp, which is a rounding error against a turn
    /// measured in minutes and is the price of not reordering the loop.
    fn stamp_role_usage(&mut self) {
        let mut out: Vec<RoleUsage> = Vec::new();
        for role in [Role::Lead, Role::Worker, Role::Verifier, Role::Reflector] {
            let mut usage = RoleUsage {
                role,
                turns: 0,
                secs: 0.0,
                tokens_in: 0,
                tokens_out: 0,
                cached: 0,
            };
            let mut any = false;
            for member in self.roster().filter(|member| member.role == role) {
                if member.turns == 0 {
                    continue;
                }
                any = true;
                usage.turns = usage
                    .turns
                    .saturating_add(member.turns.min(u32::MAX as u64) as u32);
                usage.secs += member.active_secs;
                usage.tokens_in = usage.tokens_in.saturating_add(member.tokens_in);
                usage.tokens_out = usage.tokens_out.saturating_add(member.tokens_out);
                usage.cached = usage.cached.saturating_add(member.cached_tokens);
            }
            if any {
                out.push(usage);
            }
        }
        self.role_usage = out;
    }

    /// Whether every task is verified.
    pub fn all_verified(&self) -> bool {
        !self.tasks.is_empty() && self.tasks.iter().all(|t| t.state.is_verified())
    }

    pub fn touch(&mut self) {
        self.updated_at = now();
    }

    /* -------------------------------------------------------------- *
     * Persistence
     * -------------------------------------------------------------- */

    /// The run's directory under the app data dir.
    pub fn dir(&self) -> PathBuf {
        run_dir(&self.id)
    }

    pub fn save(&self) -> Result<(), String> {
        save_at(&self.dir(), self)
    }

    pub fn load(id: &str) -> Result<Run, String> {
        load_at(&run_dir(id))
    }
}

/// Where a run lives. Checked, because the id becomes a path component.
/// The default for a field that was added after runs existed on disk.
///
/// A run written before the skill plane was handed no method, but it also had no
/// switch to turn off — so `true` is the reading that does not accuse an old run
/// of a decision nobody made. The panel only uses this to choose a sentence.
fn on_by_default() -> bool {
    true
}

pub fn run_dir(id: &str) -> PathBuf {
    crate::paths::app_data_dir().join("swarm").join(id)
}

pub fn save_at(dir: &Path, run: &Run) -> Result<(), String> {
    std::fs::create_dir_all(dir)
        .map_err(|error| format!("could not create {}: {error}", dir.display()))?;
    // A run cannot be written in a terminal state without an end time. Stamping
    // here rather than at each of the eleven places the loop reaches a terminal
    // state means the clock cannot be forgotten by a branch added later, and
    // `finish` is idempotent so the first branch to arrive is the one that
    // counts.
    let mut run = run.clone();
    if run.state.is_terminal() {
        run.finish();
    }
    let body = serde_json::to_string_pretty(&run)
        .map_err(|error| format!("could not encode the run: {error}"))?;
    // Write-then-rename: a crash mid-write must not leave a half run that the
    // panel then renders as a real one.
    let temp = dir.join("run.json.tmp");
    std::fs::write(&temp, body)
        .map_err(|error| format!("could not write {}: {error}", temp.display()))?;
    std::fs::rename(&temp, dir.join("run.json"))
        .map_err(|error| format!("could not replace run.json: {error}"))
}

pub fn load_at(dir: &Path) -> Result<Run, String> {
    let path = dir.join("run.json");
    let body = std::fs::read_to_string(&path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    let mut run: Run = serde_json::from_str(&body)
        .map_err(|error| format!("{} is not a run: {error}", path.display()))?;
    // The derived figures are recomputed on load. A run written before the spend
    // fields existed carries zeros for them, and a zero spend reads as a free run
    // — which is the mistake the whole money plane exists to stop making.
    run.recompute_spend();
    Ok(run)
}

/// One run, by id, or nothing when the id names no run.
///
/// Validated before it is used as a path: a run id arrives from the webview, and
/// a `..` in it would read a file outside the swarm directory.
pub fn load_run(id: &str) -> Option<Run> {
    if !valid_run_id(id) {
        return None;
    }
    load_at(&run_dir(id)).ok()
}

/// Every run on disk, newest first.
////// A directory that does not parse is skipped rather than failing the list: one
/// corrupt run must not hide the others.
pub fn list_runs() -> Vec<Run> {
    let root = crate::paths::app_data_dir().join("swarm");
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut runs: Vec<Run> = entries
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| {
            let id = entry.file_name().to_string_lossy().to_string();
            if !valid_run_id(&id) {
                return None;
            }
            load_at(&entry.path()).ok()
        })
        .collect();
    runs.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    runs.truncate(RUN_HISTORY);
    runs
}

/// Delete a run and everything it owns.
///
/// The member session directories go with it, because a run owns its members'
/// traces the same way it owns their worktrees. The caller shows the count and
/// the size first — this function does not ask.
pub fn delete_run(id: &str) -> Result<(), String> {
    if !valid_run_id(id) {
        return Err(format!("`{id}` is not a run id"));
    }
    let dir = run_dir(id);
    if dir.is_dir() {
        std::fs::remove_dir_all(&dir)
            .map_err(|error| format!("could not remove {}: {error}", dir.display()))?;
    }
    Ok(())
}

/* ------------------------------------------------------------------ *
 * The snapshot the panel renders
 * ------------------------------------------------------------------ */

/// Everything the swarm panel needs, in one round trip.
///
/// One command rather than six, for the same reason the capability ledger is
/// one: a panel assembled from six round trips can disagree with itself, and
/// this panel's whole job is to be the single true account of a run.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmSnapshot {
    /// The live run, when there is one.
    pub run: Option<Run>,
    /// The board tail for the live run.
    pub board: Vec<board::BoardEntry>,
    /// Preconditions, computed now.
    pub preconditions: Vec<Precondition>,
    /// Whether all six capabilities are on. The panel entry is dim without it.
    pub capabilities_all_on: bool,
    /// How many capabilities are on, and how many there are.
    pub capabilities_on: usize,
    pub capabilities_total: usize,
    /// Finished runs, newest first, without their boards.
    pub history: Vec<RunSummary>,
    /// Whether a run is live right now.
    pub live: bool,
    /// Whether the live run is driven by another process.
    ///
    /// A run outlives the window that started it, so the panel can be showing a
    /// run this process cannot stop or approve. Saying so is the difference
    /// between a viewer and a lie.
    pub external: bool,
}

/// A finished run, as the history list shows it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunSummary {
    pub id: String,
    pub goal: String,
    pub state: RunState,
    pub created_at: String,
    pub tasks: usize,
    pub verified: usize,
    /// Every token that crossed the wire, cache reads included.
    pub tokens: u64,
    /// The part of `tokens` the provider served from cache.
    pub cached_tokens: u64,
    /// The part of `tokens` the run was billed for.
    pub spent_tokens: u64,
    /// What that cost in µusd, and absent when a model in the run is unpriced.
    pub spend_micros: Option<u64>,
    pub rehearsal: bool,
}

impl From<&Run> for RunSummary {
    fn from(run: &Run) -> Self {
        Self {
            id: run.id.clone(),
            goal: run.goal.clone(),
            state: run.state,
            created_at: run.created_at.clone(),
            tasks: run.tasks.len(),
            verified: run.tasks.iter().filter(|t| t.state.is_verified()).count(),
            tokens: run.budget.used_tokens,
            cached_tokens: run.budget.cached_tokens,
            spent_tokens: run.budget.spent_tokens,
            spend_micros: run.budget.spend_micros,
            rehearsal: run.rehearsal,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lead() -> Member {
        Member::new("lead", Role::Lead, "mistral-large")
    }

    /* -------------------------------------------------------------- *
     * The roster's state word
     * -------------------------------------------------------------- */

    /// Every state answers both questions, and the two differ in exactly one
    /// place: an idle member is part of the live run but has no turn in flight.
    ///
    /// The distinction is load-bearing. `is_live` is what stop, interrupt and
    /// escalate ask — is there a process here to park or lose — and an idle
    /// member's process is still alive, so leaving it `idle` on a cancelled run
    /// would be the roster claiming a process that has just been killed.
    /// `is_turning` is what the reflection timeout asks — is this pass still
    /// running — and an idle reflector has stopped running.
    #[test]
    fn a_state_says_whether_it_is_live_and_whether_it_is_turning() {
        let expected: [(MemberState, bool, bool); 9] = [
            (MemberState::Queued, false, false),
            (MemberState::Starting, true, true),
            (MemberState::Idle, true, false),
            (MemberState::Working, true, true),
            (MemberState::Parked, false, false),
            (MemberState::Waiting, true, true),
            (MemberState::Done, false, false),
            (MemberState::Failed, false, false),
            (MemberState::Lost, false, false),
        ];
        for (state, live, turning) in expected {
            assert_eq!(state.is_live(), live, "{} is_live", state.label());
            assert_eq!(state.is_turning(), turning, "{} is_turning", state.label());
        }
    }

    /// A member created for a plan has no process and no turn.
    #[test]
    fn a_new_member_is_queued() {
        assert_eq!(lead().state, MemberState::Queued);
        assert_eq!(MemberState::Queued.label(), "queued");
    }

    /* -------------------------------------------------------------- *
     * Usage: the agent reports cumulative counters, not deltas
     * -------------------------------------------------------------- */

    #[test]
    fn a_cumulative_reading_is_counted_once_not_once_per_turn() {
        // The live agent's second turn reports the first turn's tokens again:
        // turn 1 was 13069 in / 166 out, turn 2 reported 26312 / 169. Summing
        // the readings would bill 39381 input tokens for 26312 of work.
        let mut usage = Usage::default();
        usage.observe(13069, 166, 12800);
        usage.observe(26312, 169, 25984);
        assert_eq!(usage.input, 26312);
        assert_eq!(usage.output, 169);
        assert_eq!(usage.cached, 25984);
    }

    #[test]
    fn a_reading_that_goes_down_is_a_new_session() {
        // A retry spawns a fresh member with a fresh session, whose counters
        // start at zero. The whole new reading is new work, not a correction.
        let mut usage = Usage::default();
        usage.observe(13069, 166, 12800);
        usage.observe(900, 40, 0);
        assert_eq!(usage.input, 13069 + 900);
        assert_eq!(usage.output, 166 + 40);
        assert_eq!(usage.cached, 12800);
    }

    #[test]
    fn a_repeated_reading_adds_nothing() {
        // The agent emits the same snapshot several times per turn.
        let mut usage = Usage::default();
        usage.observe(1000, 50, 800);
        usage.observe(1000, 50, 800);
        usage.observe(1000, 50, 800);
        assert_eq!(usage.input, 1000);
        assert_eq!(usage.output, 50);
    }

    #[test]
    fn the_zero_reading_at_session_start_does_not_reset_the_baseline() {
        // A new session opens with an all-zero snapshot before any work. That
        // must not be read as "the counters went down", or the first real
        // reading of the new session would be counted twice.
        let mut usage = Usage::default();
        usage.observe(5000, 100, 4000);
        usage.observe(0, 0, 0);
        usage.observe(700, 20, 0);
        assert_eq!(usage.input, 5700);
        assert_eq!(usage.output, 120);
    }

    #[test]
    fn the_cache_rate_is_the_share_of_input_that_was_cached() {
        let mut usage = Usage::default();
        usage.observe(1000, 10, 750);
        assert!((usage.cache_rate() - 0.75).abs() < 1e-9);
    }

    #[test]
    fn a_run_with_no_input_has_a_zero_cache_rate_and_does_not_divide_by_zero() {
        let usage = Usage::default();
        assert_eq!(usage.cache_rate(), 0.0);
        assert!(usage.is_empty());
    }

    #[test]
    fn a_cache_rate_cannot_exceed_one() {
        // A provider that reports more cached tokens than input tokens is
        // reporting nonsense; the panel must not render 140%.
        let mut usage = Usage::default();
        usage.observe(100, 10, 400);
        assert_eq!(usage.cache_rate(), 1.0);
    }

    #[test]
    fn a_turn_is_counted_with_its_wall_clock() {
        let mut usage = Usage::default();
        usage.finish_turn(12.5);
        usage.finish_turn(7.5);
        assert_eq!(usage.turns, 2);
        assert!((usage.active_secs - 20.0).abs() < 1e-9);
    }

    #[test]
    fn a_nonsense_turn_duration_is_ignored_rather_than_added() {
        let mut usage = Usage::default();
        usage.finish_turn(f64::NAN);
        usage.finish_turn(-4.0);
        assert_eq!(usage.active_secs, 0.0);
        assert_eq!(usage.turns, 2, "the turn still happened");
    }

    #[test]
    fn merging_members_sums_the_counters_and_takes_the_widest_context() {
        let mut a = Usage::default();
        a.observe(100, 10, 50);
        a.observe_context(900, 1000);
        let mut b = Usage::default();
        b.observe(200, 20, 150);
        b.observe_context(4000, 8000);
        a.merge(&b);
        assert_eq!(a.input, 300);
        assert_eq!(a.output, 30);
        assert_eq!(a.cached, 200);
        assert_eq!(a.context_used, 4000);
        assert_eq!(a.context_size, 8000);
    }

    #[test]
    fn a_members_usage_round_trips_through_its_record() {
        let mut member = lead();
        let mut usage = Usage::default();
        usage.observe(1234, 56, 1000);
        usage.finish_turn(9.0);
        usage.observe_context(1234, 200000);
        member.set_usage(&usage);
        assert_eq!(member.tokens_in, 1234);
        assert_eq!(member.tokens_out, 56);
        assert_eq!(member.cached_tokens, 1000);
        assert_eq!(member.turns, 1);
        assert_eq!(member.context_size, 200000);
        assert_eq!(member.usage().input, 1234);
        assert_eq!(member.usage().cached, 1000);
    }

    #[test]
    fn the_run_total_is_the_sum_of_its_roster() {
        let mut run = Run::new("goal", "D:/ws", lead());
        let mut worker = Member::new("w-t1", Role::Worker, "m");
        worker.tokens_in = 100;
        worker.tokens_out = 10;
        worker.cached_tokens = 60;
        run.members.push(worker);
        run.lead.tokens_in = 50;
        run.lead.tokens_out = 5;
        run.recompute_spend();
        assert_eq!(run.budget.used_tokens, 165);
        assert_eq!(run.budget.cached_tokens, 60);
        assert_eq!(run.usage().input, 150);
    }

    /// The payload carries the spend, the share the cache served, and the models
    /// it could not price — all three, because a reader given only `usedTokens`
    /// read 56M of traffic against a 25M ceiling and called 17.9% of a cap a
    /// blowout.
    #[test]
    fn a_recomputed_run_reports_what_it_spent_and_what_it_could_not_price() {
        let mut run = Run::new("goal", "D:/ws", Member::new("lead", Role::Lead, "mistral-large"));
        let mut worker = Member::new("w-t1", Role::Worker, "mistral-large");
        worker.tokens_in = 1_000_000;
        worker.cached_tokens = 900_000;
        worker.tokens_out = 10_000;
        run.members.push(worker);
        run.lead.tokens_in = 500_000;
        run.lead.tokens_out = 5_000;
        run.recompute_spend();

        // 1.5M in, 0.9M of it cached, 15k out
        assert_eq!(run.budget.used_tokens, 1_515_000);
        assert_eq!(run.budget.cached_tokens, 900_000);
        assert_eq!(run.budget.spent_tokens, 615_000);
        assert!((run.budget.hit_rate - 900_000.0 / 1_515_000.0).abs() < 1e-9);
        // 600k fresh at $2/M + 900k cached at $0.20/M + 15k out at $6/M
        // = 1,200,000 + 180,000 + 90,000 µusd
        assert_eq!(run.budget.spend_micros, Some(1_470_000));
        assert!(run.budget.unpriced_models.is_empty());
        assert_eq!(run.budget.rates_version, rates::RATES_VERSION);
        // No money ceiling was set, so there is no money percentage at all; the
        // token percentage is the one that exists, and it counts billed tokens.
        assert_eq!(run.budget.spend_pct, None);
        assert!((run.budget.token_pct - 615_000.0 / 2_000_000.0 * 100.0).abs() < 1e-9);

        // With a ceiling and a price, the money percentage is the answer and it
        // is 1,470,000 of 2,940,000 µusd.
        run.budget.max_spend_micros = Some(2_940_000);
        run.recompute_spend();
        assert_eq!(run.budget.spend_micros, Some(1_470_000));
        assert_eq!(run.budget.spend_pct, Some(50.0));

        // A member on a model this table does not price makes the money absent
        // rather than zero, and names the model.
        let mut local = Member::new("w-t2", Role::Worker, "some-local-gguf");
        local.tokens_in = 1_000;
        run.members.push(local);
        run.recompute_spend();
        assert_eq!(run.budget.spend_micros, None);
        assert_eq!(
            run.budget.spend_pct, None,
            "a run that cannot be priced has no money percentage"
        );
        assert_eq!(run.budget.unpriced_models, vec!["some-local-gguf".to_string()]);
    }

    /// A run on priced models reports money, priced per member: the rehearsal's
    /// lead and workers ran on `deepseek` while its verifiers ran on
    /// `mistral-large`, and one rate for the run would be a guess about which.
    #[test]
    fn a_run_on_priced_models_reports_money_rather_than_unknown() {
        let mut run = Run::new("g", "D:/ws", Member::new("lead", Role::Lead, "deepseek"));
        let mut worker = Member::new("w-t1", Role::Worker, "deepseek");
        worker.tokens_in = 1_000_000;
        worker.cached_tokens = 900_000;
        worker.tokens_out = 10_000;
        let mut verifier = Member::new("v-t1", Role::Verifier, "mistral-large-4-0-3");
        verifier.tokens_in = 1_000_000;
        verifier.cached_tokens = 900_000;
        verifier.tokens_out = 10_000;
        run.members = vec![worker, verifier];
        run.budget.max_spend_micros = Some(1_500_000);
        run.recompute_spend();

        // worker on deepseek: 100k fresh at $0.30/M + 900k cached at $0.007/M
        // + 10k out at $1.20/M = 30,000 + 6,300 + 12,000
        // verifier on mistral-large: 200,000 + 180,000 + 60,000
        assert_eq!(
            run.budget.spend_micros,
            Some(48_300 + 440_000),
            "unpriced: {:?}",
            run.budget.unpriced_models
        );
        assert!(run.budget.unpriced_models.is_empty());
        let pct = run.budget.spend_pct.expect("a priced run has a money percentage");
        assert!((pct - 488_300.0 / 1_500_000.0 * 100.0).abs() < 1e-9, "{pct}");
    }

    #[test]
    fn a_run_that_has_stopped_reports_a_frozen_elapsed_time() {
        let mut run = Run::new("goal", "D:/ws", lead());
        run.created_at = "2026-11-18T20:00:00.000Z".into();
        run.ended_at = Some("2026-11-18T20:12:30.000Z".into());
        assert_eq!(run.elapsed_secs(), 750);
    }

    #[test]
    fn finishing_a_run_twice_keeps_the_first_end_time() {
        // The drive loop reaches a terminal state from several branches; the
        // first one to arrive is the one that counts.
        let mut run = Run::new("goal", "D:/ws", lead());
        run.ended_at = Some("2026-11-18T20:12:30.000Z".into());
        run.finish();
        assert_eq!(run.ended_at.as_deref(), Some("2026-11-18T20:12:30.000Z"));
    }

    #[test]
    fn a_run_with_no_end_time_is_still_counting() {
        let mut run = Run::new("goal", "D:/ws", lead());
        run.created_at = chrono::Utc::now().to_rfc3339();
        assert!(run.ended_at.is_none());
        assert!(run.elapsed_secs() < 5);
    }

    #[test]
    fn a_run_id_is_a_safe_path_component() {
        let id = new_run_id();
        assert!(valid_run_id(&id), "{id}");
        assert!(!valid_run_id("../etc"));
        assert!(!valid_run_id(""));
        assert!(!valid_run_id("a/b"));
    }

    #[test]
    fn two_runs_started_in_the_same_second_get_different_ids() {
        let a = new_run_id();
        let b = new_run_id();
        assert_ne!(a, b);
    }

    #[test]
    fn a_fresh_run_is_planning_and_holds_only_the_lead() {
        let run = Run::new("do the thing", "/tmp/ws", lead());
        assert_eq!(run.state, RunState::Planning);
        assert!(run.state.is_live());
        assert!(!run.state.is_terminal());
        assert_eq!(run.roster().count(), 1);
        assert!(run.tasks.is_empty());
        assert!(!run.all_verified(), "an empty run is not verified");
    }

    #[test]
    fn the_lead_is_reachable_through_the_roster_helpers() {
        let mut run = Run::new("g", "/tmp", lead());
        run.members.push(Member::new("w1", Role::Worker, "small"));
        assert_eq!(run.roster().count(), 2);
        assert_eq!(run.member("lead").unwrap().role, Role::Lead);
        assert_eq!(run.member("w1").unwrap().role, Role::Worker);
        assert!(run.member("nobody").is_none());

        run.member_mut("w1").unwrap().turns = 3;
        assert_eq!(run.member("w1").unwrap().turns, 3);
        run.member_mut("lead").unwrap().turns = 1;
        assert_eq!(run.lead.turns, 1);
    }

    #[test]
    fn spend_is_derived_from_the_roster_including_the_lead() {
        let mut run = Run::new("g", "/tmp", lead());
        run.lead.tokens_in = 100;
        run.lead.tokens_out = 50;
        run.members.push(Member::new("w1", Role::Worker, "small"));
        run.members[0].tokens_in = 10;
        run.members[0].tokens_out = 5;
        run.recompute_spend();
        assert_eq!(run.budget.used_tokens, 165);
    }

    #[test]
    fn a_budget_is_exceeded_at_the_ceiling_not_past_it() {
        let mut budget = Budget {
            max_tokens: 100,
            ..Default::default()
        };
        budget.used_tokens = 99;
        budget.spent_tokens = 99;
        assert!(!budget.exceeded());
        budget.used_tokens = 100;
        budget.spent_tokens = 100;
        assert!(budget.exceeded(), "the ceiling is inclusive");
    }

    /// A run that is mostly cache reads is cheap, and the ceiling must say so.
    ///
    /// A member re-sends its whole context every turn, so traffic and spend are
    /// different numbers by an order of magnitude. The live run that exposed this
    /// crossed 4.4M tokens against a 4M ceiling with 96% of them cached — five
    /// cents of work — and the run was killed for it.
    #[test]
    fn a_run_that_is_mostly_cache_reads_is_not_over_budget() {
        let budget = Budget {
            max_tokens: 4_000_000,
            used_tokens: 4_406_635,
            cached_tokens: 4_230_370,
            spent_tokens: billed_tokens(4_406_635, 4_230_370),
            ..Default::default()
        };
        assert_eq!(budget.spent_tokens, 176_265);
        assert!(
            !budget.exceeded(),
            "176k billable tokens is not a blown 4M ceiling"
        );
    }

    /// The ceiling still bites when the spend is real.
    #[test]
    fn a_run_that_spends_its_ceiling_in_fresh_tokens_is_over_budget() {
        let budget = Budget {
            max_tokens: 100,
            used_tokens: 100,
            cached_tokens: 0,
            spent_tokens: 100,
            ..Default::default()
        };
        assert!(budget.exceeded());
    }

    /// Cache reads cannot make the spend negative.
    ///
    /// A provider that reports more cached tokens than input tokens would
    /// otherwise underflow and hand the run an enormous allowance.
    #[test]
    fn a_cache_figure_larger_than_the_traffic_does_not_underflow() {
        let budget = Budget {
            max_tokens: 100,
            used_tokens: 10,
            cached_tokens: 50,
            spent_tokens: billed_tokens(10, 50),
            ..Default::default()
        };
        assert_eq!(budget.spent_tokens, 0);
        assert!(!budget.exceeded());
    }

    /// A money ceiling is checked when the models are priced, and a run this
    /// build cannot price is unbounded in money rather than free.
    ///
    /// `spend_micros` is `None` for an unpriced model on purpose: the fallback
    /// rate was the alternative, and a guessed price is worse than a missing one
    /// because it looks answered.
    #[test]
    fn a_money_ceiling_is_checked_only_for_a_priced_run() {
        let priced = Budget {
            max_tokens: 0,
            max_spend_micros: Some(1_000_000),
            spend_micros: Some(1_200_000),
            ..Default::default()
        };
        assert!(priced.exceeded(), "$1.20 spent against a $1.00 ceiling");

        let unpriced = Budget {
            max_tokens: 0,
            max_spend_micros: Some(1_000_000),
            spend_micros: None,
            ..Default::default()
        };
        assert!(
            !unpriced.exceeded(),
            "an unpriced run is unbounded, not free — it stops on the token ceiling"
        );
    }

    #[test]
    fn a_zero_budget_means_unlimited_rather_than_instantly_exceeded() {        let budget = Budget {
            max_tokens: 0,
            max_wall_secs: 0,
            ..Default::default()
        };
        assert!(!budget.exceeded());
        assert!(!budget.out_of_time());
    }

    #[test]
    fn elapsed_time_is_zero_before_the_run_starts() {
        let budget = Budget::default();
        assert_eq!(budget.elapsed_secs(), 0);
        assert!(!budget.out_of_time());
    }

    #[test]
    fn a_verdict_only_counts_when_it_is_earned() {
        let base = Verdict {
            passed: true,
            reason: "ok".into(),
            evidence: vec!["cargo test → ok".into()],
            command: Some("cargo test".into()),
            base_exit: Some(1),
            change_exit: Some(0),
            failing_first: true,
            base_dirtied: false,
            at: now(),
        };
        assert!(base.is_earned());

        let approving = Verdict {
            failing_first: false,
            ..base.clone()
        };
        assert!(!approving.is_earned(), "a verdict that never saw a failure is a read");

        let dirty = Verdict {
            base_dirtied: true,
            ..base.clone()
        };
        assert!(!dirty.is_earned());

        let failing = Verdict {
            passed: false,
            ..base
        };
        assert!(!failing.is_earned());
    }

    #[test]
    fn task_states_agree_about_which_slot_they_hold() {
        assert!(TaskState::Running.holds_worker());
        assert!(!TaskState::Running.holds_verifier());
        assert!(TaskState::Verifying.holds_verifier());
        assert!(!TaskState::Verifying.holds_worker());
        // Verification overlaps implementation: a verifying task must not
        // consume a worker slot, or the two phases serialise.
        assert!(!TaskState::Verifying.holds_worker());
        assert!(TaskState::Verified.is_verified());
        assert!(!TaskState::Ready.is_verified());
    }

    #[test]
    fn a_run_round_trips_through_disk() {
        let dir = std::env::temp_dir().join(format!("vd-swarm-{}", new_run_id()));
        let mut run = Run::new("goal", "/tmp/ws", lead());
        run.tasks.push(
            Task::new("t1", "one", "do it", "cargo test passes")
                .with_files(&["src/a.rs"])
                .with_deps(&[]),
        );
        run.state = RunState::Running;
        save_at(&dir, &run).unwrap();

        let back = load_at(&dir).unwrap();
        assert_eq!(back.id, run.id);
        assert_eq!(back.state, RunState::Running);
        assert_eq!(back.tasks.len(), 1);
        assert_eq!(back.tasks[0].files, vec!["src/a.rs".to_string()]);
        assert_eq!(back.lead.role, Role::Lead);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The ceiling is on the run, and an old run file gets the new default.
    ///
    /// The engine used to pass a literal `2` to `apply_verdict`, so a task got
    /// one retry and the number was invisible to the panel. A run written
    /// before the field existed must still load, and must load with the
    /// ceiling the engine will actually use.
    #[test]
    fn a_run_carries_its_retry_ceiling_and_an_old_file_gets_the_default() {
        let run = Run::new("goal", "/tmp/ws", lead());
        assert_eq!(run.max_attempts, 3, "one retry is not a retry budget");

        let mut value = serde_json::to_value(&run).unwrap();
        value.as_object_mut().unwrap().remove("max_attempts");
        let back: Run = serde_json::from_value(value).unwrap();
        assert_eq!(back.max_attempts, default_max_attempts());
    }

    /// A run written before the turn clock existed must still load, and the panel
    /// must find it under the name it reads.
    ///
    /// `Member` is serialized into `run.json`, so a field that cannot be missing
    /// makes every run on disk unreadable — which is how `read_plan` failed
    /// silently on every plan a real lead ever wrote.
    #[test]
    fn an_old_run_file_gets_no_turn_clock_rather_than_failing_to_load() {
        let mut run = Run::new("goal", "/tmp/ws", lead());
        run.members.push(Member::new("w-t1", Role::Worker, "m"));
        run.member_mut("w-t1").unwrap().turn_started_at = Some(now());

        let mut value = serde_json::to_value(&run).unwrap();
        assert!(
            value["members"][0].get("turnStartedAt").is_some(),
            "the panel reads camelCase"
        );
        value["members"][0]
            .as_object_mut()
            .unwrap()
            .remove("turnStartedAt");
        let back: Run = serde_json::from_value(value).unwrap();
        assert_eq!(back.member("w-t1").unwrap().turn_started_at, None);
    }

    /// The feed is live data and is left out of the record.
    ///
    /// `run.json` is what a run did, and the ring is what a member is doing. The
    /// engine fills `activity` on the copy it hands the panel and never on the run
    /// it saves, so the field is absent from every file — and a file written
    /// before the field existed still loads.
    #[test]
    fn a_member_with_nothing_to_say_writes_no_activity_field() {
        let mut run = Run::new("goal", "/tmp/ws", lead());
        run.members.push(Member::new("w-t1", Role::Worker, "m"));

        let value = serde_json::to_value(&run).unwrap();
        assert!(
            value["members"][0].get("activity").is_none(),
            "an empty feed is not part of the record"
        );

        let back: Run = serde_json::from_value(value).unwrap();
        assert!(back.member("w-t1").unwrap().activity.is_empty());
    }

    /// A run written before the timeline existed must still load, and the panel
    /// must find the spans under the name it reads.
    ///
    /// `Member` is serialized into `run.json`, so a field that cannot be missing
    /// makes every run on disk unreadable. The spans are the record the timeline
    /// draws a verifier row from, and a run that predates them has none — which
    /// is a row with no bar, not a run that fails to open.
    #[test]
    fn an_old_run_file_gets_no_spans_rather_than_failing_to_load() {
        let mut run = Run::new("goal", "/tmp/ws", lead());
        run.members.push(Member::new("v-t1", Role::Verifier, "m"));
        run.member_mut("v-t1")
            .unwrap()
            .push_span("2026-01-01T00:00:00.000Z".into(), "2026-01-01T00:20:42.000Z".into());

        let mut value = serde_json::to_value(&run).unwrap();
        assert!(
            value["members"][0].get("spans").is_some(),
            "the panel reads camelCase"
        );
        value["members"][0]
            .as_object_mut()
            .unwrap()
            .remove("spans");
        let back: Run = serde_json::from_value(value).unwrap();
        assert!(back.member("v-t1").unwrap().spans.is_empty());
    }

    /// The rate series is live data and is left out of the record.
    ///
    /// Same argument as the feed: `run.json` is what a run did, and the series is
    /// what a member is spending right now. A saved one would be a snapshot of a
    /// moment presented as history, and would grow the file by forty readings per
    /// member on every save the drive loop makes.
    #[test]
    fn a_member_with_no_readings_writes_no_series_field() {
        let mut run = Run::new("goal", "/tmp/ws", lead());
        run.members.push(Member::new("w-t1", Role::Worker, "m"));

        let value = serde_json::to_value(&run).unwrap();
        assert!(
            value["members"][0].get("activity").is_none(),
            "an empty series is not part of the record"
        );

        let back: Run = serde_json::from_value(value).unwrap();
        assert!(back.member("w-t1").unwrap().activity.series.is_empty());
    }

    #[test]
    fn a_corrupt_run_file_is_an_error_rather_than_a_default() {        let dir = std::env::temp_dir().join(format!("vd-swarm-bad-{}", new_run_id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("run.json"), "{ not json").unwrap();
        assert!(load_at(&dir).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn deleting_a_run_refuses_an_id_that_is_a_path() {
        assert!(delete_run("../escape").is_err());
        assert!(delete_run("a/b").is_err());
    }

    #[test]
    fn a_summary_counts_verified_tasks() {
        let mut run = Run::new("g", "/tmp", lead());
        run.tasks.push(Task::new("t1", "a", "d", "c"));
        run.tasks.push(Task::new("t2", "b", "d", "c"));
        run.tasks[0].state = TaskState::Verified;
        let summary = RunSummary::from(&run);
        assert_eq!(summary.tasks, 2);
        assert_eq!(summary.verified, 1);
    }
}
