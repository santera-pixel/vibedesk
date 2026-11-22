//! The orchestrator.
//!
//! Substrate, not a router. The engine decides *when* a member may run — it owns
//! the worktrees, the scheduler, the budget and the board file — and it never
//! decides what a member says. Members read the board and write to it directly;
//! the engine only appends what they wrote, in arrival order.
//!
//! One `vibe-acp` process per member, not one process with N sessions. Three
//! reasons, in order of weight: it reuses [`AcpBridge`] verbatim instead of
//! refactoring the file two other agents are in; a member that crashes takes its
//! own process down and not the run; and a member's `cwd` is its worktree, which
//! is what makes `session/list` filter it out of the workstation's chat list by
//! construction.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::acp::bridge::{AcpBridge, EventSink};
use crate::capabilities::{self, CapabilityId};
use crate::config;

use super::amend;
use super::board::{self as board_file, Board, BoardEntry, BoardKind};
use super::integrate::{self, IntegrationOutcome};
use super::history;
use super::permissions;
use super::plan::{self, Plan, Violation};
use super::reflect;
use super::prove;
use super::roles::{self, DiversityPolicy, RoleModels};
use super::schedule::{self, Transition};
use super::skills::{self, Catalogue, Skill};
use super::worktree::{self, CommandGit};
use super::{
    now, Gotcha, IntegrationState, Member, MemberState, Precondition, Role, Run,
    RunState, RunSummary, SwarmSnapshot, Task, TaskState, Usage, Verdict, BOARD_TAIL,
    MAX_REPAIR_ROUNDS,
};

/// Emitted whenever the run changes, so the panel can re-read without polling
/// faster than the engine moves.
pub const EVENT_SWARM: &str = "swarm://changed";

/// How often the drive loop looks at the world.
///
/// 400 ms, the same cadence as the proposal mailbox: fast enough that a member's
/// entry appears while the user is watching, slow enough that a parked run costs
/// nothing.
const TICK: std::time::Duration = std::time::Duration::from_millis(400);

/// How long a member may sit in `Working` with no board entry before the engine
/// treats it as wedged.
const MEMBER_IDLE_LIMIT_SECS: i64 = 600;

/// How long a worker may work without producing a commit before the run says so.
///
/// Twenty minutes, because a worker is not silent while it is writing — the idle
/// limit does not see it — and the audited run's workers sat `working` for thirty
/// minutes holding nothing but `README.md` and `.git`. This is a report, not a
/// kill: the engine posts one note per task and leaves the decision to the user,
/// because a slow worker that is about to commit and a stuck one look identical
/// from here and only one of them is worth interrupting.
const STALL_SECS: i64 = 1200;

/// The reflection pass's member id.
///
/// One per run, and named rather than generated: the pass is a stage of the run,
/// not a task, so there is exactly one of it and the roster can say so.
const REFLECTOR_ID: &str = "reflect";

/// The lead's member id, which is also the id the roster and the board use.
const LEAD_ID: &str = "lead";

/// How many times the lead may revise a plan the gate refused.
///
/// One shot is not a planner, it is a lottery: a lead that forgets the
/// integration suite — the single most common omission — used to kill the run
/// outright. Two revisions is enough for a real correction and few enough that a
/// lead which cannot satisfy the gate fails the run instead of looping.
/// How many times the lead may rewrite a plan the gate refused.
///
/// Three, not two, because the criterion gate below is the one refusal a lead
/// cannot see coming: a criterion that reads perfectly well can still pass on the
/// base commit, and the first revision is where the lead learns that. Two
/// revisions is one lesson and no room to apply it.
const PLAN_ATTEMPTS: u32 = 3;

/// How long one criterion gets when the engine checks it against the base tree
/// before accepting a plan.
///
/// Shorter than `prove::PROOF_TIMEOUT` on purpose. This check runs once per task
/// while the user is waiting for a plan to be accepted, and a criterion that
/// needs more than a minute to answer "does this pass on the base" is a criterion
/// the engine will not be able to prove at verdict time either. A timeout is not
/// a violation: the engine refuses a plan it has *observed* to be unprovable, not
/// one it failed to observe.
const PLAN_PROOF_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// How long the lead may sit silent in `Planning` before the engine asks again.
///
/// A turn can end with nothing written — a provider that drops its tool calls, a
/// model that answers in prose — and the run then waits for a file that is never
/// coming. Three minutes is long enough for a real plan and short enough that the
/// user is not left watching a run that has already died.
const LEAD_NUDGE_SECS: i64 = 180;

/// How often a quiet member's worktree HEAD is read.
///
/// The probe is a `git rev-parse`, measured at 46ms on this machine, and the
/// drive loop ticks every 400ms. Probing on every tick meant four quiet workers
/// spent 184ms of every 400ms tick spawning git — 46% of the loop's budget on a
/// liveness check. Fifteen seconds still notices a commit long before the
/// three-minute nudge threshold, and costs one probe per member per fifteen
/// seconds instead of thirty-seven.
const HEAD_PROBE_SECS: i64 = 15;

/// How many times a member may be asked to finish its turn.
///
/// Two is enough for a provider that dropped one response and not enough to spin
/// on a model that will never write the file. After that the idle limit is the
/// honest answer.
const NUDGE_ATTEMPTS: u32 = 2;

/// How long a nudge waits for the member's turn to end before giving up.
///
/// A prompt cannot be delivered to a member that is mid-turn, and the retry path
/// nudges a worker whose turn was cancelled a second earlier. Five minutes is
/// longer than any turn that is making progress and shorter than the ceiling that
/// would otherwise be the only thing to notice.
const NUDGE_WAIT_SECS: u64 = 300;
const NUDGE_POLL_SECS: u64 = 2;

/// How long a turn may be in flight with no sign of life before it is a hang.
///
/// The bridge abandons a stalled request but keeps `busy` set, because the agent
/// may still be working. That is right for a chat and wrong for a swarm: a
/// provider that never answers would hold a worker slot until the wall-clock
/// budget expired. Half an hour is longer than any honest turn and short enough
/// that a hung run fails while the user is still watching it.
const MEMBER_TURN_CEILING_SECS: i64 = 1800;

/// How long a model call may say nothing at all before it is not answering.
///
/// The ceiling above is the fallback for a turn whose stream cannot be read. This
/// is the real answer, and it is much shorter because the evidence is much better:
/// a turn that has emitted no notification since it began, with no tool running,
/// is a request the provider has not answered. Every chunk of an answer — including
/// a thought — is a notification, so five minutes of total silence is not thinking,
/// it is a dead connection or a refused request.
///
/// Five minutes rather than one because the first token of a large prompt on a
/// busy provider can take a while, and failing a run for a slow start would be a
/// worse bug than the one this fixes.
const MODEL_SILENCE_SECS: i64 = 300;

/// How long the integrated suite may take.
///
/// Longer than a single task's criterion on purpose: the suite runs the whole
/// repository's tests, not one file's, and a swarm that merged six tasks is
/// running six times the tests.
const SUITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(900);

/// The file a driver holds an exclusive lock on for the life of a run.
const DRIVER_LOCK: &str = "driver.lock";

/// The file an adopting process writes to ask the driver to stop the run.
///
/// A run outlives the window that started it, so the panel can be showing a run
/// another process is driving. That process's cancel flag is not reachable from
/// here — it is an `AtomicBool` in another address space — and the run directory
/// is the only thing the two share. So the request is a file: the adopter writes
/// it, the driver checks for it once per tick, and neither side has to trust the
/// other's memory. Both sides ship together, which is what makes this a
/// mechanism rather than a workaround.
const STOP_REQUEST: &str = "stop.request";

/// How long the reflection pass may take before the run records a degraded
/// reflection rather than waiting on it forever.
///
/// Shorter than the member idle limit on purpose. A wedged worker is a wedged
/// worker; a wedged reflection pass is a run that has already succeeded being
/// held open by its own bookkeeping.
const REFLECTION_LIMIT_SECS: i64 = 180;

/// How many skills the goal's ranking puts in front of the lead.
///
/// Small on purpose. The lead is given the whole catalogue below this, so the
/// shortlist is a hint about where to look rather than the set it may choose
/// from — and a hint of twenty is not a hint.
const SKILL_SHORTLIST: usize = 6;

/// How many skills the lead's brief describes in full.
///
/// A machine with a hundred skills would otherwise spend a thousand tokens of
/// brief on descriptions the lead will not use. Past this the brief lists names
/// only, because the rules refuse a name the brief does not carry: a skill that
/// is described nowhere is still assignable, a skill that is named nowhere is
/// not.
const SKILL_CATALOGUE_IN_BRIEF: usize = 40;

/// How many skills a task is given when the plan named none.
///
/// Two. A brief with five skills in it is a brief the worker skims, and the
/// ranking's third-best guess is usually a coincidence.
const SKILLS_PER_TASK: usize = 2;

/* ------------------------------------------------------------------ *
 * Who is driving
 * ------------------------------------------------------------------ */

/// Take the driver's lock for a run, or report that somebody else has it.
///
/// A run outlives the window that started it, so "is anybody driving this?" has
/// to be answerable from another process. A pid file answers it badly: pids are
/// reused, and a crashed driver leaves a file that still claims to be alive. An
/// exclusive file lock answers it exactly — the OS drops it when the process
/// dies, however it dies — so the lock is the driver's heartbeat and its
/// tombstone at once.
fn hold_driver_lock(dir: &Path) -> Option<std::fs::File> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(dir.join(DRIVER_LOCK))
        .ok()?;
    file.try_lock().ok()?;
    Some(file)
}

/// Whether another process is driving the run in `dir`.
///
/// An I/O error counts as alive: a lock that cannot be read is not evidence that
/// the driver is gone, and interrupting a live run on a transient error would
/// throw away work that is still being done.
fn driver_is_alive(dir: &Path) -> bool {
    let Ok(file) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(dir.join(DRIVER_LOCK))
    else {
        // No lock file at all: a run from before the lock existed, or one whose
        // directory was cleaned. Nobody is holding it either way.
        return false;
    };
    match file.try_lock() {
        Ok(()) => {
            // We got it, so nobody else had it. Release it again so the answer
            // does not depend on this handle being dropped promptly.
            let _ = file.unlock();
            false
        }
        Err(_) => true,
    }
}

/// Whether somebody has asked the driver of the run in `dir` to stop it.
///
/// Presence is the whole signal. The body is for the human reading the directory
/// afterwards, not for the driver: a request that had to be parsed would be a
/// request a malformed write could silently drop.
fn stop_requested(dir: &Path) -> bool {
    dir.join(STOP_REQUEST).exists()
}

/// Write the stop request a driver checks for.
///
/// Written whole and then renamed, for the same reason `run.json` is: a driver
/// that read the file mid-write would see half a request, and a half request is
/// indistinguishable from none.
fn request_stop_at(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir)
        .map_err(|error| format!("could not create {}: {error}", dir.display()))?;
    let body = json!({ "at": now(), "by": "another process" }).to_string();
    let temp = dir.join(format!("{STOP_REQUEST}.tmp"));
    std::fs::write(&temp, body)
        .map_err(|error| format!("could not write {}: {error}", temp.display()))?;
    std::fs::rename(&temp, dir.join(STOP_REQUEST))
        .map_err(|error| format!("could not write the stop request: {error}"))
}

/// Mark a run whose driver is gone as interrupted, once, on disk.
///
/// `Interrupted` exists for exactly this case and was never written: a run whose
/// process died stayed `running` on disk forever, which is why the panel used to
/// show a phantom run after a restart. Members that were burning become `lost`,
/// which is what the roster already knows how to draw.
fn interrupt_orphan(mut run: Run, board: &mut Board) -> Run {
    run.state = RunState::Interrupted;
    run.failure = Some("the process driving this run is gone".into());
    for member in std::iter::once(&mut run.lead).chain(run.members.iter_mut()) {
        if member.state.is_live() {
            member.state = MemberState::Lost;
        }
    }
    run.touch();
    let entry = board.append(BoardEntry::new(
        "engine",
        BoardKind::Escalation,
        "the process driving this run is gone — members are lost, worktrees are kept",
    ));
    let _ = Board::append_at(&run.dir(), &entry);
    let _ = run.save();
    run
}

/* ------------------------------------------------------------------ *
 * The engine
 * ------------------------------------------------------------------ */

pub struct Engine {
    inner: Mutex<Inner>,
}

struct Inner {
    run: Option<Run>,
    board: Board,
    /// One bridge per member id. Dropping a bridge kills its process.
    bridges: BTreeMap<String, Arc<AcpBridge>>,
    /// One usage ledger per member id, written by that member's gate and read
    /// by the drive loop. Kept beside the bridges because they share a
    /// lifetime: a member that is gone has nothing left to spend.
    usage: BTreeMap<String, Arc<Mutex<Usage>>>,
    /// One skill watch per member id, for the same reason as the ledgers: it
    /// records what the member's own tool calls loaded, and a member that is
    /// gone has nothing left to load.
    watches: BTreeMap<String, Arc<super::watch::MemberWatch>>,
    /// Set by `stop`, read by the drive loop.
    cancel: Arc<AtomicBool>,
    /// Whether the drive loop is running, so a second `approve` cannot start a
    /// second loop over the same run.
    driving: bool,
    /// True when `run` was read from disk rather than started here. This process
    /// is then a viewer, not the driver: it cannot stop or approve the run, and
    /// saying so is the difference between a panel and a lie.
    adopted: bool,
    /// Held for the life of a run this process drives. Dropping it — or dying —
    /// is what tells every other process the run has no driver.
    driver_lock: Option<std::fs::File>,
    /// The length `board.jsonl` had the last time memory was reconciled with it.
    ///
    /// Zero means "re-read it": set after every `post`, because a write from
    /// this process is exactly the moment a write from another one could have
    /// slipped in beside it.
    board_bytes: u64,
    /// Members whose agent process this engine stopped itself. A turn awaiting a
    /// bridge the engine took away sees the same close a dropped provider gives.
    stopped: BTreeSet<String>,
    /// The run whose end-of-run release has already happened. The drive loop and
    /// the headless runner both ask for it, and the second ask must do nothing.
    ended: Option<String>,
}

impl Inner {
    /// Take the member's bridge, recording that the stop was the engine's.
    fn take_bridge(&mut self, member_id: &str) -> Option<Arc<AcpBridge>> {
        self.stopped.insert(member_id.to_string());
        self.bridges.remove(member_id)
    }

    /// Give the member a bridge, which makes an earlier stop stale.
    fn put_bridge(&mut self, member_id: &str, bridge: Arc<AcpBridge>) {
        self.stopped.remove(member_id);
        self.bridges.insert(member_id.to_string(), bridge);
    }

    /// Whether the last thing this engine did to the member was stop its process.
    fn stopped_deliberately(&self, member_id: &str) -> bool {
        self.stopped.contains(member_id)
    }
}

impl Default for Engine {
    fn default() -> Self {
        Self {
            inner: Mutex::new(Inner {
                run: None,
                board: Board::new(),
                bridges: BTreeMap::new(),
                usage: BTreeMap::new(),
                watches: BTreeMap::new(),
                cancel: Arc::new(AtomicBool::new(false)),
                driving: false,
                adopted: false,
                driver_lock: None,
                board_bytes: 0,
                stopped: BTreeSet::new(),
                ended: None,
            }),
        }
    }
}

static ENGINE: std::sync::OnceLock<Engine> = std::sync::OnceLock::new();

pub fn engine() -> &'static Engine {
    ENGINE.get_or_init(Engine::default)
}

impl Engine {
    /* -------------------------------------------------------------- *
     * Reading
     * -------------------------------------------------------------- */

    /// Whether the engine stopped this member's process itself.
    pub(crate) fn stopped_deliberately(&self, member_id: &str) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .stopped_deliberately(member_id)
    }

    /// The live run, cloned.
    pub fn run(&self) -> Option<Run> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).run.clone()
    }

    /// The board tail for the live run.
    pub fn board_tail(&self) -> Vec<BoardEntry> {
        self.reconcile_board();
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.board.tail(BOARD_TAIL).to_vec()
    }

    /// Fold entries written to `board.jsonl` by anything other than `post` into
    /// the in-memory board.
    ///
    /// The gate permits a member to write inside the run directory, and a member
    /// that misreads the brief writes `board.jsonl` directly instead of its
    /// inbox. The engine trusted memory alone, so that entry was on disk and
    /// invisible to the drive loop: the twelfth live run's w-t3 posted its
    /// evidence that way, the loop never saw it, and the run died ten minutes
    /// later with "a member stopped responding" — a reason that named nothing.
    /// The file is the record; memory is a cache of it.
    ///
    /// Cheap when nothing changed: one `metadata` call, and the file is only
    /// read when its length moved. Entries are matched on what they say rather
    /// than on their sequence number, because a writer outside the engine has no
    /// way to know which numbers are taken.
    pub(crate) fn reconcile_board(&self) -> usize {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(run) = inner.run.clone() else {
            return 0;
        };
        let path = board_file::board_path(&run.dir());
        let Ok(meta) = std::fs::metadata(&path) else {
            return 0;
        };
        if meta.len() == inner.board_bytes {
            return 0;
        }
        // The length is recorded before the read, not after: a write that lands
        // between the two is then still shorter than the file, so the next tick
        // reads again and finds it. Recording it after the read would swallow
        // that write for the rest of the run.
        inner.board_bytes = meta.len();
        let on_disk = Board::load_at(&run.dir());
        let known: std::collections::HashSet<(String, String, String)> = inner
            .board
            .entries()
            .iter()
            .map(|e| (e.at.clone(), e.from.clone(), e.text.clone()))
            .collect();
        let mut added = 0;
        for entry in on_disk.entries() {
            let key = (entry.at.clone(), entry.from.clone(), entry.text.clone());
            if known.contains(&key) {
                continue;
            }
            inner.board.adopt(entry.clone());
            added += 1;
        }
        added
    }

    /// The run the panel should show, and whether this process drives it.
    ///
    /// Three cases, and the third is the one that used to be a lie:
    ///
    ///   owned     this process started it and is driving it — memory is truth
    ///   adopted   another process (or a previous window) is driving it — disk is
    ///             truth, re-read every time, because the writer is not us
    ///   none      nothing live anywhere
    ///
    /// A run outlives the window that started it. Before this, restarting the app
    /// mid-run left the panel showing an empty state while the run kept spending
    /// tokens, and a run started by `swarm-run` was invisible to the app entirely.
    /// An adopted run is reported as `external` so the panel can say who is
    /// driving it instead of offering a Stop button that cannot work.
    fn current(&self) -> (Option<Run>, Vec<BoardEntry>, bool) {
        self.reconcile_board();
        let (owned, adopted, board) = {
            let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            (
                inner.run.clone(),
                inner.adopted,
                inner.board.tail(BOARD_TAIL).to_vec(),
            )
        };

        if let Some(run) = owned {
            if !adopted {
                return (Some(run), board, false);
            }
            // Adopted: the process driving it is the writer, so the copy in
            // memory is only as fresh as the last read — and the driver can die
            // while the panel is watching. That is the case that used to leave a
            // phantom run on screen: adopted once, live forever.
            let Ok(fresh) = Run::load(&run.id) else {
                // The run's directory is gone: deleted from another window, or
                // cleaned up. A ghost is worse than an empty panel, so forget it
                // and fall through to the scan.
                let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                inner.run = None;
                inner.board = Board::new();
                inner.adopted = false;
                return (None, Vec::new(), false);
            };
            let mut loaded = Board::load_at(&fresh.dir());
            let alive = fresh.state.is_live() && driver_is_alive(&fresh.dir());
            let shown = if fresh.state.is_live() && !alive {
                interrupt_orphan(fresh, &mut loaded)
            } else {
                fresh
            };
            let tail = loaded.tail(BOARD_TAIL).to_vec();
            {
                let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                inner.run = Some(shown.clone());
                inner.board = loaded;
                inner.adopted = alive;
            }
            return (Some(shown), tail, alive);
        }

        // Nothing in memory. A run on disk that is still live is a run this
        // process should be showing — unless nobody is driving it, in which case
        // it is an orphan and the honest thing is to say so once, on disk, and
        // show it as interrupted rather than as a run that is still working.
        for found in super::list_runs() {
            if !found.state.is_live() {
                continue;
            }
            let mut loaded = Board::load_at(&found.dir());
            let alive = driver_is_alive(&found.dir());
            let shown = if alive {
                found
            } else {
                interrupt_orphan(found, &mut loaded)
            };
            let tail = loaded.tail(BOARD_TAIL).to_vec();
            {
                let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                inner.run = Some(shown.clone());
                inner.board = loaded;
                inner.adopted = alive;
            }
            return (Some(shown), tail, alive);
        }
        (None, Vec::new(), false)
    }

    /// Every member's live activity, with the feed attached to one of them.
    ///
    /// Read from this engine's own watches rather than from the global one, so a
    /// test can build an engine, put rings in it, and read back exactly what the
    /// panel would be handed.
    fn activity_of(&self, selected: Option<&str>) -> Vec<(String, super::Activity)> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner
            .watches
            .iter()
            .map(|(id, watch)| (id.clone(), watch.activity(Some(id.as_str()) == selected)))
            .collect()
    }

    /// Everything the panel renders.
    ///
    /// `selected` is the member the panel is showing in detail, if any. It is a
    /// parameter rather than a field on the engine because it is a fact about the
    /// window, not about the run: two windows can be looking at two members of
    /// the same run, and the engine has no business remembering which.
    ///
    /// It is here because the feed is the one part of the snapshot that is not
    /// bounded by the run. Two hundred items per member is four thousand items on
    /// a twenty-one member run, every two seconds, for a pane that can only ever
    /// show one member. So `current` goes to everyone and `recent` goes to the
    /// selected member alone.
    pub fn snapshot(&self, workspace: Option<&Path>, selected: Option<&str>) -> SwarmSnapshot {
        let (mut run, board, external) = self.current();

        // The rings are live data and are never saved, so they are read here
        // rather than folded onto the run by the drive loop. A `run.json` that
        // carried them would be a record of what each member was doing at the
        // moment of the last save, which is the thing this phase exists to stop
        // the panel from showing.
        if let Some(run) = run.as_mut() {
            for (id, activity) in self.activity_of(selected) {
                if let Some(member) = run.member_mut(&id) {
                    member.activity = activity;
                }
            }
        }

        let records = capabilities::registry().snapshot();
        let total = records.len();
        let on = records.iter().filter(|r| r.enabled).count();

        let mut preconditions = preconditions(workspace);
        // The capability gate is the one the panel entry's dim state encodes, so
        // it is computed from the same snapshot the ledger renders — and from
        // the same reachability check the precondition itself uses, so the two
        // cannot disagree about whether a member can call what the panel counts.
        let unreachable = unreachable_capabilities(workspace);
        if on < total || !unreachable.is_empty() {
            if let Some(entry) = preconditions.iter_mut().find(|p| p.key == "capabilities") {
                entry.ready = false;
                entry.detail = if !unreachable.is_empty() {
                    format!(
                        "{} is on but not registered as an MCP server, so no member can call it — add it in Capabilities",
                        unreachable.join(" and ")
                    )
                } else {
                    format!("{on} of {total} capabilities on — the swarm leans on all of them")
                };
            }
        }

        let live = run.as_ref().map(|r| r.state.is_live()).unwrap_or(false);
        let history = super::list_runs()
            .iter()
            .filter(|r| !r.state.is_live())
            .map(RunSummary::from)
            .collect();

        SwarmSnapshot {
            run,
            board,
            preconditions,
            capabilities_all_on: on == total && total > 0 && unreachable.is_empty(),
            capabilities_on: on,
            capabilities_total: total,
            history,
            live,
            external,
        }
    }

    /* -------------------------------------------------------------- *
     * The rehearsal — no processes, no spend
     * -------------------------------------------------------------- */

    /// Schedule and cost a plan without running it.
    ///
    /// This is the feature that makes the swarm honest before it is expensive:
    /// the user sees the waves, the wall-clock floor and the token estimate, and
    /// decides whether 15× is worth it for *this* plan. It is also what makes the
    /// panel demonstrable without a live agent.
    ///
    /// The wall clock is costed at what this workspace's own runs actually spent
    /// per task, not at the constant Phase 13 shipped. The observed run's worker
    /// turn was 6 minutes and its verifier's 20.7, so a task-pair is about 27
    /// minutes and the constant understated a real task by more than half — which
    /// is why the warning that was supposed to fire on that plan did not.
    pub fn rehearse(&self, plan: &Plan, concurrency: usize) -> RehearsalView {
        self.rehearse_with(plan, concurrency, self.history())
    }

    /// The same review, against a history the caller supplies.
    ///
    /// Split from [`Engine::rehearse`] so the review is a pure function of the
    /// plan, the width and what the workspace learned. The app data directory is
    /// a process-wide `OnceLock` that a test cannot point somewhere else, so a
    /// review that could only be exercised by writing runs to the real one would
    /// be a review with no test.
    fn rehearse_with(
        &self,
        plan: &Plan,
        concurrency: usize,
        history: history::History,
    ) -> RehearsalView {
        let violations = plan::validate(plan).err().unwrap_or_default();
        let shape = plan::shape(plan);
        let useful = schedule::useful_concurrency(&plan.tasks, concurrency);
        let rehearsal = schedule::rehearse(&plan.tasks, useful, Some(&history));

        RehearsalView {
            violations: violations.iter().map(|v| ViolationView::from(v)).collect(),
            shape,
            rehearsal,
            concurrency: useful,
            role_medians: history.role_medians(),
        }
    }

    /// The past runs of the workspace this engine is looking at.
    ///
    /// The workspace comes from the run in hand, because that is the only
    /// workspace the engine knows about and the review is about that one. With no
    /// run there is no workspace, and the review falls back to the assumed median
    /// rather than guessing at a directory.
    ///
    /// The current run is excluded by id. `list_runs` reads the directory and the
    /// current run is written there before its lead is spawned, so it is always
    /// in the list and would otherwise be the newest thing the review measured.
    fn history(&self) -> history::History {
        let (workspace, id) = {
            let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            match inner.run.as_ref() {
                Some(run) => (run.workspace.clone(), run.id.clone()),
                None => return history::History::empty(),
            }
        };
        history::History::of(&workspace, Some(&id))
    }

    /// Rehearse the built-in example plan.
    pub fn rehearse_example(&self, concurrency: usize) -> RehearsalView {
        self.rehearse(&roles::example_plan(), concurrency)
    }

    /* -------------------------------------------------------------- *
     * Starting
     * -------------------------------------------------------------- */

    /// Create a run and spawn the lead.
    ///
    /// The lead is the only member alive at this point. It writes a plan into its
    /// inbox; the drive loop validates it and parks the run at
    /// `AwaitingApproval` for the user's one decision.
    pub async fn start(
        &self,
        sink: Arc<dyn EventSink>,
        goal: String,
        workspace: PathBuf,
        models: RoleModels,
        budget_tokens: u64,
        budget_micros: Option<u64>,
        concurrency: usize,
    ) -> Result<Run, String> {
        {
            let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(run) = inner.run.as_ref() {
                if run.state.is_live() {
                    // An adopted run is somebody else's to stop, so the advice
                    // has to name the right place to stop it.
                    return Err(if inner.adopted {
                        format!(
                            "run {} is still {} and is driven by another process — wait for it or stop it there",
                            run.id,
                            run.state.label()
                        )
                    } else {
                        format!(
                            "run {} is still {} — stop it before starting another",
                            run.id,
                            run.state.label()
                        )
                    });
                }
            }
        }

        let mut lead = Member::new("lead", Role::Lead, models.lead.clone());
        lead.state = MemberState::Starting;
        lead.thinking = Some(models.thinking.lead.clone()).filter(|l| !l.is_empty());

        let mut run = Run::new(goal.clone(), workspace.to_string_lossy().to_string(), lead);
        run.budget.max_tokens = budget_tokens;
        // The ceiling in money, when the caller set one. Reported spend is
        // always computed; this only decides whether the run stops on it.
        run.budget.max_spend_micros = budget_micros.filter(|micros| *micros > 0);
        run.budget.started_at = Some(now());
        run.concurrency = concurrency.max(1);

        // The method this run will be handed. Selected before the lead's first
        // turn, because the lead is the one that assigns skills to tasks and it
        // cannot assign what it has not been told exists.
        let (catalogue, shortlist, lead_skills) = plan_skills(&goal, &workspace);
        run.skills = catalogue.skills.clone();
        run.skill_shortlist = shortlist.clone();
        // Recorded, not re-read: the switch can be flipped while the run is in
        // flight, and the panel would then describe a run that did not happen.
        run.skills_enabled = crate::settings::load().swarm_skills;
        if !shortlist.is_empty() {
            run.skill_gaps.clear();
        }

        let dir = run.dir();
        std::fs::create_dir_all(&dir).map_err(|e| format!("could not create the run: {e}"))?;
        run.save()?;

        {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.run = Some(run.clone());
            inner.board = Board::new();
            inner.bridges.clear();
            inner.watches.clear();
            inner.cancel = Arc::new(AtomicBool::new(false));
            inner.driving = false;
            inner.adopted = false;
            inner.driver_lock = hold_driver_lock(&dir);
        }

        emit(&sink, &run);

        // Spawn the lead. A failure here is a named state, not a hang.
        match spawn_member(
            sink.clone(),
            "lead",
            Role::Lead,
            &workspace,
            &models.lead,
            Some(models.thinking.lead.as_str()),
            Some(roles::agent_for(Role::Lead)),
            Vec::new(),
            None,
        )
        .await
        {
            Ok(spawned) => {
                let Spawned {
                    bridge,
                    usage,
                    session_id,
                    pid,
                    thinking,
                    agent,
                    watch,
                    ..
                } = spawned;
                let inbox = inbox_dir(&run.id, "lead");
                let _ = std::fs::create_dir_all(&inbox);
                // What the last runs of this workspace learned. The run was
                // saved before this point, so it is already in `list_runs` and is
                // excluded by id: a lead told about itself has learned nothing.
                let history = history::History::of(
                    &workspace.to_string_lossy(),
                    Some(run.id.as_str()),
                );
                let brief = roles::lead_brief(
                    &goal,
                    &workspace.to_string_lossy(),
                    &run.id,
                    &inbox.to_string_lossy(),
                    &lead_skills,
                    &history.lead_section(),
                );
                {
                    let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                    inner.put_bridge("lead", bridge.clone());
                    inner.watches.insert("lead".into(), watch.clone());
                    inner.usage.insert("lead".into(), usage.clone());
                    if let Some(run) = inner.run.as_mut() {
                        run.lead.session_id = Some(session_id.clone());
                        run.lead.pid = pid;
                        run.lead.thinking = thinking.clone();
                        run.lead.agent = agent.clone();
                        run.lead.state = MemberState::Working;
                        run.lead.last_at = Some(now());
                        run.touch();
                    }
                }
                // Persist the run as it now stands, not the copy taken before the
                // session existed: the session id is what makes the member
                // resumable, and saving the stale copy would lose it.
                let started = self.run().unwrap_or(run);
                let _ = started.save();
                emit(&sink, &started);

                // The brief is the lead's first turn. It is not awaited here: the
                // drive loop owns the run's progress, and a turn can take minutes.
                let bridge_for_turn = bridge.clone();
                let session_for_turn = session_id.clone();
                let sink_for_turn = sink.clone();
                tokio::spawn(async move {
                    drive_turn(
                        &sink_for_turn,
                        &bridge_for_turn,
                        LEAD_ID,
                        &session_for_turn,
                        &brief,
                    )
                    .await;
                });

                self.spawn_drive(sink.clone(), started.id.clone());
                Ok(started)
            }
            Err(error) => {
                let mut failed = run.clone();
                failed.state = RunState::Failed;
                failed.failure = Some(error.clone());
                failed.lead.state = MemberState::Failed;
                failed.lead.error = Some(error.clone());
                let _ = failed.save();
                {
                    let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                    inner.run = Some(failed.clone());
                }
                emit(&sink, &failed);
                Err(error)
            }
        }
    }

    /// Approve the plan and start the workers.
    ///
    /// The plan is re-validated here rather than trusted from the panel: the
    /// panel is a view, and a view must not be able to start a run the gate would
    /// refuse.
    pub async fn approve(
        &self,
        sink: Arc<dyn EventSink>,
        plan: Plan,
        models: RoleModels,
        concurrency: usize,
    ) -> Result<Run, String> {
        let mut plan = plan;
        // The panel sends back the plan it read, which may be the lead's plan
        // before the engine filled its bare tasks. Filling here as well as in the
        // drive loop means the approved plan and the planned plan are the same
        // plan, whichever path the run took to approval.
        let current = self.run();
        let mut catalogue = Catalogue {
            skills: current
                .as_ref()
                .map(|run| run.skills.clone())
                .unwrap_or_default(),
        };
        // The same extension the drive loop makes, so a plan approved from the
        // panel is gated against the same catalogue the loop would have used.
        if let Some(run) = current.as_ref() {
            catalogue.skills.extend(
                plan.skills_to_write
                    .iter()
                    .filter(|draft| plan::draft_problem(draft).is_none())
                    .map(|draft| draft_skill(&run.id, draft)),
            );
        }
        settle_task_skills(&catalogue, &mut plan);
        let violations = plan::validate_with(&plan, Some(&catalogue))
            .err()
            .unwrap_or_default();
        if !violations.is_empty() {
            return Err(violations
                .iter()
                .map(Violation::message)
                .collect::<Vec<_>>()
                .join("; "));
        }

        if self.inner.lock().unwrap_or_else(|e| e.into_inner()).adopted {
            return Err(
                "this run is driven by another process — approve it there".to_string(),
            );
        }

        let mut run = self
            .run()
            .ok_or_else(|| "there is no run to approve".to_string())?;
        if run.state != RunState::AwaitingApproval && run.state != RunState::Planning {
            return Err(format!(
                "run {} is {}, not awaiting approval",
                run.id,
                run.state.label()
            ));
        }

        let workspace = PathBuf::from(&run.workspace);
        let base = worktree::head(&CommandGit, &workspace)
            .map_err(|e| format!("could not read HEAD: {e}"))?;

        // The base checkout, detached at the commit the run started from.
        //
        // Three jobs. It is the precondition probe — if git cannot make a detached
        // worktree, the run should say so before spending a lead turn rather than
        // discovering it at the first verdict. It is the baseline for a task
        // whose own base commit was never recorded, which is every task in a run
        // adopted from disk after an upgrade. A task that does have one is proven
        // against its own checkout instead, because a dependent task's criterion
        // was written for its dependency's tree, not for the run's base. And it is
        // the tree the criterion gate below runs against, which is why it is made
        // before the plan is accepted rather than after.
        //
        // A failure here is not fatal: the run proceeds and every verdict records
        // that its proof could not be attempted.
        let base_checkout = worktree::base_checkout_path(&run.id);
        let base_ready = worktree::ensure_detached(&CommandGit, &workspace, &base_checkout, &base)
            .map_err(|error| error.to_string());

        // The same gate the drive loop applies when the lead writes a plan, run
        // again here because the panel is a view and a view must not be able to
        // start a run the gate would refuse. A criterion that already passes on
        // the base commit cannot prove anything, and the user is about to spend a
        // run's worth of tokens discovering that.
        if base_ready.is_ok() {
            let unprovable = criteria_passing_on_base(&run.id, &workspace, &base, &plan);
            if !unprovable.is_empty() {
                return Err(unprovable
                    .iter()
                    .map(Violation::message)
                    .collect::<Vec<_>>()
                    .join("; "));
            }
        }

        // What this workspace's own runs cost per task, and per role, so the clock
        // and the token estimate the run is given are measurements rather than the
        // constants Phase 13 shipped.
        let history = history::History::of(&run.workspace, Some(run.id.as_str()));
        run = start_running(&run, &plan, &base, concurrency, Some(&history));

        // The skills the plan authored, written now that the plan is accepted.
        // The catalogue the gate saw already held them; this is the file they
        // point at, and the run's own catalogue is what every brief reads.
        let (written, failed) = write_plan_skills(&run.id, &plan);
        if !written.is_empty() {
            run.skills = catalogue.skills.clone();
            self.post(
                BoardEntry::new(
                    "engine",
                    BoardKind::Note,
                    format!(
                        "the plan wrote {} for this run: {}",
                        if written.len() == 1 { "a skill" } else { "skills" },
                        written
                            .iter()
                            .map(|skill| skill.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                )
                .about("run"),
            );
        }
        if !failed.is_empty() {
            self.post(
                BoardEntry::new(
                    "engine",
                    BoardKind::Note,
                    format!("could not write the plan's skills: {}", failed.join("; ")),
                )
                .about("run"),
            );
        }

        // The integration branch is created up front so a merge failure is a
        // merge failure, not a missing branch.
        worktree::create_branch(&CommandGit, &workspace, &run.integration.branch, &base)
            .map_err(|e| format!("could not create the integration branch: {e}"))?;

        if let Err(error) = &base_ready {
            self.post(
                BoardEntry::new(
                    "engine",
                    BoardKind::Escalation,
                    format!("no base checkout, so failing-first cannot be proven: {error}"),
                )
                .about("run"),
            );
        }

        // One worker per task, one verifier per task. Named by role so the roster
        // reads as a roster rather than as a list of ids, and scoped to their task
        // at creation rather than at first spawn: a member that has not started
        // yet still belongs to a task, and a roster that cannot say which one is
        // a roster of strangers until the run is half over.
        for task in &run.tasks {
            let mut worker = Member::new(
                format!("w-{}", task.id),
                Role::Worker,
                models.worker.clone(),
            );
            worker.task = Some(task.id.clone());
            worker.thinking = Some(models.thinking.worker.clone()).filter(|l| !l.is_empty());
            let mut verifier = Member::new(
                format!("v-{}", task.id),
                Role::Verifier,
                models.verifier.clone(),
            );
            verifier.task = Some(task.id.clone());
            verifier.thinking = Some(models.thinking.verifier.clone()).filter(|l| !l.is_empty());
            run.members.push(worker);
            run.members.push(verifier);
        }

        run.save()?;
        // A request left in the directory would cancel this run on its first
        // tick. The directory is per-run, so this should never fire — which is
        // exactly why it is cheaper to make true than to assume.
        let _ = std::fs::remove_file(run.dir().join(STOP_REQUEST));
        {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.run = Some(run.clone());
            inner.cancel = Arc::new(AtomicBool::new(false));
            inner.adopted = false;
            if inner.driver_lock.is_none() {
                inner.driver_lock = hold_driver_lock(&run.dir());
            }
        }
        emit(&sink, &run);

        self.spawn_drive(sink, run.id.clone());
        Ok(run)
    }

    /// Stop the run. Members are parked, worktrees are kept and named.
    pub fn stop(&self, sink: &Arc<dyn EventSink>) -> Result<Run, String> {
        self.cancel_run(sink, true)
    }

    /// Cancel the run this process drives.
    ///
    /// Split out of `stop` so the drive loop can reach it. A stop asked for by
    /// another process arrives as a file, and the loop that reads the file is the
    /// same one that has to act on it — so the acting half lives here, where both
    /// callers can see it.
    ///
    /// `refuse_adopted` is the one thing the two callers disagree about. A viewer
    /// must not cancel a run it does not drive: setting this process's cancel flag
    /// would change nothing and marking the run cancelled would write a state the
    /// driver is about to overwrite — a Stop button that reports success and stops
    /// nothing is worse than one that refuses. The drive loop, by definition, is
    /// the driver, so it passes `false`. The check is inside this function rather
    /// than in `stop` so that it and the cancel happen under one lock: a check
    /// that released the lock first could be answered about a different run.
    fn cancel_run(
        &self,
        sink: &Arc<dyn EventSink>,
        refuse_adopted: bool,
    ) -> Result<Run, String> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if refuse_adopted && inner.adopted {
            let id = inner.run.as_ref().map(|r| r.id.clone()).unwrap_or_default();
            return Err(format!(
                "run {id} is driven by another process — stop it there"
            ));
        }
        inner.cancel.store(true, Ordering::SeqCst);
        let Some(run) = inner.run.as_mut() else {
            return Err("there is no run to stop".into());
        };
        if run.state.is_terminal() {
            return Err(format!("run {} is already {}", run.id, run.state.label()));
        }
        run.state = RunState::Cancelled;
        for member in std::iter::once(&mut run.lead).chain(run.members.iter_mut()) {
            if member.state.is_live() {
                member.state = MemberState::Parked;
                member.parked_at = Some(now());
            }
        }
        run.touch();
        let stopped = run.clone();
        let _ = stopped.save();
        drop(inner);
        emit(sink, &stopped);
        Ok(stopped)
    }

    /// Ask the process driving an adopted run to stop it.
    ///
    /// The run directory is shared and the driver's cancel flag is not, so the
    /// request is a file the driver checks each tick. This process cannot know
    /// whether the driver acted, so it does not claim the run stopped: it writes
    /// the request and says so, and the panel shows the run's own state from then
    /// on, which is the only honest source.
    pub fn request_stop(&self, sink: &Arc<dyn EventSink>) -> Result<Run, String> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(run) = inner.run.clone() else {
            return Err("there is no run to stop".into());
        };
        if !inner.adopted {
            return Err(format!(
                "run {} is driven here — stop it directly",
                run.id
            ));
        }
        if run.state.is_terminal() {
            return Err(format!("run {} is already {}", run.id, run.state.label()));
        }
        drop(inner);
        request_stop_at(&run.dir())?;
        // The request is a fact about the run, so it goes on the board as well as
        // in the directory: the driver's own log should say who asked for this.
        self.post(
            BoardEntry::new(
                "engine",
                BoardKind::Note,
                "stop requested from another process — the process driving this run will act on it on its next tick",
            )
            .about("run"),
        );
        emit(sink, &run);
        Ok(run)
    }

    /// Act on a stop requested from another process, if there is one.
    ///
    /// Called once per tick by the drive loop, which already holds the run and so
    /// passes the directory rather than making this clone the document again.
    /// Returns the cancelled run when it acted, so the loop can say so and return;
    /// `None` when nobody asked.
    ///
    /// The request file is removed after it is honoured, so its presence always
    /// means "a request is outstanding" rather than "a request was once made".
    fn honour_stop_request(&self, dir: &Path, sink: &Arc<dyn EventSink>) -> Option<Run> {
        if !stop_requested(dir) {
            return None;
        }
        let stopped = self.cancel_run(sink, false).ok()?;
        let _ = std::fs::remove_file(dir.join(STOP_REQUEST));
        Some(stopped)
    }

    /// Delete a run and everything it owns.
    ///
    /// The order is the fix for the audit's sixth fault, in which a cancelled run
    /// left four worktrees and four branches behind: `git worktree remove --force`
    /// failed with "Permission denied" on three of them because the worker
    /// processes the engine had stopped were still holding the directories, and
    /// the branches had to be deleted by hand afterwards.
    ///
    /// So: stop the member processes, then remove the checkouts, then delete the
    /// branches, then the run directory. Anything that could not be cleaned is
    /// named in the event the panel receives, because a delete that reports
    /// success and leaves a directory behind is the failure this ordering exists
    /// to prevent.
    pub async fn delete(&self, sink: &Arc<dyn EventSink>, id: &str) -> Result<(), String> {
        {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            // A live run owns worktrees and member sessions that are still being
            // written. Deleting it would race the driver and leave half a run on
            // disk, so the answer is no until it is stopped.
            if let Some(run) = inner.run.as_ref() {
                if run.id == id && run.state.is_live() {
                    return Err(format!(
                        "run {id} is still {} — stop it before deleting it",
                        run.state.label()
                    ));
                }
            }
            if inner.run.as_ref().map(|r| r.id.as_str()) == Some(id) {
                inner.cancel.store(true, Ordering::SeqCst);
                inner.bridges.clear();
                inner.watches.clear();
                inner.run = None;
                inner.board = Board::new();
                inner.adopted = false;
                // Dropped before the directory goes: Windows will not remove a
                // file this process still holds open.
                inner.driver_lock = None;
            }
        }
        // Read before the directory goes: the workspace path and the branch names
        // live in the run document, and neither can be found afterwards.
        let loaded = Run::load(id).ok();
        let workspace = loaded.as_ref().map(|run| PathBuf::from(&run.workspace));
        let branches: Vec<String> = loaded
            .as_ref()
            .map(|run| {
                let mut names: Vec<String> = run
                    .tasks
                    .iter()
                    .map(|task| worktree::branch_name(&run.id, &task.id))
                    .collect();
                if !run.integration.branch.trim().is_empty() {
                    names.push(run.integration.branch.clone());
                }
                names
            })
            .unwrap_or_default();
        // The member processes first. Windows keeps a directory locked while the
        // process that has it open is alive, and `stop` returns when the child has
        // been signalled rather than when the OS has finished tearing it down.
        release_bridges().await;
        super::delete_run(id)?;
        let mut problems: Vec<String> = Vec::new();
        if let Some(workspace) = workspace {
            problems.extend(worktree::release_run(&CommandGit, &workspace, id));
            for branch in branches {
                if let Err(error) = worktree::delete_branch(&CommandGit, &workspace, &branch) {
                    problems.push(format!("the branch {branch} could not be deleted: {error}"));
                }
            }
        }
        sink.emit(EVENT_SWARM, json!({ "deleted": id, "cleanupProblems": problems }));
        Ok(())
    }

    /// Load a finished run into the panel without resuming it.
    pub fn open(&self, sink: &Arc<dyn EventSink>, id: &str) -> Result<Run, String> {
        let run = Run::load(id)?;
        let board = Board::load_at(&run.dir());
        {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.run = Some(run.clone());
            inner.board = board;
            inner.bridges.clear();
            inner.watches.clear();
            // Opening a run never resumes it, so a live one is still somebody
            // else's to drive.
            inner.adopted = run.state.is_live();
        }
        emit(sink, &run);
        Ok(run)
    }

    /* -------------------------------------------------------------- *
     * The drive loop
     * -------------------------------------------------------------- */

    fn spawn_drive(&self, sink: Arc<dyn EventSink>, run_id: String) {
        {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if inner.driving {
                return;
            }
            inner.driving = true;
        }
        let cancel = {
            let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.cancel.clone()
        };
        tokio::spawn(async move {
            drive(sink, run_id, cancel).await;
            let engine = engine();
            // A run that has stopped must stop spending.
            //
            // A member's turn is a task of its own, so the drive loop returning
            // does not end it. The thirteenth live run's v-t2 posted its verdict,
            // the run escalated, and the verifier went on getting commands
            // approved for another minute — tokens spent on a run that was
            // already over, and a roster that said `working` for a member whose
            // run had ended. Parking a member changes what the panel shows;
            // cancelling its session is what ends the turn.
            let (terminal, sessions) = {
                let inner = engine.inner.lock().unwrap_or_else(|e| e.into_inner());
                let terminal = inner
                    .run
                    .as_ref()
                    .map(|r| r.state.is_terminal())
                    .unwrap_or(false);
                let sessions: Vec<(String, String)> = inner
                    .run
                    .as_ref()
                    .map(|r| {
                        std::iter::once(&r.lead)
                            .chain(r.members.iter())
                            .filter_map(|m| {
                                m.session_id.clone().map(|s| (m.id.clone(), s))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                (terminal, sessions)
            };
            if terminal {
                let mut cancelled = 0usize;
                for (member, session) in sessions {
                    let bridge = {
                        let inner =
                            engine.inner.lock().unwrap_or_else(|e| e.into_inner());
                        inner.bridges.get(&member).cloned()
                    };
                    if let Some(bridge) = bridge {
                        if bridge.cancel(&session).await.is_ok() {
                            cancelled += 1;
                        }
                    }
                }
                // A finished run releases its agents.
                //
                // Nothing else does. The bridges live in the engine's map, which
                // is `'static`, so a terminal run left one `vibe-acp` process per
                // member alive until the app itself exited. Eight members per run
                // and a handful of runs is dozens of idle processes, and the
                // orphaned ones hold their worktree directories open, which is why
                // cleaning a project between runs failed with "Permission denied"
                // until the app was restarted. The end-to-end harness found it:
                // the test body finished and the process would not exit.
                if cancelled > 0 {
                    engine.post(
                        BoardEntry::new(
                            "engine",
                            BoardKind::Note,
                            format!(
                                "run stopped — cancelled {cancelled} live turn{}",
                                if cancelled == 1 { "" } else { "s" }
                            ),
                        )
                        .about("run"),
                    );
                }
                // Releasing the agents and removing the checkouts is
                // `end_of_run`'s job, and it happens once per run: this wrapper
                // and the drive loop's terminal branch both end up here, and each
                // doing its own release posted the reap note twice for one run.
                end_of_run().await;
            }
            let mut inner = engine.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.driving = false;
        });
    }

    /// Append an entry to the board and persist it.
    ///
    /// `pub(crate)` because the permission gate writes to the board from outside
    /// the drive loop: a decision the engine made on the user's behalf has to be
    /// as visible as one a member made.
    pub(crate) fn post(&self, entry: BoardEntry) -> BoardEntry {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let stored = inner.board.append(entry);
        if let Some(run) = inner.run.as_ref() {
            let _ = Board::append_at(&run.dir(), &stored);
        }
        // A write from here is the moment a write from somewhere else could have
        // landed beside it, so the next read of the file is not optional.
        inner.board_bytes = 0;
        stored
    }
}

/* ------------------------------------------------------------------ *
 * Preconditions
 * ------------------------------------------------------------------ */

/// Make the workspace git-ready for a run: `git init` when the folder is not
/// inside a repository, and an empty root commit when there is no HEAD to base
/// worktrees on. The user's files are never staged or committed. Called before
/// the gate at every start, so a folder becomes a repository instead of
/// refusing the run; a failure here surfaces as the repository precondition.
pub fn prepare_workspace(workspace: &Path) -> Result<worktree::RepoPrep, String> {
    worktree::ensure_repo(&CommandGit, workspace)
}

/// Everything that must be true before a run may start.
///
/// Computed when the panel opens, not discovered at run start: a refusal is only
/// cheap before the user has typed anything.
pub fn preconditions(workspace: Option<&Path>) -> Vec<Precondition> {
    preconditions_with(workspace, None)
}

/// The preconditions, with the agent's catalogue when the caller has one.
///
/// The model and diversity checks are about what the *agent* will accept, not
/// about what `config.toml` declares. A caller that has a live session passes
/// its catalogue and gets the truth; a headless caller passes nothing and gets
/// the config fallback, which is the same answer the old code gave.
pub fn preconditions_with(
    workspace: Option<&Path>,
    catalog: Option<&super::catalog::Catalog>,
) -> Vec<Precondition> {
    let mut out = Vec::new();

    // 1. All six capabilities on, and the two that arrive over MCP actually
    //    declared to the members. A toggle is not a capability: the code graph
    //    and web search reach a member only if their server is in the list the
    //    session is opened with, and a panel that counts the toggle alone
    //    certifies something the run cannot call.
    let records = capabilities::registry().snapshot();
    let total = records.len();
    let on = records.iter().filter(|r| r.enabled).count();
    let unreachable = unreachable_capabilities(workspace);
    if on == total && total > 0 && unreachable.is_empty() {
        out.push(Precondition::ready(
            "capabilities",
            "All capabilities on",
        ));
    } else if !unreachable.is_empty() {
        out.push(Precondition::blocked(
            "capabilities",
            "All capabilities on",
            format!(
                "{} is on but not registered as an MCP server, so no member can call it — add it in Capabilities",
                unreachable.join(" and ")
            ),
        ));
    } else {
        out.push(Precondition::blocked(
            "capabilities",
            "All capabilities on",
            format!("{on} of {total} capabilities on — the swarm leans on all of them"),
        ));
    }

    // 2. git on PATH.
    if worktree::git_available() {
        out.push(Precondition::ready("git", "git on PATH"));
    } else {
        out.push(Precondition::blocked(
            "git",
            "git on PATH",
            "git is not installed or not on PATH — the swarm isolates workers in worktrees",
        ));
    }

    // 3. A git repository with a root commit, for any task that writes.
    //    A folder that is not one is not a dead end: [`prepare_workspace`]
    //    makes it one when a run starts, so the detail says what will happen.
    match workspace {
        Some(path) if path.is_dir() => {
            if !worktree::is_repo(&CommandGit, path) {
                out.push(Precondition::blocked(
                    "repository",
                    "A git repository",
                    "not a git repository yet — starting a run runs git init here and adds an empty root commit (your files are never staged or committed)",
                ));
            } else if worktree::head(&CommandGit, path).is_err() {
                out.push(Precondition::blocked(
                    "repository",
                    "A git repository",
                    "this repository has no commits yet — starting a run adds an empty root commit, the base worktrees are created from",
                ));
            } else {
                out.push(Precondition::ready("repository", "A git repository"));
            }
        }
        Some(path) => out.push(Precondition::blocked(
            "repository",
            "A git repository",
            format!("{} is not a folder", path.display()),
        )),
        None => out.push(Precondition::blocked(
            "repository",
            "A git repository",
            "no workspace is open",
        )),
    }

    // 4. The worktree root is writable.
    let root = worktree::worktree_root();
    let probe = root.join(".vd-probe");
    let writable = std::fs::create_dir_all(&root)
        .and_then(|_| std::fs::write(&probe, b"ok"))
        .is_ok();
    let _ = std::fs::remove_file(&probe);
    if writable {
        out.push(Precondition::ready("worktrees", "Worktree root writable"));
    } else {
        out.push(Precondition::blocked(
            "worktrees",
            "Worktree root writable",
            format!("could not write to {}", root.display()),
        ));
    }

    // 5. A model, and a second one for the verifier.
    let (config, _) = config::load_effective(workspace);
    let resolved = match catalog {
        Some(catalog) if !catalog.is_empty() => catalog.clone(),
        _ => super::catalog::Catalog::from_config(&config),
    };
    let available: Vec<String> = resolved.aliases();
    if available.is_empty() {
        out.push(Precondition::blocked(
            "models",
            "A configured model",
            "no model is configured — the swarm cannot spawn a member",
        ));
        out.push(Precondition::blocked(
            "diversity",
            "A second model for the verifier",
            "no model is configured",
        ));
    } else {
        out.push(Precondition::ready("models", "A configured model"));
        let picked = roles::pick_models(
            &available,
            config.active_model.as_deref(),
            DiversityPolicy::Require,
        );
        match picked {
            Some(models) if models.diverse => {
                out.push(Precondition::ready(
                    "diversity",
                    "A second model for the verifier",
                ));
            }
            _ => out.push(Precondition::blocked(
                "diversity",
                "A second model for the verifier",
                "one model configured — a verifier on the same model is not independent",
            )),
        }
    }

    // 6. The swarm agents are provisioned.
    //
    //    Without them the lead runs on Vibe's own coding prompt, which says "if
    //    the task requires an edit, edit" — so the lead implements the goal
    //    itself instead of planning it and handing it to the workers. That is a
    //    silent failure: the run looks like a swarm and behaves like one agent.
    match crate::provision::swarm_ready() {
        Ok(()) => out.push(Precondition::ready("agents", "Swarm agents provisioned")),
        Err(reason) => out.push(Precondition::blocked(
            "agents",
            "Swarm agents provisioned",
            reason,
        )),
    }

    out
}

/// Whether every precondition holds.
pub fn ready(preconditions: &[Precondition]) -> bool {
    preconditions.iter().all(|p| p.ready)
}

/* ------------------------------------------------------------------ *
 * Spawning a member
 * ------------------------------------------------------------------ */

/// What starting a member actually produced.
///
/// The requested model and mode are requests: `vibe-acp` validates both and
/// silently keeps its own value when it does not like one. Recording what the
/// session ended up with — rather than what was asked for — is the difference
/// between a roster that describes the run and one that describes the intent.
pub struct Spawned {
    pub bridge: Arc<AcpBridge>,
    /// The ledger this member's gate writes its spend into.
    pub usage: Arc<Mutex<Usage>>,
    pub session_id: String,
    pub pid: Option<u32>,
    /// The model the session reports it is on.
    pub model: Option<String>,
    /// The reasoning level the session reports it is on.
    pub thinking: Option<String>,
    /// The mode the session reports it is in.
    pub agent: Option<String>,
    /// The skills this member's own tool calls loaded, live.
    ///
    /// Held rather than copied because the drive loop reads it while the member
    /// is still working: a worker that loads its skill at minute two and commits
    /// at minute nine should show the load at minute two.
    pub watch: Arc<super::watch::MemberWatch>,
}

/// The value a session reports for one config option.
fn config_value(session: &Value, id: &str) -> Option<String> {
    session
        .get("configOptions")
        .and_then(Value::as_array)
        .and_then(|options| {
            options
                .iter()
                .find(|option| option.get("id").and_then(Value::as_str) == Some(id))
        })
        .and_then(|option| option.get("currentValue"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Which config option carries the agent on this session.
///
/// `vibe-acp` 3.1.4 lists agents on an `agent` option and keeps `mode` for
/// permissions; 3.1.2 listed them as modes. Whichever offers this value is the
/// one to set, with `agent` preferred when both do — the newer layout, and the
/// one the installed version uses. `None` means neither, which is an error the
/// caller raises with both lists in it rather than a member left on the session
/// default.
fn agent_option(session: &Value, agent: &str) -> Option<&'static str> {
    if config_offers(session, "agent", agent) {
        return Some("agent");
    }
    if config_offers(session, "mode", agent) {
        return Some("mode");
    }
    None
}

/// A list for an error message, which has to say "nothing" rather than "".
fn listed(values: &[String]) -> String {
    if values.is_empty() {
        "nothing".to_string()
    } else {
        values.join(", ")
    }
}

/// Whether a session offers a value for one config option.
///
/// `vibe-acp` answers an unknown value with `-32602 Unsupported config option`,
/// so asking first is cheaper than a round trip that fails and a member that
/// runs in a mode nobody chose.
fn config_offers(session: &Value, id: &str, value: &str) -> bool {
    session
        .get("configOptions")
        .and_then(Value::as_array)
        .and_then(|options| {
            options
                .iter()
                .find(|option| option.get("id").and_then(Value::as_str) == Some(id))
        })
        .and_then(|option| option.get("options"))
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .any(|v| v.get("value").and_then(Value::as_str) == Some(value))
        })
        .unwrap_or(false)
}

/// The values a session offers for one config option, for an error message.
fn config_options(session: &Value, id: &str) -> Vec<String> {
    session
        .get("configOptions")
        .and_then(Value::as_array)
        .and_then(|options| {
            options
                .iter()
                .find(|option| option.get("id").and_then(Value::as_str) == Some(id))
        })
        .and_then(|option| option.get("options"))
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|v| v.get("value").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Start one member: its own process, its own session, its own worktree.
///
/// Takes a sink rather than an `AppHandle` so the whole path can be exercised
/// without a window — which is how the live test drives it against a real
/// `vibe-acp`.
///
/// `ledger` is the member's spend so far, when it already has some. A respawn
/// hands the new gate the ledger the previous session wrote into, so the run's
/// total is the member's whole cost rather than its latest attempt's.
async fn spawn_member(
    sink: Arc<dyn EventSink>,
    member_id: &str,
    role: Role,
    cwd: &Path,
    model: &str,
    thinking: Option<&str>,
    agent: Option<&str>,
    readable: Vec<PathBuf>,
    ledger: Option<Arc<std::sync::Mutex<super::Usage>>>,
) -> Result<Spawned, String> {
    // Scoped, not plain: a member's notifications must not land in the main
    // chat's transcript. The scope is the whole mechanism — the main window
    // listens on `acp://notification`, this bridge emits on
    // `acp://notification/swarm/<member>`, and the two never meet.
    //
    // The gate sits between the bridge and the real sink because a member has no
    // human to answer its permission requests. Without it every turn stalls for
    // `INTERACTION_TIMEOUT` and is then answered `cancelled` — which is what
    // every run before this one did.
    //
    // The watch sits outside the gate, so it sees the member's notifications
    // before the gate decides what to do with them. It only reads: a skill load
    // is a `tool_call` announcement, and the gate's permission traffic is a
    // different event.
    let watch = Arc::new(super::watch::MemberWatch::new(sink));
    let mut gate = permissions::Gate::new(watch.clone(), member_id, role, cwd.to_path_buf())
        .readable(readable)
        .watching(watch.clone());
    if let Some(ledger) = ledger {
        gate = gate.continuing(ledger);
    }
    let gate = Arc::new(gate);
    let bridge = Arc::new(
        AcpBridge::with_sink(gate.clone() as Arc<dyn EventSink>).scoped(format!("swarm/{member_id}")),
    );
    gate.attach(&bridge);
    let status = bridge
        .start(Some(cwd.to_path_buf()))
        .await
        .map_err(|e| format!("{member_id} could not start: {e}"))?;

    let session = bridge
        .request(
            "session/new",
            json!({
                "cwd": cwd.to_string_lossy(),
                // The user's own servers, and only those.
                //
                // This was `[]`, which told the agent it had no MCP servers at
                // all — so the code graph and web search the precondition panel
                // demands were unreachable from inside every run. A member is a
                // session like any other and gets the capabilities the user
                // registered.
                //
                // Not `mcp_servers_for_session`, which adds `vibedesk-swarm`:
                // that is the app's control surface, and a member that can start
                // a swarm is a member that can recurse. Capabilities yes,
                // control surface no.
                "mcpServers": crate::config::mcp_servers_for_acp(Some(cwd)),
            }),
        )
        .await
        .map_err(|e| format!("{member_id} could not open a session: {e}"))?;

    let session_id = session
        .get("sessionId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("{member_id} opened a session with no id"))?
        .to_string();

    bridge.set_session(Some(session_id.clone())).await;

    // The model is a config option, not a `session/new` field. A refusal here is
    // not fatal: the member runs on the session's default model and the roster
    // shows which one it actually got.
    let mut applied_model = config_value(&session, "model");
    if config_offers(&session, "model", model) {
        if let Ok(result) = bridge
            .request(
                "session/set_config_option",
                json!({
                    "sessionId": session_id,
                    "configId": "model",
                    "value": model,
                }),
            )
            .await
        {
            applied_model = config_value(&result, "model").or(applied_model);
        }
    }

    // The reasoning level, checked against what the session offers *for the
    // model it is now on*. The levels are per model — `glm-5-3` takes
    // `low, high, max`, `mistral-medium-3.5` takes `off, high` — so this has to
    // come after the model switch and be validated against the session that
    // answered it, not against a list read before the switch.
    let mut applied_thinking = config_value(&session, "thinking");
    if let Some(level) = thinking.map(str::trim).filter(|l| !l.is_empty()) {
        if config_offers(&session, "thinking", level) {
            if let Ok(result) = bridge
                .request(
                    "session/set_config_option",
                    json!({
                        "sessionId": session_id,
                        "configId": "thinking",
                        "value": level,
                    }),
                )
                .await
            {
                applied_thinking = config_value(&result, "thinking").or(applied_thinking);
            }
        }
    }

    // The agent, checked against what the session offers. `vibe-acp` rejects an
    // unknown value outright, so a blind set would leave the member on whatever
    // agent the session defaulted to and the roster would be wrong about it.
    //
    // The option is `agent`, not `mode`: 3.1.4 exposes `mode` as the permission
    // mode (`ask`, `accept-edits`, `auto-approve`) and `agent` as the agent the
    // session runs. Setting the role on `mode` is what made the first real run
    // after the audit fail at the first spawn with "the session does not offer it
    // … Offered: ask, accept-edits, auto-approve".
    //
    // An agent the session does not offer is the failure this whole path exists
    // to prevent: the member keeps the session default, which is Vibe's coding
    // prompt, and the lead implements the goal instead of planning it. The
    // precondition in `preconditions_with` refuses a run before this point, but a
    // precondition is a check and a check can be raced — the agent file can be
    // deleted between the panel reading it and the spawn. So a missing agent is an
    // error here, not a silent fallback.
    let mut applied_agent = config_value(&session, "agent").or_else(|| config_value(&session, "mode"));
    if let Some(agent) = agent {
        let Some(option) = agent_option(&session, agent) else {
            return Err(format!(
                "{member_id} could not run as `{agent}`: no agent or mode on this session is named that, so it would have run on Vibe's coding prompt instead. Expected {} to define it. Agents offered: {}. Modes offered: {}",
                crate::paths::vibe_home()
                    .join("agents")
                    .join(format!("{agent}.toml"))
                    .display(),
                listed(&config_options(&session, "agent")),
                listed(&config_options(&session, "mode")),
            ));
        };
        if let Ok(result) = bridge
            .request(
                "session/set_config_option",
                json!({
                    "sessionId": session_id,
                    "configId": option,
                    "value": agent,
                }),
            )
            .await
        {
            applied_agent = config_value(&result, option).or(applied_agent);
        }
    }

    Ok(Spawned {
        usage: gate.usage(),
        bridge,
        session_id,
        pid: status.pid,
        model: applied_model,
        thinking: applied_thinking,
        agent: applied_agent,
        watch,
    })
}

/// What a worker's worktree is, once it has been prepared.
///
/// A struct rather than a tuple because the last field is the one that makes a
/// speculative start reversible, and a map in the fifth slot of a tuple is a
/// value nobody reads the type of. The path and the branch are not here: both are
/// derived from the run and task ids by the caller that needs them, and a field
/// nothing reads is a field that goes stale.
struct WorkerPrep {
    /// The commit the task's work starts from: the run's base with every
    /// dependency merged in.
    base: String,
    /// The branch the worktree is on.
    ///
    /// Returned rather than assumed, because a task whose dependency's verdict
    /// failed has its branch discarded and its record cleared — and the re-run
    /// creates the branch again from the deterministic name without anything
    /// writing it back. The task then verifies with `branch: None`, and
    /// integration refuses it with "verified but has no branch" after the work
    /// is already done. The thing that creates the branch is the thing that
    /// knows its name.
    branch: String,
    /// Conflicts the plan itself resolved.
    resolved: Vec<worktree::Resolved>,
    /// The commit each dependency contributed, keyed by dependency task id.
    built_on: BTreeMap<String, String>,
}

/// Prepare a worker's worktree: its own branch, started from its dependencies.
///
/// Synchronous, and called from the drive loop rather than from the spawned
/// member, because the commit it returns has to be on the run the loop is about
/// to save. The first live run lost it: the spawned member wrote `base_commit`
/// to the authoritative run, and the loop then saved a clone taken before that
/// write, so every dependent task was proven against the run's base instead of
/// its dependency's tree — the exact bug the field exists to fix.
///
/// Idempotent, because a retried task keeps its branch: the verifier said the
/// work was wrong, not that it should be thrown away. A reused worktree returns
/// the base commit it was given the first time, so a retry does not move the
/// proof's base forward onto the failed attempt.
fn prepare_worker(run: &Run, task_id: &str) -> Result<WorkerPrep, String> {
    let task = run
        .task(task_id)
        .ok_or_else(|| format!("no task {task_id} to prepare"))?;
    let workspace = PathBuf::from(&run.workspace);
    let path = worktree::worktree_path(&run.id, &format!("w-{task_id}"));
    let branch = task
        .branch
        .clone()
        .unwrap_or_else(|| worktree::branch_name(&run.id, task_id));

    // A directory is not a worktree. `discard_worktree` removes the
    // registration and the contents, but on Windows a process that still holds
    // the directory open leaves it behind empty — and a bare `is_dir()` then
    // hands the worker a tree with no `.git` in it, which fails as "not a git
    // repository" and escalates the whole run. Ask the repository whether this
    // is a worktree, not the filesystem whether there is a directory.
    if path.is_dir() {
        if let Ok(at) = worktree::head(&CommandGit, &path) {
            let base = task.base_commit.clone().unwrap_or(at);
            // A reused worktree keeps the `built_on` it was given. Its branch
            // still contains exactly those commits, and re-reading the
            // dependency tips would record commits the branch does not have — a
            // dependency that has since been re-verified has a new tip this
            // tree never merged.
            return Ok(WorkerPrep {
                base,
                branch,
                resolved: Vec::new(),
                built_on: task.built_on.clone(),
            });
        }
        worktree::remove_stale_dir(&path)?;
    }

    worktree::add_worktree(&CommandGit, &workspace, &path, &branch, &run.integration.base)?;

    let deps: Vec<(String, String)> = task
        .deps
        .iter()
        .filter_map(|dep| {
            run.task(dep)
                .and_then(|t| t.branch.clone())
                .map(|branch| (dep.clone(), branch))
        })
        .collect();
    // The plan's ownership declaration is what resolves a conflict between two
    // dependencies. It was already in the run and nothing read it.
    let owners: Vec<(String, Vec<String>)> = run
        .tasks
        .iter()
        .map(|t| (t.id.clone(), t.files.clone()))
        .collect();
    let ownership = worktree::Ownership {
        current: task_id,
        tasks: &owners,
    };
    let (base, resolved) = worktree::merge_deps(&CommandGit, &path, &deps, &ownership)?;
    // What each dependency contributed, read from the repository rather than
    // from `merge_deps`: that function merges the branches and returns the
    // resulting head, not the tips it merged. Read after the merge, because the
    // merge is into this task's branch and leaves every dependency branch where
    // it was — so the tip read here is the commit that went in.
    //
    // A dependency whose branch cannot be read is left out rather than recorded
    // as empty. An absent entry means "this task was not built on that", which
    // is the safe reading: it can never cause a rollback of work that is fine.
    let built_on: BTreeMap<String, String> = deps
        .iter()
        .filter_map(|(id, branch)| {
            worktree::rev_parse(&CommandGit, &workspace, branch)
                .ok()
                .map(|commit| (id.clone(), commit))
        })
        .collect();
    Ok(WorkerPrep {
        base,
        branch,
        resolved,
        built_on,
    })
}

/// Throw away the work of every task that was built on a task whose verdict
/// failed.
///
/// The other half of the speculative start. A dependent may begin while its
/// dependency is still `Verifying`, which is what takes verification off the
/// critical path; when that verdict fails, the dependent's branch contains work
/// that has just been refused, and it has to go. The pure half — which tasks, and
/// what their state becomes — is [`schedule::invalidate_dependents`]. This is the
/// half that owns the filesystem and the processes.
///
/// Order matters. The member processes go first, because on Windows a worktree a
/// live process still holds open cannot be removed, and a worktree that survives
/// is a tree `prepare_worker` will reuse — with the refused dependency's commits
/// still in it, which is the one outcome worse than losing the work.
///
/// "Go first" has to mean *gone*, not *asked to stop*. A member's current
/// directory is its worktree, and a process's current directory holds a lock on
/// it. Dropping the bridge only sets `kill_on_drop`, which terminates the child
/// asynchronously, so the removal that followed raced the teardown and failed
/// with "os error 32" — and a task that cannot be prepared escalates the whole
/// run. `stop` kills the child and waits for it to exit, so the removal happens
/// after the lock is released.
///
/// The verifier's detached checkout goes before the branch does. `git branch -D`
/// refuses a branch that is checked out anywhere, and a verifier that was reading
/// the branch when the verdict landed would otherwise leave the branch
/// undeletable — so the next `worktree add -b` for that task would fail with
/// "branch already exists" and the task would never start again.
async fn invalidate_built_on(run: &mut Run, failed: &str) -> Vec<String> {
    // Which of them had actually started, read before the rollback clears the
    // record. A task that was only `Ready` has no branch to discard and no
    // process to stop: it goes back to waiting and nothing else.
    let started: Vec<String> = run
        .tasks
        .iter()
        .filter(|t| !t.built_on.is_empty())
        .map(|t| t.id.clone())
        .collect();
    let rolled = schedule::invalidate_dependents(&mut run.tasks, failed);
    if rolled.is_empty() {
        return rolled;
    }
    let workspace = PathBuf::from(&run.workspace);
    for id in &rolled {
        if !started.contains(id) {
            engine().post(
                BoardEntry::new(
                    "engine",
                    BoardKind::Note,
                    format!(
                        "{id} was waiting on {failed}, whose verdict failed — it waits for the next one"
                    ),
                )
                .about(id.clone()),
            );
            continue;
        }
        for member_id in [format!("w-{id}"), format!("v-{id}")] {
            end_member_turn(
                &member_id,
                run.member(&member_id).and_then(|m| m.session_id.clone()),
            );
            let bridge = {
                let engine = engine();
                let mut inner = engine.inner.lock().unwrap_or_else(|e| e.into_inner());
                inner.take_bridge(&member_id)
            };
            if let Some(bridge) = bridge {
                let _ = bridge.stop().await;
            }
            if let Some(member) = run.member_mut(&member_id) {
                // `Queued` is the honest state: the member exists on the roster
                // and has no process. Leaving it `working` would be the roster
                // claiming a turn that has been cancelled.
                member.state = MemberState::Queued;
                member.session_id = None;
                member.pid = None;
                member.nudges = 0;
            }
        }
        let verifier_path = worktree::worktree_path(&run.id, &format!("v-{id}"));
        let _ = worktree::remove_worktree(&CommandGit, &workspace, &verifier_path);
        let path = worktree::worktree_path(&run.id, &format!("w-{id}"));
        let branch = worktree::branch_name(&run.id, id);
        worktree::discard_worktree(&CommandGit, &workspace, &path, &branch);

        engine().post(
            BoardEntry::new(
                "engine",
                BoardKind::Note,
                format!(
                    "{id} was built on {failed}, whose verdict failed — its branch is discarded and it is back in the queue"
                ),
            )
            .about(id.clone()),
        );
    }
    rolled
}

/// The engine's own proof of a task's criterion, run before the verifier is
/// spawned.
///
/// The verifier's brief states this result as given rather than asking for it.
/// The engine already ran the criterion twice at verdict time to compute
/// `failing_first`, and the verifier was running it twice more to reach the same
/// answer — which is what made its turn 13+ minutes against the worker's 6, and
/// what the cancelled run's `v-t1` spent 20.7 minutes and 1,989,387 input tokens
/// on before it ran out of room and wrote no verdict.
///
/// The cost is real and accepted: the criterion now runs twice here and twice
/// again when the verdict arrives, because the verdict-time proof is what makes
/// the ruling earned rather than asserted and it proves the verifier's own
/// `command`, which may not be the criterion. The guard is what keeps it from
/// being wasted: a criterion the engine cannot run is not attempted at all, and
/// the brief says so instead of handing over a proof of nothing.
///
/// Off the runtime's thread, because this runs the criterion twice with a
/// two-minute deadline each and the drive loop's ticks must not wait behind it.
async fn criterion_proof(task: &Task, base: &Path, change: &Path) -> prove::Proof {
    let criterion = task.acceptance.trim().to_string();
    if criterion.is_empty() {
        return prove::Proof::unattempted("the task has no acceptance criterion to run");
    }
    // A criterion that is prose is refused by the plan validator, but a run
    // adopted from disk predates that gate and a criterion like "the tests pass"
    // slips through it. Handing a sentence to a shell produces exit 127 on both
    // sides, which reads as a broken command rather than as a criterion the
    // engine cannot run.
    if let Some(marker) = plan::prose_marker(&criterion) {
        return prove::Proof::unattempted(format!(
            "the criterion is not one shell command — it contains {marker}"
        ));
    }
    if !base.is_dir() || !change.is_dir() {
        return prove::Proof::unattempted(
            "the base checkout or the change is missing, so the criterion could not be run on both",
        );
    }

    let base = base.to_path_buf();
    let change = change.to_path_buf();
    tokio::task::spawn_blocking(move || {
        prove::prove(&base, &change, &criterion, prove::PROOF_TIMEOUT)
    })
    .await
    .unwrap_or_else(|error| prove::Proof::unattempted(format!("the proof did not run: {error}")))
}

/// Everything a verifier needs before its process exists.
///
/// The order is the point, and it is carried by the type rather than by the
/// sequence of statements in `spawn_for_task`: the proof is a field of this
/// value, so it is computed by the step that prepares the trees and before
/// `spawn_member` is called. A verifier's session is never opened to sit
/// unprompted while the criterion runs twice — on the critical path of the phase
/// whose purpose is to shorten that turn, four minutes of a live process doing
/// nothing is the same waste in a new place.
struct VerifierPrep {
    /// The directories the verifier may read outside its own worktree.
    readable: Vec<PathBuf>,
    /// The base checkout the brief names and the proof ran against.
    base_path: PathBuf,
    /// The engine's own two-sided run of the criterion.
    proof: prove::Proof,
}

/// Prepare a verifier's trees and prove the criterion, before anything is
/// spawned.
///
/// The worktree is detached at the branch under test, so the change side of the
/// proof is the worker's branch tip — the same commit the verifier will read.
/// The base side is the task's own base commit, because that is the tree the
/// criterion was written for: against the run's base a dependent task's test
/// fails only because its dependency is missing, which is a proof of nothing.
///
/// Returns the reason it could not prepare the worktree, which is fatal: a
/// verifier with no tree has nothing to read.
async fn prepare_verifier(
    run_id: &str,
    task: &Task,
    workspace: &Path,
    path: &Path,
    branch: &str,
    base_commit: &str,
) -> Result<VerifierPrep, String> {
    if path.is_dir() {
        let _ = worktree::remove_worktree(&CommandGit, workspace, path);
    }
    worktree::add_detached(&CommandGit, workspace, path, branch)
        .map_err(|error| format!("could not prepare a worktree: {error}"))?;

    let mut readable = Vec::new();
    let base_path = worktree::task_base_path(run_id, &task.id);
    if !base_path.is_dir()
        && worktree::add_detached(&CommandGit, workspace, &base_path, base_commit).is_err()
    {
        // Not fatal: the engine's own proof falls back to the run's base
        // checkout, and the verifier can still read the diff. Saying so is
        // better than a verifier that silently cannot check whether a case it
        // found is new.
        let _ = engine().post(
            BoardEntry::new(
                "engine",
                BoardKind::Note,
                format!(
                    "could not check out the base for {} — the verifier proves against the run base",
                    task.id
                ),
            )
            .about(&task.id),
        );
    } else {
        readable.push(base_path);
    }

    let base_path = readable
        .first()
        .cloned()
        .unwrap_or_else(|| worktree::base_checkout_path(run_id));
    let proof = criterion_proof(task, &base_path, path).await;
    Ok(VerifierPrep {
        readable,
        base_path,
        proof,
    })
}

/// Start one member for one task: worktree, process, session, brief.
///
/// Idempotent by construction. The guard is `bridges`, which is the same map the
/// engine uses to keep the process alive, so a tick that re-observes the same
/// transition cannot spawn a second process and double-spend the budget.
fn spawn_for_task(
    sink: Arc<dyn EventSink>,
    run_id: String,
    task_id: String,
    verify: bool,
    retry: Option<String>,
) {
    tokio::spawn(async move {
        let engine = engine();
        let member_id = if verify {
            format!("v-{task_id}")
        } else {
            format!("w-{task_id}")
        };

        {
            let inner = engine.inner.lock().unwrap_or_else(|e| e.into_inner());
            if inner.bridges.contains_key(&member_id) {
                return;
            }
        }

        let Some(run) = engine.run() else {
            return;
        };
        if run.id != run_id {
            return;
        }
        let Some(task) = run.task(&task_id).cloned() else {
            return;
        };
        let Some(member) = run.member(&member_id).cloned() else {
            return;
        };

        let workspace = PathBuf::from(&run.workspace);
        let path = worktree::worktree_path(&run_id, &member_id);
        let branch = task
            .branch
            .clone()
            .unwrap_or_else(|| worktree::branch_name(&run_id, &task_id));

        // What the task's work started from. The loop recorded it; this is the
        // copy the briefs are written against.
        let base_commit = task
            .base_commit
            .clone()
            .unwrap_or_else(|| run.integration.base.clone());

        // A worker's worktree was prepared by the drive loop, which had to know
        // the base commit before it saved the run. A verifier gets a detached
        // worktree so it has nowhere to commit — recreated on a retry, because a
        // stale checkout would be verifying the previous attempt's branch tip.
        // Neither is enforced by the filesystem: the engine detects a dirty base
        // rather than claiming it prevented one.
        //
        // A verifier also gets its task's base checkout, created here rather than
        // lazily by the proof, because the brief names it and a path that does
        // not exist yet is a path the verifier cannot use. The gate is told about
        // it too: without that the `cd` is refused as "outside workdir" and the
        // base checkout becomes a directory the member is not allowed to read.
        //
        // All of it — the trees and the engine's proof of the criterion — happens
        // before `spawn_member`, so the verifier's process is never opened to
        // wait on a proof.
        let mut readable = Vec::new();
        let mut base_path = worktree::base_checkout_path(&run_id);
        let mut proof = None;
        if verify {
            match prepare_verifier(&run_id, &task, &workspace, &path, &branch, &base_commit).await {
                Ok(prep) => {
                    readable = prep.readable;
                    base_path = prep.base_path;
                    proof = Some(prep.proof);
                }
                Err(error) => {
                    fail_member(&sink, &member_id, &error);
                    return;
                }
            }
        } else if !path.is_dir() {
            fail_member(
                &sink,
                &member_id,
                "the drive loop did not prepare a worktree for this task",
            );
            return;
        }

        let role = if verify { Role::Verifier } else { Role::Worker };
        // The ledger this member already has, if this is a respawn.
        //
        // Read before the spawn rather than after, because the spawn is slow and
        // the gate has to be writing into the same accumulation from its first
        // reading. A member's spend is keyed by member for the life of the run: a
        // respawn is a new session, not a new member.
        let ledger = {
            let inner = engine.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.usage.get(&member_id).cloned()
        };
        match spawn_member(
            sink.clone(),
            &member_id,
            role,
            &path,
            &member.model,
            member.thinking.as_deref(),
            Some(roles::agent_for(role)),
            readable,
            ledger,
        )
        .await
        {
            Ok(spawned) => {
                let Spawned {
                    bridge,
                    usage,
                    session_id,
                    pid,
                    thinking,
                    agent,
                    watch,
                    ..
                } = spawned;
                let inbox = inbox_dir(&run_id, &member_id);
                let _ = std::fs::create_dir_all(&inbox);

                let brief = if let Some(reason) = retry.as_deref() {
                    // A retry is a fresh process, not a nudge into the old one.
                    // The old session was cancelled when the worker posted its
                    // evidence, and a cancelled ACP session does not take another
                    // turn: the ninth and tenth live runs both had the retry post
                    // its claim and then go silent until the idle limit failed the
                    // run. The worktree and the commits survive the process, so
                    // the only thing a fresh worker needs is what the verifier
                    // said.
                    roles::retry_brief(&task, reason, &inbox.to_string_lossy())
                } else if verify {
                    // The proof was computed before the process existed, so the
                    // brief states the engine's result instead of asking the
                    // verifier to reproduce it. The cancelled run's `v-t1` spent
                    // 20.7 minutes and 1,989,387 input tokens on that
                    // duplication and ended with no verdict at all.
                    let proof = proof.unwrap_or_else(|| {
                        prove::Proof::unattempted(
                            "the engine did not prove the criterion before the spawn",
                        )
                    });
                    roles::verifier_brief(
                        &task,
                        &run_id,
                        &inbox.to_string_lossy(),
                        &base_commit,
                        &branch,
                        &base_path.to_string_lossy(),
                        &verifier_skills(&run, &task),
                        &proof,
                    )
                } else {
                    // The dependency handoff is a decision digest, not a
                    // transcript: a worker inherits what upstream decided, not
                    // how it thought.
                    let digest = {
                        let inner = engine.inner.lock().unwrap_or_else(|e| e.into_inner());
                        inner.board.digest_for(&task_id, &task.deps)
                    };
                    // What past runs of this workspace learned about the files
                    // this task owns. `gotchas_for` matches on the task's own
                    // files, so a gotcha about `src/rate.rs` reaches the worker
                    // editing it and no one else. The current run is excluded:
                    // its own reflection has not run yet, and a run that read its
                    // own half-written memory would be learning from itself.
                    let gotchas = gotchas_for(
                        &history::History::of(&run.workspace, Some(run_id.as_str())).gotchas(),
                        &task.files,
                    );
                    roles::worker_brief(
                        &task,
                        &digest,
                        &run_id,
                        &inbox.to_string_lossy(),
                        &base_commit,
                        &roles::ownership_map(&run.tasks, &task_id),
                        &worker_skills(&run, &task),
                        &roles::gotcha_section(&gotchas),
                    )
                };

                {
                    let mut inner = engine.inner.lock().unwrap_or_else(|e| e.into_inner());
                    inner.put_bridge(&member_id, bridge.clone());
                    inner.watches.insert(member_id.clone(), watch.clone());
                    // The same ledger the gate was handed, so a respawn inserts
                    // the accumulation it is already writing into rather than a
                    // fresh one that starts at zero.
                    inner.usage.insert(member_id.clone(), usage.clone());
                    if let Some(run) = inner.run.as_mut() {
                        if let Some(member) = run.member_mut(&member_id) {
                            member.session_id = Some(session_id.clone());
                            member.pid = pid;
                            // What the session reports, not what was asked for.
                            member.thinking = thinking.clone();
                            member.agent = agent.clone();
                            // Starting, not working: the process exists and the
                            // session is open, but no turn has been asked for
                            // yet. The turn start is what makes it `working`,
                            // and it is the only thing that can.
                            member.state = MemberState::Starting;
                            member.worktree = Some(path.to_string_lossy().to_string());
                            member.task = Some(task_id.clone());
                            member.last_at = Some(now());
                            // What this member was told to load. The evidence of
                            // whether it did comes from its own tool calls, not
                            // from here.
                            member.skills_assigned = task.skills.clone();
                            member.skills_loaded.clear();
                        }
                        run.touch();
                    }
                }
                if let Some(updated) = engine.run() {
                    let _ = updated.save();
                    emit(&sink, &updated);
                }

                drive_turn(&sink, &bridge, &member_id, &session_id, &brief).await;
            }
            Err(error) => fail_member(&sink, &member_id, &error),
        }
    });
}

/// Drive one member turn, and make a failed one visible.
///
/// The turn is where the model call happens, so it is where a provider that
/// refuses shows up. The result used to be discarded, and the cost was the whole
/// run: a member whose model call failed sat `working` until the thirty-minute
/// turn ceiling, and the run then died with "a turn in flight with no sign of
/// life" — a sentence that names neither the provider nor the error. A member
/// that cannot get an answer is a member that failed, and the reason belongs on
/// the roster where the user can read it.
///
/// A failure is retried, because a provider that refuses one request often
/// serves the next. The retry is bounded by the same `MAX_RESPAWNS` the process
/// restarts use, so a provider that is down for the whole run fails the task
/// instead of looping.
async fn drive_turn(
    sink: &Arc<dyn EventSink>,
    bridge: &Arc<AcpBridge>,
    member_id: &str,
    session_id: &str,
    text: &str,
) {
    // The quiet clock starts at the prompt, not at the member's last word. A
    // member that finished a turn ten minutes ago and was just given a new one is
    // not ten minutes silent; it has not been asked anything until now.
    //
    // This is also the one place a turn begins, for every role: the lead's plan,
    // a worker's brief, a verifier's brief, the reflector's pass and every nudge
    // all arrive here. The watch records the moment, and the drive loop folds it
    // onto the roster as `working` with a start time the panel can subtract from
    // its own clock.
    if let Some(watch) = watch_of(member_id) {
        watch.turn_started();
    }
    let result = bridge
        .turn(
            "session/prompt",
            json!({
                "sessionId": session_id,
                "prompt": [{ "type": "text", "text": text }],
            }),
        )
        .await;

    // The turn is over however it ended. A clock left running is a panel that
    // says a member has been working for forty minutes on a turn that ended in
    // the first one.
    let Err(error) = result else {
        stop_turn_clock(member_id);
        return;
    };
    // A turn that is still running, or one refused because another was already
    // in flight, is not a member that failed. The stall grace and the wedge
    // detector own those, and failing the member here would kill a turn that is
    // about to answer. The clock keeps running for the same reason: the turn it
    // is measuring has not ended.
    if matches!(
        error,
        crate::error::AppError::TurnInProgress | crate::error::AppError::TurnStalled(_)
    ) {
        return;
    }
    stop_turn_clock(member_id);
    // A close the engine caused is not a member that failed: the engine stops a
    // member whose part in the run is over.
    if engine().stopped_deliberately(member_id) {
        return;
    }
    let message = error.to_string();
    engine().post(
        BoardEntry::new(
            member_id,
            BoardKind::Escalation,
            format!("{member_id} could not get an answer: {message}"),
        )
        .about("run"),
    );
    restart_or_fail(sink, member_id, RestartCause::of(&error), &message);
}

/// How many times the agent process may be found dead in one run before the run
/// stops trying.
///
/// Small on purpose. An infra restart is not a slow model: the process is gone,
/// so every worker shares the cause and each restart spends a member's budget and
/// a task's context re-reading its brief. The audited run lost two workers in the
/// same eight seconds to exactly this, spent one of each member's two restarts on
/// it, and could not say afterwards which clock had fired. Three is enough to
/// rule out a one-off crash of a shared process; the fourth says the process is
/// the problem.
pub const MAX_INFRA_RESTARTS: u32 = 3;

/// Why a member is being restarted.
///
/// The audit's third fault: three liveness mechanisms — the evidence nudge, the
/// wedged limit and driver-process death — fed one restart path that logged
/// "restarted it on t3" and nothing else the log can be asked about. These are
/// not interchangeable. A process that is gone will not answer later; a model
/// that is thinking might; a provider that refused might serve the next call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartCause {
    /// The agent process is gone, was never reachable, or the transport to it
    /// closed: [`crate::error::AppError::AgentExited`], `AgentNotRunning`,
    /// `NoSession`, a missing binary or an io error on the pipe.
    Infra,
    /// The member was silent past [`MEMBER_IDLE_LIMIT_SECS`].
    Timeout,
    /// A provider or transport refused the call: an RPC error or a timeout.
    Provider,
}

impl RestartCause {
    /// Which clock fired, from the error the bridge returned.
    ///
    /// Classified by variant rather than by message text: the message is written
    /// for a person, and string-matching it would break silently the first time
    /// somebody rewords it.
    pub fn of(error: &crate::error::AppError) -> Self {
        use crate::error::AppError;
        match error {
            AppError::AgentNotRunning
            | AppError::AgentExited(_)
            | AppError::NoSession
            | AppError::BinaryNotFound(_)
            | AppError::Io(_) => RestartCause::Infra,
            AppError::TurnStalled(_) | AppError::Timeout(_) => RestartCause::Timeout,
            AppError::Rpc { .. } | AppError::Message(_) | AppError::Json(_) | AppError::Toml { .. } => {
                RestartCause::Provider
            }
            // The two startup states are not failures of a running member; they are
            // refusals the caller has to answer. Classed as infra because that is
            // the only one of the three that a restart cannot fix by waiting.
            AppError::AgentAlreadyRunning | AppError::TurnInProgress => RestartCause::Infra,
        }
    }

    /// How the board names it.
    pub fn label(self) -> &'static str {
        match self {
            RestartCause::Infra => "the agent process is gone",
            RestartCause::Timeout => "silent past the wedged limit",
            RestartCause::Provider => "the provider refused the call",
        }
    }
}

/// Whether the run has lost the agent process often enough to stop.
///
/// `Some(sentence)` means stop: the sentence is the failure the run records, and
/// it names the count and the cause so the next reader does not have to infer it
/// from a restart line. `None` means the run may keep trying.
fn infra_overloaded(run: &Run) -> Option<String> {
    let total: u32 = std::iter::once(&run.lead)
        .chain(run.members.iter())
        .map(|member| member.restarts_infra)
        .sum();
    if total <= MAX_INFRA_RESTARTS {
        return None;
    }
    Some(format!(
        "the agent process died {total} times in this run — stopped instead of restarting a {}th time",
        total + 1
    ))
}

/// Whether a restart could be charged, and why not when it could not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Charge {
    /// Charged. The member is now `Starting` with a fresh session slot.
    Allowed,
    /// A restart is already in flight for this member.
    AlreadyRestarting,
    /// The budget is gone. The member is failed, not retried.
    Exhausted,
}

/// Charge one restart against a member's budget, at most once per death.
///
/// The single place `respawns` moves. Two paths notice a dead member — a failed
/// turn (`after_turn_error`) and the idle clock (`respawn_dead`) — and they fire
/// independently: the audited run's w-t1 logged the nudge, the idle restart, the
/// failed nudge delivery and the error restart inside 29 ms, and finished at
/// `respawns = 2` having restarted once. One event, two slots of a two-slot
/// budget, so the member was never restarted again and sat `Working` with no
/// process for the rest of the run.
///
/// `Starting` is the flag that makes it idempotent: both paths set it as part of
/// charging, so whichever loses the race sees a restart already in flight rather
/// than a member to charge for.
fn charge_restart(member: &mut Member, cause: RestartCause) -> Charge {
    if member.state == MemberState::Starting {
        return Charge::AlreadyRestarting;
    }
    if member.respawns >= MAX_RESPAWNS {
        return Charge::Exhausted;
    }
    member.respawns += 1;
    match cause {
        RestartCause::Infra => member.restarts_infra = member.restarts_infra.saturating_add(1),
        RestartCause::Timeout => {
            member.restarts_timeout = member.restarts_timeout.saturating_add(1);
        }
        RestartCause::Provider => {
            member.restarts_provider = member.restarts_provider.saturating_add(1);
        }
    }
    member.state = MemberState::Starting;
    member.session_id = None;
    member.pid = None;
    member.nudges = 0;
    member.last_at = Some(now());
    Charge::Allowed
}

/// What to do about a member that could not get an answer.
///
/// Pure, so the decision is testable without a live agent: the effect is a
/// process spawn, and a test that has to stand one up is a test that is not run.
///
/// The same ceiling as a process restart, because from the run's side a member
/// that dies on spawn and a member whose provider refuses are the same problem:
/// something outside the run is not working, and the run has to decide how long
/// to wait for it. Two attempts is enough to tell a transient refusal from a
/// provider that is down.
///
/// The lead is the exception. It has no task to restart, and a run whose lead
/// cannot be reached has no plan and no way to get one, so the run fails with
/// the provider's own words rather than sitting in `planning` forever.
fn after_turn_error(
    run: &mut Run,
    member_id: &str,
    cause: RestartCause,
    error: &str,
) -> Option<Restart> {
    // A run that has ended has no members left to restart: the release at the end
    // of a run wakes every awaiting turn with a closed connection.
    if run.state.is_terminal() {
        if let Some(member) = run.member_mut(member_id) {
            member.error = Some(error.to_string());
            if member.state.is_live() {
                member.state = MemberState::Parked;
                member.parked_at = Some(now());
            }
        }
        return None;
    }
    let Some(member) = run.member_mut(member_id) else {
        return None;
    };
    let task = member.task.clone();
    let verify = member.role == Role::Verifier;
    let lead = member.role == Role::Lead;
    member.error = Some(error.to_string());
    let charge = charge_restart(member, cause);
    if lead {
        member.state = MemberState::Failed;
        run.state = RunState::Failed;
        run.failure = Some(format!("the lead could not get an answer: {error}"));
        return None;
    }
    match charge {
        // Another path noticed this death first and has already paid for the
        // restart. Returning here is what stops one process exit consuming two
        // slots of the budget.
        Charge::AlreadyRestarting => return None,
        Charge::Exhausted => {
            member.state = MemberState::Failed;
            let task = task?;
            if let Some(record) = run.task_mut(&task) {
                record.state = TaskState::Failed;
            }
            return None;
        }
        Charge::Allowed => {}
    }

    // Charged, and the count is now over the ceiling: stop the run rather than
    // spend another member's budget on a process that is not there. The member is
    // failed with the run so the roster and the run agree about why.
    if cause == RestartCause::Infra {
        if let Some(failure) = infra_overloaded(run) {
            run.state = RunState::Failed;
            run.failure = Some(failure);
            if let Some(member) = run.member_mut(member_id) {
                member.state = MemberState::Failed;
            }
            if let Some(task) = task.clone() {
                if let Some(record) = run.task_mut(&task) {
                    record.state = TaskState::Failed;
                }
            }
            return None;
        }
    }

    let task = task?;
    Some(Restart {
        member: member_id.to_string(),
        task,
        verify,
        cause,
        reason: Some(error.to_string()),
    })
}

/// Apply [`after_turn_error`] and, when it says to, start the member again.
fn restart_or_fail(
    sink: &Arc<dyn EventSink>,
    member_id: &str,
    cause: RestartCause,
    error: &str,
) {
    let engine = engine();
    let (snapshot, restart) = {
        let mut inner = engine.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(run) = inner.run.as_mut() else {
            return;
        };
        let restart = after_turn_error(run, member_id, cause, error);
        run.touch();
        (run.clone(), restart)
    };
    let _ = snapshot.save();
    write_run(&snapshot);
    emit(sink, &snapshot);
    {
        let mut inner = engine.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.take_bridge(member_id);
    }
    let Some(restart) = restart else {
        // Nothing to escalate: the member's part was settled before the close.
        if snapshot.state.is_terminal() {
            return;
        }
        // Either the lead, which has already failed the run, or a member that
        // has spent its restarts. Both are already written down; the board line
        // is what makes the reason findable without opening the roster.
        if let Some(failure) = snapshot.failure.clone() {
            engine.post(
                BoardEntry::new("engine", BoardKind::Escalation, failure).about("run"),
            );
        } else if let Some(task) = snapshot
            .members
            .iter()
            .find(|m| m.id == member_id)
            .and_then(|m| m.task.clone())
        {
            engine.post(
                BoardEntry::new(
                    "engine",
                    BoardKind::Escalation,
                    format!(
                        "{member_id} could not get an answer after {MAX_RESPAWNS} restarts, so {task} failed: {error}"
                    ),
                )
                .about(task),
            );
        }
        return;
    };
    // The clock is named, because the audit's finding was that none of the three
    // said which one had fired: "w-t3 stopped responding — restarted it on t3" was
    // the same line for a dead process, a wedged turn and a refused nudge.
    engine.post(
        BoardEntry::new(
            "engine",
            BoardKind::Note,
            format!(
                "{} could not get an answer on {} — {} — restarted it: {error}",
                restart.member,
                restart.task,
                restart.cause.label()
            ),
        )
        .about(&restart.task),
    );
    spawn_for_task(
        sink.clone(),
        snapshot.id.clone(),
        restart.task,
        restart.verify,
        restart.reason,
    );
}

/// Record a member failure as a named state rather than a silent absence.
fn fail_member(sink: &Arc<dyn EventSink>, member_id: &str, error: &str) {
    let engine = engine();
    let snapshot = {
        let mut inner = engine.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(run) = inner.run.as_mut() else {
            return;
        };
        if let Some(member) = run.member_mut(member_id) {
            member.state = MemberState::Failed;
            member.error = Some(error.to_string());
        }
        run.touch();
        run.clone()
    };
    let _ = snapshot.save();
    emit(sink, &snapshot);
}

/* ------------------------------------------------------------------ *
 * The inbox
 * ------------------------------------------------------------------ */

/// Where one member writes.
pub fn inbox_dir(run_id: &str, member_id: &str) -> PathBuf {
    super::run_dir(run_id).join("inbox").join(member_id)
}

fn receipts_dir(run_id: &str, member_id: &str) -> PathBuf {
    inbox_dir(run_id, member_id).join("receipts")
}

/// What a member writes into its inbox.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InboxEntry {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    task: Option<String>,
    #[serde(default)]
    to: Vec<String>,
    #[serde(default)]
    evidence: Vec<String>,
    /// The one command that fails on the base and passes on the change.
    #[serde(default)]
    command: Option<String>,
    /// The ruling the verifier committed to, when it committed to one.
    #[serde(default)]
    passed: Option<bool>,
    /// A verdict asking the lead for a decision instead of a retry.
    #[serde(default)]
    escalate: Option<String>,
}

/// What the engine writes back.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Receipt {
    accepted: bool,
    reason: String,
    seq: Option<u64>,
    at: String,
}

/// Whether a member's inbox carries board entries.
///
/// Every role does except the reflector, whose inbox carries the reflection
/// itself. One rule, stated once, because the alternative is a filename
/// exception inside the drain loop that the next reader has to notice.
fn drains_board_entries(role: Role) -> bool {
    role != Role::Reflector
}

fn parse_kind(value: &str) -> Option<BoardKind> {
    match value.trim().to_ascii_lowercase().as_str() {
        "claim" => Some(BoardKind::Claim),
        "decision" => Some(BoardKind::Decision),
        "evidence" => Some(BoardKind::Evidence),
        "verdict" => Some(BoardKind::Verdict),
        "conflict" => Some(BoardKind::Conflict),
        "escalation" => Some(BoardKind::Escalation),
        "note" => Some(BoardKind::Note),
        _ => None,
    }
}

/// Drain one member's inbox into the board.
///
/// Same discipline as the proposal mailbox: validate, apply or reject, write a
/// receipt, and remove the file so a rejected entry does not re-fire. A member
/// that writes garbage gets a receipt saying why rather than silence.
/// Files in a member's inbox that are not board entries.
///
/// The lead writes its plan to `plan.json` in the same directory it posts
/// decisions to, and the reflector writes `gotchas.json`. Draining those as
/// entries rejected them as malformed and deleted them — which is how a live run
/// sat in `planning` forever with a receipt saying "`` is not a board kind"
/// while the plan it was waiting for had already been thrown away.
const RESERVED_INBOX_FILES: &[&str] = &["plan", "gotchas", "amendment"];

/// Files in a member's inbox that are not entries and not reserved.
///
/// The brief says one `.json` file per entry. A member that writes
/// `evidence.jsonl` instead is not speaking, and the engine used to ignore the
/// file in silence: the twelfth live run's w-t3 wrote its evidence that way, the
/// drive loop never saw it, and the run died ten minutes later with "a member
/// stopped responding". A file that looks like an entry and is not one is a
/// protocol mistake, and the board is where a mistake gets said.
fn ignored_inbox_files(run: &Run, member_id: &str) -> Vec<String> {
    let dir = inbox_dir(&run.id, member_id);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<String> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .filter(|path| path.extension().map(|x| x != "json").unwrap_or(true))
        .filter_map(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().to_string())
        })
        .filter(|name| {
            // A file being written is not a mistake. The lead writes its plan
            // through a temp file and renames it, and reporting
            // `.plan.json.miob3653.tmp` as a protocol error put a false line on
            // the board of the fourteenth live run before the plan had even
            // landed.
            if name.starts_with('.') || name.ends_with(".tmp") || name.contains(".tmp.") {
                return false;
            }
            let stem = name.split('.').next().unwrap_or(name.as_str());
            !RESERVED_INBOX_FILES.contains(&stem)
        })
        .collect();
    out.sort();
    out
}

fn drain_inbox(run: &Run, member_id: &str) -> Vec<BoardEntry> {
    // The reflector writes a reflection, not board entries. Draining its inbox
    // would reject `gotchas.json` as a malformed entry and delete it, which is
    // the one file the run is waiting on.
    if !run
        .member(member_id)
        .map(|m| drains_board_entries(m.role))
        .unwrap_or(true)
    {
        return Vec::new();
    }
    let dir = inbox_dir(&run.id, member_id);
    let receipts = receipts_dir(&run.id, member_id);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let _ = std::fs::create_dir_all(&receipts);

    let mut accepted = Vec::new();
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().map(|x| x == "json").unwrap_or(false))
        .collect();
    // Deterministic order: two entries written in the same tick land in the
    // order their names sort, not in whatever order the filesystem returns.
    paths.sort();

    for path in paths {
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "entry".into());

        // A reserved name belongs to the run, not to the board. It is left in
        // place for the reader that is waiting on it.
        if RESERVED_INBOX_FILES.contains(&name.as_str()) {
            continue;
        }

        let body = match std::fs::read_to_string(&path) {
            Ok(body) => body,
            Err(error) => {
                write_receipt(&receipts, &name, false, format!("could not read: {error}"), None);
                let _ = std::fs::remove_file(&path);
                continue;
            }
        };

        let parsed: Result<InboxEntry, _> = serde_json::from_str(&body);
        let entry = match parsed {
            Ok(entry) => entry,
            Err(error) => {
                write_receipt(&receipts, &name, false, format!("not an entry: {error}"), None);
                let _ = std::fs::remove_file(&path);
                continue;
            }
        };

        let Some(kind) = parse_kind(&entry.kind) else {
            write_receipt(
                &receipts,
                &name,
                false,
                format!("`{}` is not a board kind", entry.kind),
                None,
            );
            let _ = std::fs::remove_file(&path);
            continue;
        };

        if entry.text.trim().is_empty() {
            write_receipt(&receipts, &name, false, "an entry needs text".into(), None);
            let _ = std::fs::remove_file(&path);
            continue;
        }

        // A verdict without evidence is discarded: the whole point of the
        // adversarial role is that the ruling is backed by a command.
        if kind == BoardKind::Verdict && entry.evidence.is_empty() {
            write_receipt(
                &receipts,
                &name,
                false,
                "a verdict needs evidence — one command and the line that settled it".into(),
                None,
            );
            let _ = std::fs::remove_file(&path);
            continue;
        }

        let mut board_entry = BoardEntry::new(member_id, kind, entry.text.trim());
        board_entry.task = entry.task.clone();
        board_entry.to = entry.to.clone();
        board_entry.evidence = entry.evidence.clone();
        board_entry.command = entry.command.clone();
        board_entry.passed = entry.passed;
        board_entry.escalate = entry.escalate.clone();
        accepted.push(board_entry);
        let _ = std::fs::remove_file(&path);
    }

    accepted
}

/* ------------------------------------------------------------------ *
 * The reflection pass
 * ------------------------------------------------------------------ */

/// What the reflection pass has produced so far.
///
/// Three outcomes rather than an `Option`, because "has not written yet" and
/// "wrote something that is not a reflection" are different states: the first
/// means wait, the second means the pass is broken and the run should record a
/// degraded reflection now rather than wait out the timeout.
enum ReflectionRead {
    Missing,
    Draft(reflect::ReflectionDraft),
    Malformed(String),
}

/// Read the reflection pass's output, consuming it.
///
/// Consumed so a later tick cannot re-apply a draft that was already accepted,
/// and so a malformed file is reported once rather than on every tick.
fn read_reflection(dir: &Path) -> ReflectionRead {
    let path = dir.join("gotchas.json");
    let Ok(body) = std::fs::read_to_string(&path) else {
        return ReflectionRead::Missing;
    };
    let _ = std::fs::remove_file(&path);
    match reflect::parse(&body) {
        Ok(draft) => ReflectionRead::Draft(draft),
        Err(error) => {
            // Kept beside the inbox rather than deleted: the user can see what
            // the pass actually wrote, which is the only way to tell a bad
            // prompt from a bad model.
            let _ = std::fs::write(dir.join("gotchas.rejected.txt"), &body);
            ReflectionRead::Malformed(error)
        }
    }
}

/// Start the reflection pass.
///
/// Its own process and its own session, like every other member, but no
/// worktree: it has nothing to change, and a worktree would give it somewhere to
/// change it. It reads the workspace and writes one file.
fn spawn_reflector(sink: Arc<dyn EventSink>, run_id: String) {
    tokio::spawn(async move {
        let engine = engine();
        {
            let inner = engine.inner.lock().unwrap_or_else(|e| e.into_inner());
            if inner.bridges.contains_key(REFLECTOR_ID) {
                return;
            }
        }

        let Some(run) = engine.run() else {
            return;
        };
        if run.id != run_id {
            return;
        }
        let Some(member) = run.member(REFLECTOR_ID).cloned() else {
            return;
        };

        let workspace = PathBuf::from(&run.workspace);
        let inbox = inbox_dir(&run_id, REFLECTOR_ID);
        let _ = std::fs::create_dir_all(&inbox);

        match spawn_member(
            sink.clone(),
            REFLECTOR_ID,
            Role::Reflector,
            &workspace,
            &member.model,
            member.thinking.as_deref(),
            Some(roles::agent_for(Role::Reflector)),
            Vec::new(),
            None,
        )
        .await
        {
            Ok(spawned) => {
                let Spawned {
                    bridge,
                    usage,
                    session_id,
                    pid,
                    thinking,
                    agent,
                    watch,
                    ..
                } = spawned;
                // Verdicts, conflicts, escalations and decisions only. A digest
                // over the whole board would be mostly claims, and a gotcha
                // derived from a claim is a guess with a citation.
                let digest = {
                    let inner = engine.inner.lock().unwrap_or_else(|e| e.into_inner());
                    roles::reflection_digest(inner.board.entries())
                };
                let brief = roles::reflection_brief(&run_id, &inbox.to_string_lossy(), &digest);

                {
                    let mut inner = engine.inner.lock().unwrap_or_else(|e| e.into_inner());
                    inner.put_bridge(REFLECTOR_ID, bridge.clone());
                    inner.watches.insert(REFLECTOR_ID.to_string(), watch.clone());
                    inner.usage.insert(REFLECTOR_ID.to_string(), usage.clone());
                    if let Some(run) = inner.run.as_mut() {
                        if let Some(member) = run.member_mut(REFLECTOR_ID) {
                            member.session_id = Some(session_id.clone());
                            member.pid = pid;
                            member.thinking = thinking.clone();
                            member.agent = agent.clone();
                            member.state = MemberState::Working;
                            member.last_at = Some(now());
                        }
                        run.touch();
                    }
                }
                if let Some(updated) = engine.run() {
                    let _ = updated.save();
                    emit(&sink, &updated);
                }

                drive_turn(&sink, &bridge, REFLECTOR_ID, &session_id, &brief).await;
            }
            Err(error) => fail_member(&sink, REFLECTOR_ID, &error),
        }
    });
}

fn write_receipt(dir: &Path, name: &str, accepted: bool, reason: String, seq: Option<u64>) {
    let receipt = Receipt {
        accepted,
        reason,
        seq,
        at: now(),
    };
    if let Ok(body) = serde_json::to_string_pretty(&receipt) {
        let _ = std::fs::write(dir.join(format!("{name}.json")), body);
    }
}

/* ------------------------------------------------------------------ *
 * The drive loop
 * ------------------------------------------------------------------ */

/// How many times one member's process may be restarted before the run gives up.
const MAX_RESPAWNS: u32 = 2;

/// How many times the lead may be asked to resolve the same task's dependency
/// conflict before the task is failed for real.
///
/// Two, because the first amendment is the lead reading the conflict and the
/// second is the lead reading why its first answer did not work. A third is a
/// lead that cannot see the problem, and a run that keeps asking is a run that
/// never ends.
const MAX_CONFLICT_REPAIRS: u32 = 2;

/// One member the engine decided to restart, and what to tell it.
#[derive(Debug)]
struct Restart {
    member: String,
    task: String,
    verify: bool,
    /// Which clock fired, carried so the board line and the panel can name it
    /// rather than saying "restarted it" and leaving the reader to guess.
    cause: RestartCause,
    reason: Option<String>,
}

/// Restart a member whose process is gone while its task still needs it.
///
/// A stalled turn used to keep `busy` set forever, so the engine waited its
/// 1800s turn ceiling for a reply that was never coming: the sixth live run's
/// w-t1 sat at 0.015s of CPU for half an hour while the run showed `running`.
/// The bridge now kills a turn that has been stalled past its grace, which
/// leaves a member with no process and a task that still needs one.
///
/// Failing the run there throws away a worktree full of commits. A fresh process
/// with the retry brief is the same recovery the engine already uses for a
/// refused verdict, and it is bounded so a member that dies on spawn cannot loop.
///
/// The restarts are returned rather than spawned here, so the decision is
/// testable without a live agent.
fn respawn_dead(sink: &Arc<dyn EventSink>, run: &Run, busy: &dyn Fn(&str) -> bool) -> Option<(Run, Vec<Restart>)> {
    let mut next = run.clone();
    let mut restarted: Vec<(String, String, bool)> = Vec::new();
    for member in std::iter::once(&next.lead).chain(next.members.iter()) {
        if member.state != MemberState::Working || member.respawns >= MAX_RESPAWNS {
            continue;
        }
        let Some(task_id) = member.task.clone() else {
            continue;
        };
        let Some(task) = next.task(&task_id) else {
            continue;
        };
        let expected = match member.role {
            Role::Worker => task.state == TaskState::Running,
            Role::Verifier => task.state == TaskState::Verifying,
            Role::Lead | Role::Reflector => false,
        };
        if !expected || busy(&member.id) {
            continue;
        }
        let Some(idle) = silence_secs(member) else {
            continue;
        };
        if idle <= MEMBER_IDLE_LIMIT_SECS {
            continue;
        }
        restarted.push((member.id.clone(), task_id, member.role == Role::Verifier));
    }
    if restarted.is_empty() {
        return None;
    }
    next.touch();
    let mut restarts = Vec::new();
    for (id, task_id, verify) in restarted {
        // Charged through the one owner, and skipped when another path has
        // already paid for this death or the budget is gone. The old code
        // incremented unconditionally here *and* in `after_turn_error`, so one
        // process exit could spend both slots.
        let allowed = next
            .member_mut(&id)
            .map(|member| charge_restart(member, RestartCause::Timeout))
            .unwrap_or(Charge::Exhausted);
        if allowed != Charge::Allowed {
            continue;
        }
        {
            let mut inner = engine().inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.take_bridge(&id);
        }
        // A verifier gets its brief again; a worker gets what the verifier said,
        // because the worktree it is restarting into already carries its work.
        let reason = if verify {
            None
        } else {
            Some(
                next.task(&task_id)
                    .and_then(|t| t.verdict.as_ref())
                    .map(|v| v.reason.clone())
                    .unwrap_or_else(|| {
                        "your process stopped responding and was restarted. The worktree                          and its commits are intact — continue from them."
                            .into()
                    }),
            )
        };
        restarts.push(Restart {
            member: id,
            task: task_id,
            verify,
            cause: RestartCause::Timeout,
            reason,
        });
    }
    if restarts.is_empty() {
        return None;
    }
    // Saved after the charges, not before: a crash between the two would leave
    // the restart on disk with no budget spent against it.
    let _ = next.save();
    write_run(&next);
    emit(sink, &next);
    Some((next, restarts))
}

/// Advance the run until it is terminal or cancelled.
/// Move `last_at` forward for members whose worktree HEAD has moved.
///
/// The board is the primary signal that a member is alive, and it is the only
/// one that carries evidence. It is not the only one that carries *work*: a
/// worker that reads four files and then commits has done more than one that
/// posted a claim, and the engine used to see only the claim. Two live runs
/// died of that blindness — one nudged a committing worker twice, the next
/// failed a worker for silence after it had committed.
///
/// Only members already past the nudge threshold are probed, so a healthy run
/// pays nothing. The first probe records a baseline and rescues nothing: a
/// member that commits once and then stops is still a member that stopped.
/// Fold every member's skill loads onto the run, and recompute the gaps.
///
/// The loads come from the member's own `tool_call` notifications, so this is
/// the one place the run learns whether the method it handed out was used. A gap
/// is an assigned skill nobody loaded; it is recorded rather than punished,
/// because a task can be small enough to need no method at all — but a run whose
/// every assignment went unloaded is a run that paid for a brief nobody read, and
/// the ledger should say so.
fn sync_skill_loads(sink: &Arc<dyn EventSink>, run: Run) -> Run {
    let watches: Vec<(String, Vec<String>)> = {
        let inner = engine().inner.lock().unwrap_or_else(|e| e.into_inner());
        inner
            .watches
            .iter()
            .map(|(id, watch)| (id.clone(), watch.loaded()))
            .collect()
    };
    if watches.is_empty() {
        return run;
    }

    let mut next = run;
    let mut moved = false;
    for (id, loaded) in watches {
        if loaded.is_empty() {
            continue;
        }
        let member = if id == "lead" {
            Some(&mut next.lead)
        } else {
            next.member_mut(&id)
        };
        if let Some(member) = member {
            if member.skills_loaded != loaded {
                member.skills_loaded = loaded;
                moved = true;
            }
        }
    }

    // The gaps are a function of the whole run, so they are recomputed rather
    // than accumulated: a retry that loads the skill closes the gap, and a gap
    // that only ever grew would report a run that fixed itself as broken.
    // A task has two members on it — the worker that wrote it and the verifier
    // that tried to break it — and either may open the skill. The gap is the
    // skill nobody on the task opened, so the union is the honest reading; a
    // verifier that read the method and a worker that did not is a task that was
    // worked with the method.
    let mut gaps: Vec<String> = Vec::new();
    for task in &next.tasks {
        // A task nobody has been spawned on has not been worked, so it cannot
        // have skipped a skill. Counting it would report every assignment as a
        // gap for the whole of the approval wait, which is the one moment the
        // user is reading the plan.
        if task.owner.is_none() && task.verifier.is_none() {
            continue;
        }
        let mut loaded: Vec<String> = Vec::new();
        for id in [task.owner.as_deref(), task.verifier.as_deref()].into_iter().flatten() {
            if let Some(member) = next.member(id) {
                for name in &member.skills_loaded {
                    if !loaded.contains(name) {
                        loaded.push(name.clone());
                    }
                }
            }
        }
        for name in skills::unloaded(&task.skills, &loaded) {
            if !gaps.contains(&name) {
                gaps.push(name);
            }
        }
    }
    if next.skill_gaps != gaps {
        next.skill_gaps = gaps;
        moved = true;
    }

    if moved {
        next.touch();
        let _ = next.save();
        write_run(&next);
        emit(sink, &next);
    }
    next
}

/// How long a task has been running, and `None` before it started.
fn task_age_secs(task: &Task) -> Option<i64> {
    let started = task.started_at.as_deref()?;
    let at = chrono::DateTime::parse_from_rfc3339(started).ok()?;
    Some((chrono::Utc::now() - at.with_timezone(&chrono::Utc)).num_seconds())
}

/// How long a running task has gone without a commit, if that is worth reporting.
///
/// `head` is the branch tip read this tick. A task whose tip is still the base it
/// started from holds no commits, so twenty minutes of silence there is not a
/// slow model — it is no work. `None` means "nothing to say": not running, already
/// reported, the head moved, or still inside `STALL_SECS`.
fn stalled_secs(task: &Task, head: &str) -> Option<i64> {
    if task.stalled || task.state != TaskState::Running {
        return None;
    }
    if task.base_commit.as_deref() != Some(head) {
        return None;
    }
    let age = task_age_secs(task)?;
    (age >= STALL_SECS).then_some(age)
}

fn refresh_progress(sink: &Arc<dyn EventSink>, run: Run) -> Run {
    let mut next = run.clone();
    let mut rescued: Vec<(String, String)> = Vec::new();
    // Every head this tick read, with the task its member is on. Collected rather
    // than acted on inside the loop: the stall rule has to read the task, and the
    // loop holds the roster mutably.
    let mut probed: Vec<(String, String, Option<String>)> = Vec::new();
    for member in std::iter::once(&mut next.lead).chain(next.members.iter_mut()) {
        if member.state != MemberState::Working {
            continue;
        }
        let Some(idle) = silence_secs(member) else {
            continue;
        };
        if idle < LEAD_NUDGE_SECS {
            continue;
        }
        // A probe is a subprocess, and the loop ticks every 400ms. Reading the
        // HEAD on every tick made a member that was quietly reading files spend
        // most of the loop's budget spawning git; the probe only has to be
        // frequent enough to notice a commit before the nudge threshold.
        if !head_probe_due(member) {
            continue;
        }
        let Some(head) = worktree_head(member) else {
            continue;
        };
        member.head_at = Some(now());
        let moved = member.head.as_deref().is_some_and(|seen| seen != head);
        member.head = Some(head.clone());
        probed.push((member.id.clone(), head.clone(), member.task.clone()));
        if moved {
            member.last_at = Some(now());
            rescued.push((member.id.clone(), head));
        }
    }

    // A worker whose branch tip is still the base it started from, twenty minutes
    // in, has committed nothing. That is the case the audited run could not see:
    // five of eight worktrees held README.md and .git while reporting `working`.
    // Reported once per task, and not acted on — a worker about to commit and a
    // worker that is stuck look the same from here.
    let mut stalled: Vec<(String, String, i64)> = Vec::new();
    for (member_id, head, task_id) in probed {
        let Some(task_id) = task_id else { continue };
        let Some(task) = next.task(&task_id) else { continue };
        let Some(age) = stalled_secs(task, &head) else {
            continue;
        };
        if let Some(task) = next.task_mut(&task_id) {
            task.stalled = true;
        }
        stalled.push((member_id, task_id, age));
    }

    if rescued.is_empty() && stalled.is_empty() {
        return run;
    }
    next.touch();
    let _ = next.save();
    write_run(&next);
    emit(sink, &next);
    for (id, head) in rescued {
        let short = &head[..7.min(head.len())];
        engine().post(
            BoardEntry::new(
                "engine",
                BoardKind::Note,
                format!("{id} committed {short} — working, not silent"),
            )
            .about("run"),
        );
    }
    for (member_id, task_id, age) in stalled {
        engine().post(
            BoardEntry::new(
                "engine",
                BoardKind::Conflict,
                format!(
                    "{member_id} has committed nothing on {task_id} in {age}s — its branch is still the commit it started from"
                ),
            )
            .about(task_id),
        );
    }
    next
}

/// Whether a member's worktree HEAD is due for a read.
///
/// Fails open. A member with no reading, or one whose timestamp cannot be
/// parsed, is probed: a probe that runs too often costs 46ms, and a probe that
/// never runs costs the run a worker it failed for silence after it committed.
fn head_probe_due(member: &Member) -> bool {
    let Some(probed) = member.head_at.as_deref() else {
        return true;
    };
    let Ok(probed) = chrono::DateTime::parse_from_rfc3339(probed) else {
        return true;
    };
    let since = (chrono::Utc::now() - probed.with_timezone(&chrono::Utc)).num_seconds();
    since >= HEAD_PROBE_SECS
}

/// The worktree HEAD of a member, when it has one.
///
/// `None` covers every case that is not a reading: no worktree, no git, a
/// worktree that git no longer knows about. The caller treats `None` as "no
/// evidence of work", which is what it is — the board remains the primary
/// signal and this only ever rescues a member that is committing without
/// talking.
fn worktree_head(member: &Member) -> Option<String> {
    let path = member.worktree.as_deref()?;
    worktree::head(&CommandGit, std::path::Path::new(path)).ok()
}

/// Fail the run if a member that still has work has gone silent.
///
/// Silence only means a wedge when the member still has something to do. A worker
/// whose task is verified, or a verifier whose verdict is in, is idle by design —
/// and a run that fails because a finished member stopped talking is a run that
/// punishes the swarm for working. The first live run died exactly that way: w-t1
/// was silent for 601s after its task had already been verified.
fn wedge(
    run: &Run,
    busy: &dyn Fn(&str) -> bool,
    pulse: &dyn Fn(&str) -> super::watch::Pulse,
) -> Option<Run> {
    // Whether a member is still expected to be talking, which depends on the role
    // as well as the task. A worker's part ends the moment its evidence lands and
    // the task moves to `Verifying`; a verifier's part ends when the verdict is
    // in. The lead has no task, so it is expected for as long as it is working.
    let expected = |member: &Member| -> bool {
        let Some(task_id) = member.task.as_deref() else {
            return true;
        };
        let Some(task) = run.task(task_id) else {
            return true;
        };
        match member.role {
            Role::Worker => task.state == TaskState::Running,
            Role::Verifier => task.state == TaskState::Verifying,
            Role::Lead | Role::Reflector => true,
        }
    };

    let mut next = run.clone();
    let mut wedged: Vec<String> = Vec::new();
    for member in std::iter::once(&mut next.lead).chain(next.members.iter_mut()) {
        if member.state != MemberState::Working {
            continue;
        }
        if !expected(member) {
            continue;
        }
        let Some(last) = member.last_at.as_deref() else {
            continue;
        };
        let Ok(at) = chrono::DateTime::parse_from_rfc3339(last) else {
            continue;
        };
        let idle = (chrono::Utc::now() - at.with_timezone(&chrono::Utc)).num_seconds();

        // A turn in flight is work, not silence. `last_at` only moves when the
        // member writes to the board, so a verifier that spends ten minutes
        // running a suite looks exactly like one that died — and the first live
        // run failed for it. The bridge is the only thing that can tell them
        // apart.
        //
        // But a turn in flight is not work forever. A provider that stops
        // answering keeps `busy` set with no response coming, so a turn with no
        // sign of life for half an hour is a hang, and the ceiling is what stops
        // the run waiting on it until the wall-clock budget runs out.
        //
        // Half an hour is the wrong answer when the stream itself can say. Every
        // chunk of an answer arrives as a notification, so a turn that has said
        // nothing since it began is not busy — it is a model call that is not
        // answering. The twenty-first live run spent twenty-five minutes on two
        // workers in exactly that state, with the proxy returning 400s the whole
        // time, and the run could not tell it from work. The pulse is that
        // distinction: quiet with a tool in flight is the tool's silence and gets
        // the full ceiling; quiet with nothing in flight is the model's and gets
        // `MODEL_SILENCE_SECS`.
        let in_flight = busy(&member.id);
        let pulse = if in_flight {
            pulse(&member.id)
        } else {
            super::watch::Pulse::default()
        };
        let model_silent = in_flight
            && !pulse.tool
            && pulse.quiet.is_some_and(|quiet| quiet > MODEL_SILENCE_SECS);
        let limit = if in_flight {
            MEMBER_TURN_CEILING_SECS
        } else {
            MEMBER_IDLE_LIMIT_SECS
        };
        if model_silent || idle > limit {
            member.state = MemberState::Failed;
            let why = if model_silent {
                // The provider is named because the user cannot act on "no sign
                // of life" — the model is the thing that did not answer, and the
                // roster shows which one it was.
                let quiet = pulse.quiet.unwrap_or_default();
                let model = if member.model.is_empty() {
                    "its model"
                } else {
                    member.model.as_str()
                };
                format!("{model} has not answered in {quiet}s")
            } else if in_flight {
                format!("a turn in flight for {idle}s with no sign of life")
            } else {
                format!("silent for {idle}s")
            };
            member.error = Some(why.clone());
            wedged.push(format!("{} ({why})", member.id));
        }
    }
    if wedged.is_empty() {
        return None;
    }
    next.state = RunState::Failed;
    // Named, not summarised. "A member stopped responding" was the reason the
    // twelfth live run died with, and it named neither the member nor what it
    // was waiting for — the user had to read the roster to find out, and the
    // roster said `working`, which was the one thing that was not true.
    next.failure = Some(format!("a member stopped responding: {}", wedged.join(", ")));
    next.touch();
    Some(next)
}

/// One loop, one owner. Every step reads the run, decides, writes it back and
/// emits, so the panel is never more than one tick behind and a crash leaves a
/// run that says what it was doing.
/// What a stopped member is actually asking.
///
/// Every question used to funnel into one `escalate()` with one two-token budget,
/// so two unrelated questions spent the same pot and the third fell to the user.
/// The thirteenth live run is the shape: a criterion that passed on the base
/// commit (the plan's fault) and a worker that breached its grant (the worker's
/// fault) both drew on `amendments`, and the run stopped with the budget empty
/// and neither question answered.
///
/// The classes are not decoration. Each one has an owner who can actually answer
/// it, and a budget that belongs to that owner, so a question about the criterion
/// cannot spend the budget a question about the grant needs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Question {
    /// The criterion cannot tell the change from the base commit. The plan wrote
    /// it, so the plan gate owns it and `plan_attempts` pays.
    CriterionUnprovable,
    /// A member cannot carry the plan out as written: a grant too narrow, an
    /// instruction that contradicts another, a dependency that will not merge.
    /// The lead owns the plan, so `amendments` pays.
    PlanNeedsChanging,
}

/// Who answers a question.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Owner {
    PlanGate,
    Lead,
    User,
}

/// Which counter a question spends.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Budget {
    PlanAttempts,
    Amendments,
    None,
}

/// Where a question goes, and what it costs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Route {
    owner: Owner,
    state: RunState,
    budget: Budget,
}

/// Route a question to its owner, on that owner's budget.
///
/// The fallback is not a third kind of question. It is the same question with
/// nobody left who can answer it: the owner has spent its budget, so the user
/// gets it. There is no `ForTheUser` variant because the engine cannot tell a
/// product decision from a plan defect, and a class it cannot recognise would be
/// a branch that never runs.
///
/// Pure, so the rule is testable without a live agent — which is the whole point:
/// the old rule was one comparison against one counter, and it was wrong in a way
/// only a live run could show.
fn route_question(run: &Run, question: Question) -> Route {
    match question {
        Question::CriterionUnprovable if run.plan_attempts < PLAN_ATTEMPTS => Route {
            owner: Owner::PlanGate,
            state: RunState::Replanning,
            budget: Budget::PlanAttempts,
        },
        Question::PlanNeedsChanging if run.amendments < amend::MAX_AMENDMENTS => Route {
            owner: Owner::Lead,
            state: RunState::Replanning,
            budget: Budget::Amendments,
        },
        _ => Route {
            owner: Owner::User,
            state: RunState::Escalated,
            budget: Budget::None,
        },
    }
}

/// Whether a board entry belongs to the task's current attempt.
///
/// A retry returns the task to `Running` (or `Verifying`) with the previous
/// attempt's evidence and verdict still on the board, so both handlers have to
/// ignore anything posted at or before the attempt started. The task state is
/// not a guard on its own: it says the task is waiting for a result, not which
/// attempt the result belongs to.
///
/// Both live failures this prevents are real. The second run re-verified a task
/// one second after its retry began, on the refused attempt's evidence. The
/// eleventh run failed a task whose worker had just delivered: the evidence put
/// the task in `Verifying`, and the next tick re-read the first attempt's
/// verdict, applied it again, found the attempt ceiling reached and failed the
/// task — which is what turned a delivered task into "no task can make progress".
fn entry_is_current(entry: &BoardEntry, task: &Task) -> bool {
    match task.started_at.as_deref() {
        Some(started) => entry.at.as_str() > started,
        None => true,
    }
}

/// What a verdict entry rules, and `None` when it does not say.
///
/// The boolean first. A verifier that has to write `"passed": false` cannot
/// produce a verdict that opens with "passed" and means the opposite, which is
/// the ambiguity the first-word parse was invented to survive: the first live run
/// marked a task verified on a verdict that opened with "FAILED. The mechanical
/// criterion passes on the change ... but it is too weak to catch X".
///
/// The first-word parse stays as the fallback for an entry that omits the field —
/// an entry written before the field existed, or a verifier that wrote only
/// prose. `None` means neither was there, and the caller reads that as a failure:
/// a malformed verdict must never be the reason a task is accepted.
fn verdict_ruling(entry: &BoardEntry) -> Option<bool> {
    entry.passed.or_else(|| schedule::ruling(&entry.text))
}

/// The member escalation that should stop this run, if any.
///
/// Pure, and separate from the drive loop, because the loop's escalation branch
/// cannot be exercised without a live agent: this is the one part of it that can
/// be proven. `from != "engine"` matters — the engine posts its own escalations
/// for plan violations, and those are handled by sending the plan back to the
/// lead, not by stopping the run.
/// Stop the run and put the question to whoever can answer it.
///
/// The lead answers first. A worker that broke a file no task owns, or a
/// verifier that found a criterion which cannot be proven, is asking a
/// coordination question, and the lead is the coordinator: the file belongs to a
/// task and the plan simply did not say so, or the criterion is the lead's own
/// writing and cannot distinguish the change from the base. Only when the lead
/// has spent its amendment budget does the question become the user's.
///
/// Returns the run to carry on with, or `None` when it stopped for the user and
/// the drive loop must end. Extracted from the tick because there are two ways
/// to escalate — a member writing to its inbox, and the engine finding an
/// unprovable criterion itself — and a second copy of this transition is a
/// second place for it to drift.
fn escalate(
    sink: &Arc<dyn EventSink>,
    run: &Run,
    entry: &BoardEntry,
    question: Question,
) -> Option<Run> {
    let mut next = run.clone();
    let route = route_question(&next, question);
    match route.budget {
        Budget::PlanAttempts => next.plan_attempts += 1,
        Budget::Amendments => next.amendments += 1,
        Budget::None => {}
    }
    next.state = route.state;
    if route.owner == Owner::User {
        next.failure = Some(entry.text.clone());
    }
    for member in std::iter::once(&mut next.lead).chain(next.members.iter_mut()) {
        if member.state.is_live() {
            member.state = MemberState::Parked;
            member.parked_at = Some(now());
        }
    }
    if route.owner != Owner::User {
        if let Some(lead) = next.member_mut("lead") {
            lead.state = MemberState::Working;
            lead.last_at = Some(now());
        }
    }
    next.touch();
    let _ = next.save();
    write_run(&next);
    emit(sink, &next);
    engine().post(
        BoardEntry::new(
            "engine",
            BoardKind::Note,
            match route.owner {
                // The criterion is the plan's writing, so the plan gate pays and
                // the amendment budget is untouched. Naming the budget on the
                // board is what makes the routing visible instead of inferred.
                Owner::PlanGate => format!(
                    "{} stopped — the criterion is the plan's, so the plan gate pays (revision {} of {PLAN_ATTEMPTS})",
                    entry.from, next.plan_attempts
                ),
                Owner::Lead => format!(
                    "{} stopped — asked the lead to amend the plan (amendment {} of {})",
                    entry.from,
                    next.amendments,
                    amend::MAX_AMENDMENTS
                ),
                Owner::User => format!(
                    "{} asked for a decision — the run stopped here, worktrees kept",
                    entry.from
                ),
            },
        )
        .about("run"),
    );
    if route.owner == Owner::User {
        return None;
    }
    let inbox = inbox_dir(&next.id, "lead");
    let tasks = next
        .tasks
        .iter()
        .map(|t| {
            format!(
                "- {} — {}: {}",
                t.id,
                t.title,
                if t.files.is_empty() {
                    "(no files declared)".to_string()
                } else {
                    t.files.join(", ")
                }
            )
        })
        .collect::<Vec<_>>()
        .join("
");
    nudge_member(
        sink,
        &next,
        "lead",
        roles::amendment_brief(&entry.text, &inbox.to_string_lossy(), &tasks),
    );
    Some(next)
}

fn escalation_stopping(posted: &[BoardEntry]) -> Option<&BoardEntry> {
    posted
        .iter()
        .find(|e| e.kind == BoardKind::Escalation && e.from != "engine")
}

/// What a member's watch knows that the roster has to show.
///
/// Two facts, both observed rather than reported: whether the gate is holding a
/// request open for it, and when its current turn was asked for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Live {
    blocked: bool,
    turn_at: Option<String>,
}

/// Fold every member's ledger onto the run. Returns whether anything moved.
///
/// Copying is idempotent because the ledger holds totals, so a tick that changes
/// nothing writes the same numbers back. Without this the roster showed zero
/// tokens for every member for the whole run: the fields existed and nothing ever
/// wrote them.
fn fold_usage(run: &mut Run, ledgers: &[(String, Usage)]) -> bool {
    let mut moved = false;
    for (id, usage) in ledgers {
        if usage.is_empty() {
            continue;
        }
        if let Some(member) = run.member_mut(id) {
            if member.usage() != *usage {
                member.set_usage(usage);
                moved = true;
            }
        }
    }
    moved
}

/// Fold the live facts onto the roster. Returns whether anything moved.
///
/// The gate and the watch see these moments; the drive loop is the only place
/// that saves the run, so it is the only place that can write them down. A direct
/// write from the gate would be lost: the loop works from a snapshot taken at the
/// top of the tick and writes that snapshot back, so anything another task wrote
/// in between is overwritten on the next save.
///
/// A turn in flight is `working`, and a turn in flight with a request open is
/// `waiting`. Nothing else is the watch's to say. A member with no turn in flight
/// keeps whatever the engine last said, because the engine's own transitions —
/// parked, done, failed — are the ones that end a member's part in the run.
fn fold_live(run: &mut Run, live: &[(String, Live)]) -> bool {
    let mut moved = false;
    for (id, facts) in live {
        let Some(member) = run.member_mut(id) else {
            continue;
        };
        if member.turn_started_at != facts.turn_at {
            // A turn that has ended is a span. `turn_started_at` is cleared here,
            // so without this the run keeps no record of when a member worked —
            // and the timeline cannot draw a verifier row from a field that is
            // `None` between turns.
            //
            // A start that moved without passing through `None` is the same
            // event seen late: `turn_started` and `turn_ended` both run inside
            // one prompt, so a turn that began and ended between two ticks
            // arrives as a new start rather than as an end. The old turn is over
            // either way, and the span is the record of it.
            //
            // The end is this tick's clock rather than the watch's, because the
            // watch does not keep one: a turn is recorded as ending within a tick
            // of when it did, which is 400ms on a span measured in minutes.
            if let Some(start) = member.turn_started_at.clone() {
                member.push_span(start, now());
            }
            member.turn_started_at = facts.turn_at.clone();
            moved = true;
        }
        let next = if facts.blocked {
            // A request the gate has not answered is the member's whole state:
            // it is not working, it is waiting on a decision.
            Some(MemberState::Waiting)
        } else if member.state == MemberState::Waiting {
            // The answer puts it back to work, whatever else is true of it. A
            // member left `waiting` by a gate that has answered is the one word
            // the roster must never keep.
            Some(MemberState::Working)
        } else if facts.turn_at.is_some() {
            // A turn in flight is work.
            Some(MemberState::Working)
        } else {
            None
        };
        // A member the engine has already decided about is not moved by a turn
        // clock. `Parked` is a decision too: `after_amendment` parks a lead that
        // has answered and has nothing more to say, and a fold that put it back
        // to `working` would make the wedge detector expect it to keep talking —
        // the failure that cost the thirteenth live run. `Failed` is the same
        // argument: the wedge detector fails a member precisely because its turn
        // is not answering, and a fold would erase the one word the run died on.
        let decided = matches!(
            member.state,
            MemberState::Parked | MemberState::Done | MemberState::Failed | MemberState::Lost
        );
        let next = next.filter(|next| !decided && member.state != *next);
        if let Some(next) = next {
            member.state = next;
            moved = true;
        }
    }
    moved
}

/// Drive a run to its end, and make sure its agents end with it.
///
/// The loop has several ways out — a terminal state, a cancel, the run being
/// replaced — and only the terminal branch released the bridges. Every other
/// exit left one `vibe-acp` process per member alive: they are children of this
/// process, a parked one keeps the runtime alive, and each holds its worktree
/// directory open. A headless run then hung after printing its result, and the
/// next run could not remove the directories the orphans still held. Releasing
/// here is one owner for "the loop is over, so the agents are over", so no exit
/// path can forget.
async fn drive(sink: Arc<dyn EventSink>, run_id: String, cancel: Arc<AtomicBool>) {
    drive_loop(sink, run_id, cancel).await;
    release_bridges().await;
}

/// What the amendment step did with the lead's answer.
#[derive(Debug, PartialEq)]
enum AmendmentStep {
    /// No amendment file was waiting.
    Nothing,
    /// The amendment was consumed without escalating the run.
    Consumed,
    /// The run escalated; the drive loop's tick ends here.
    Escalated,
}

/// Apply a waiting amendment from the lead's inbox, if any.
///
/// An amendment is applied whenever one is waiting, not only while the run
/// is `Replanning` — the lead reads the board and may correct a plan mid-run.
/// A no-op amendment is handled as benign here, not escalated: it is the
/// lead confirming the plan.
fn handle_lead_amendment(sink: Arc<dyn EventSink>, run: &Run) -> AmendmentStep {
    let replanning = run.state == RunState::Replanning;
    if !(replanning || run.state == RunState::Running) {
        return AmendmentStep::Nothing;
    }
    let path = inbox_dir(&run.id, "lead").join("amendment.json");
    let Ok(body) = std::fs::read_to_string(&path) else {
        return AmendmentStep::Nothing;
    };
    let mut next = run.clone();
    let applied = amend::Amendment::parse(&body)
        .map_err(amend::AmendError::Invalid)
        .and_then(|amendment| {
            amend::apply(&mut next, &amendment).map(|changed| (amendment, changed))
        });
    // Consumed either way: an amendment that cannot be applied must
    // not be re-read on every tick, and the lead has to be told.
    let _ = std::fs::remove_file(&path);
    match applied {
        Ok((amendment, changed)) => {
            // The lead read the question and said the narrow fix is
            // not enough. That is an answer, not a failure: the
            // question is the user's now, and the lead's reason is
            // what the user needs to read. Resuming the run with
            // nothing changed would escalate again on the next tick
            // and burn the amendment budget on a loop.
            if amendment.is_ask() {
                next.state = RunState::Escalated;
                next.failure = Some(amendment.reason.trim().to_string());
                for member in
                    std::iter::once(&mut next.lead).chain(next.members.iter_mut())
                {
                    if member.state.is_live() {
                        member.state = MemberState::Parked;
                        member.parked_at = Some(now());
                    }
                }
                next.touch();
                let _ = next.save();
                write_run(&next);
                emit(&sink, &next);
                engine().post(
                    BoardEntry::new(
                        "engine",
                        BoardKind::Note,
                        "the lead could not fix the plan — the run stopped here, worktrees kept",
                    )
                    .about("run"),
                );
                return AmendmentStep::Escalated;
            }
            if replanning {
                // The member that stopped is the one that has to
                // carry on.
                let stopped = engine()
                    .board_tail()
                    .into_iter()
                    .rev()
                    .find(|e| e.kind == BoardKind::Escalation && e.from != "engine")
                    .map(|e| e.from);
                next = after_amendment(&next, stopped.as_deref());
                let _ = next.save();
                write_run(&next);
                emit(&sink, &next);
                engine().post(
                    BoardEntry::new(
                        "engine",
                        BoardKind::Note,
                        format!("plan amended: {}", changed.join("; ")),
                    )
                    .about("run"),
                );
                if !amendment.reason.trim().is_empty() {
                    engine().post(
                        BoardEntry::new("lead", BoardKind::Decision, amendment.reason)
                            .about("run"),
                    );
                }
                if let Some(member) = stopped {
                    nudge_member(
                        &sink,
                        &next,
                        &member,
                        roles::amended_brief(&changed.join("
")),
                    );
                }
            } else {
                // Nobody stopped, so nobody has to be put back to
                // work: the tasks that were running keep running and
                // the ones that were blocked pick up the new shape
                // when the scheduler reaches them.
                next.touch();
                let _ = next.save();
                write_run(&next);
                emit(&sink, &next);
                engine().post(
                    BoardEntry::new(
                        "engine",
                        BoardKind::Note,
                        format!("plan amended: {}", changed.join("; ")),
                    )
                    .about("run"),
                );
                if !amendment.reason.trim().is_empty() {
                    engine().post(
                        BoardEntry::new("lead", BoardKind::Decision, amendment.reason)
                            .about("run"),
                    );
                }
            }
            AmendmentStep::Consumed
        }
        Err(amend::AmendError::NoOp) => {
            // A no-op amendment is the lead saying the plan is already
            // right. Say so and carry on — the run is not escalated over
            // a confirmation.
            engine().post(
                BoardEntry::new(
                    "engine",
                    BoardKind::Note,
                    "the amendment changed nothing — the plan already says that",
                )
                .about("run"),
            );
            AmendmentStep::Consumed
        }
        Err(amend::AmendError::Invalid(error)) if replanning => {
            // The lead could not answer within the contract. That is
            // the user's question now, and the reason says why.
            next.state = RunState::Escalated;
            next.failure =
                Some(format!("the lead could not amend the plan: {error}"));
            next.touch();
            let _ = next.save();
            write_run(&next);
            emit(&sink, &next);
            engine().post(
                BoardEntry::new(
                    "engine",
                    BoardKind::Escalation,
                    format!("the lead's amendment was refused: {error}"),
                )
                .about("run"),
            );
            return AmendmentStep::Escalated;
        }
        Err(amend::AmendError::Invalid(error)) => {
            // The run is fine and the amendment was not. Say so and
            // let the lead try again rather than stopping a run that
            // has nothing wrong with it.
            engine().post(
                BoardEntry::new(
                    "engine",
                    BoardKind::Note,
                    format!("the lead's amendment was refused: {error}"),
                )
                .about("run"),
            );
            let inbox = inbox_dir(&run.id, "lead");
            let tasks = run
                .tasks
                .iter()
                .map(|t| {
                    format!(
                        "{} — {} — owns {}",
                        t.id,
                        t.title,
                        if t.files.is_empty() {
                            "(no files declared)".to_string()
                        } else {
                            t.files.join(", ")
                        }
                    )
                })
                .collect::<Vec<_>>()
                .join("
");
            nudge_member(
                &sink,
                &run,
                "lead",
                roles::amendment_brief(&error, &inbox.to_string_lossy(), &tasks),
            );
            AmendmentStep::Consumed
        }
    }
}

async fn drive_loop(sink: Arc<dyn EventSink>, run_id: String, cancel: Arc<AtomicBool>) {
    let engine = engine();

    loop {
        if cancel.load(Ordering::SeqCst) {
            return;
        }

        let Some(run) = engine.run() else {
            return;
        };
        if run.id != run_id {
            return;
        }
        if run.state.is_terminal() {
            end_of_run().await;
            return;
        }

        // 0a. A stop asked for by another process.
        //
        // The adopting window cannot set this process's cancel flag, so it writes
        // a file into the run directory and this is where the driver notices. The
        // tick is the right cadence: it is the same loop that already reads the
        // board, the worktrees and every member's ledger, so a request is acted on
        // within one tick of arriving and no second watcher is needed.
        if let Some(stopped) = engine.honour_stop_request(&run.dir(), &sink) {
            engine.post(
                BoardEntry::new(
                    "engine",
                    BoardKind::Note,
                    format!(
                        "run stopped at the request of another process — {} members parked, worktrees kept",
                        std::iter::once(&stopped.lead)
                            .chain(stopped.members.iter())
                            .filter(|m| m.state == MemberState::Parked)
                            .count()
                    ),
                )
                .about("run"),
            );
            return;
        }

        // 0. Fold every member's spend and its live state onto the run.
        //
        // The gate accumulates as the agent reports, and this is the only place
        // that saves. Copying is idempotent because the ledger holds totals, so
        // a tick that changes nothing writes the same numbers back. Without this
        // the roster showed zero tokens for every member for the whole run: the
        // fields existed and nothing ever wrote them.
        //
        // The same seam carries the two facts the gate and the watch see and the
        // run has to show: whether the member is blocked on a request, and when
        // its current turn was asked for.
        {
            let (ledgers, live): (Vec<(String, Usage)>, Vec<(String, Live)>) = {
                let inner = engine.inner.lock().unwrap_or_else(|e| e.into_inner());
                let ledgers = inner
                    .usage
                    .iter()
                    .map(|(id, ledger)| {
                        let usage = ledger.lock().unwrap_or_else(|e| e.into_inner()).clone();
                        (id.clone(), usage)
                    })
                    .collect();
                let live = inner
                    .watches
                    .iter()
                    .map(|(id, watch)| {
                        (
                            id.clone(),
                            Live {
                                blocked: watch.is_blocked(),
                                turn_at: watch.turn_at(),
                            },
                        )
                    })
                    .collect();
                (ledgers, live)
            };
            let mut next = run.clone();
            let mut moved = fold_usage(&mut next, &ledgers);
            moved |= fold_live(&mut next, &live);
            if moved {
                next.recompute_spend();
                next.touch();
                let _ = next.save();
                write_run(&next);
                emit(&sink, &next);
            }
        }

        // 0b. A commit is work.
        //
        // `last_at` only moves when a member writes to the board, so a worker
        // that spends ten minutes reading and then commits looks exactly like
        // one that died. The fifth live run nudged w-t1 twice while it was
        // committing; the sixth failed a worker for silence after it had already
        // committed. The worktree is the second signal, and it is read only for
        // members already past the nudge threshold, so the cost is one
        // `git rev-parse` per suspect per tick rather than one per member.
        let run = refresh_progress(&sink, run);

        // 0c. Fold every member's skill loads onto the run.
        //
        // Read from the member's own tool calls, so this is evidence rather than
        // a claim. It runs before the inbox drain because a worker posts its
        // evidence and then stops: the load has to be on the run before the
        // verifier is spawned, or the verifier is told the worker loaded nothing.
        let run = sync_skill_loads(&sink, run);

        // 1. Drain every member's inbox into the board.
        let mut posted = Vec::new();
        for member in run.roster() {
            let entries = drain_inbox(&run, &member.id);
            for entry in entries {
                let stored = engine.post(entry);
                posted.push(stored);
            }
        }
        if !posted.is_empty() {
            emit(&sink, &run);
        }

        // 1a. A file in an inbox that is not an entry is a member that misread
        // the brief, and silence about it costs the run. Said once per file, so
        // a member that keeps writing the wrong name does not fill the board.
        for member in run.roster() {
            for name in ignored_inbox_files(&run, &member.id) {
                let receipts = receipts_dir(&run.id, &member.id);
                let marker = receipts.join(format!("{name}.ignored.json"));
                if marker.exists() {
                    continue;
                }
                let _ = std::fs::create_dir_all(&receipts);
                write_receipt(
                    &receipts,
                    &format!("{name}.ignored"),
                    false,
                    format!("`{name}` is not an entry — the inbox takes one `.json` file per entry"),
                    None,
                );
                engine.post(
                    BoardEntry::new(
                        "engine",
                        BoardKind::Note,
                        format!(
                            "{} wrote `{name}` into its inbox, which is not an entry — the inbox takes one `.json` file per entry. It was ignored.",
                            member.id
                        ),
                    )
                    .about("run"),
                );
            }
        }

        // 1b. A member asking for a decision stops the run.
        //
        // `BoardKind::Escalation` is documented as "a member asking for a human
        // decision" and used to be drained onto the board and then ignored. The
        // fourth live run's w-t1 escalated because the plan gave the arena fix to
        // a task that does not own `src/player.js`; the run carried on for four
        // more minutes and died with "no task can make progress" — a reason that
        // named nothing. The member was right. The engine was the one not
        // listening, and a channel that looks like it stops the run and does not
        // is worse than no channel at all.
        if run.state == RunState::Running {
            if let Some(entry) = escalation_stopping(&posted) {
                if escalate(&sink, &run, entry, Question::PlanNeedsChanging).is_none() {
                    return;
                }
            }
        }

        // 1c. The lead answered. Apply the amendment and put the worker back to
        // work.
        //
        // An amendment is applied whenever one is waiting, not only while the run
        // is `Replanning`. The lead is the architect and it reads the board: the
        // sixth live run's lead wrote a correct amendment — grant
        // `test/integration.test.js` to t3 and widen its criterion — while the run
        // was `Running`, and the engine ignored it because the state was not
        // `Replanning`. The file sat in the inbox for the rest of the run and the
        // plan it corrected was never corrected.
        if handle_lead_amendment(sink.clone(), &run) == AmendmentStep::Escalated {
            return;
        }

        // 2. React to what arrived.
        let mut changed = false;
        let board = engine.board_tail();

        // A plan from the lead parks the run for approval.
        if run.state == RunState::Planning {
            if let Some(plan) = read_plan(&run) {
                let mut plan = plan;
                // Fill the tasks the lead left bare before validating, so the
                // names the engine chose are checked by the same gate as the
                // names the lead chose. A filled name comes from the catalogue
                // and always passes; the point is that there is one gate.
                // The skills the plan writes are part of the catalogue the gate
                // and the ranking see, so a task the lead left bare can be
                // assigned one and a task that names one is not refused as
                // unknown. Nothing is written until the plan passes.
                let mut catalogue = Catalogue {
                    skills: run.skills.clone(),
                };
                catalogue.skills.extend(
                    plan.skills_to_write
                        .iter()
                        .filter(|draft| plan::draft_problem(draft).is_none())
                        .map(|draft| draft_skill(&run.id, draft)),
                );
                let filled = settle_task_skills(&catalogue, &mut plan);
                let mut violations = plan::validate_with(&plan, Some(&catalogue))
                    .err()
                    .unwrap_or_default();
                // The one plan check that needs the repository: a criterion that
                // already passes on the base commit cannot prove anything, and the
                // engine can see that now instead of at the third verdict.
                if violations.is_empty() {
                    violations = criteria_passing_on_base(
                        &run.id,
                        &PathBuf::from(&run.workspace),
                        &run.integration.base,
                        &plan,
                    );
                }
                let mut next = run.clone();
                // A one-wide first rank is the one shape warning worth sending
                // the plan back for — once. See `shape_send_back`.
                let one_wide = shape_send_back(run.plan_attempts, &plan);
                if violations.is_empty() && one_wide.is_none() {
                    next = accept_plan(&run, &plan);
                    // The plan passed, so the skills it authored can be written.
                    // They join the run's catalogue, which is what every brief
                    // and every later assignment reads.
                    let (written, failed) = write_plan_skills(&run.id, &plan);
                    if !written.is_empty() {
                        next.skills = catalogue.skills.clone();
                        engine.post(
                            BoardEntry::new(
                                "engine",
                                BoardKind::Note,
                                format!(
                                    "the plan wrote {} for this run: {}",
                                    if written.len() == 1 { "a skill" } else { "skills" },
                                    written
                                        .iter()
                                        .map(|skill| skill.name.as_str())
                                        .collect::<Vec<_>>()
                                        .join(", ")
                                ),
                            )
                            .about("run"),
                        );
                    }
                    if !failed.is_empty() {
                        engine.post(
                            BoardEntry::new(
                                "engine",
                                BoardKind::Note,
                                format!("could not write the plan's skills: {}", failed.join("; ")),
                            )
                            .about("run"),
                        );
                    }
                    engine.post(
                        BoardEntry::new(
                            "lead",
                            BoardKind::Note,
                            format!("plan ready: {} tasks", plan.tasks.len()),
                        )
                        .about("run"),
                    );
                    // Which assignments were the engine's rather than the lead's.
                    // A run whose skills were all filled by the ranking is a run
                    // where the lead ignored the catalogue, and the user reading
                    // the board should be able to tell.
                    if !filled.is_empty() {
                        engine.post(
                            BoardEntry::new(
                                "engine",
                                BoardKind::Note,
                                format!(
                                    "skills ranked for the tasks the lead left bare: {}",
                                    filled.join("; ")
                                ),
                            )
                            .about("run"),
                        );
                    }
                    // The shape the plan was accepted with. On the board as well
                    // as in the panel, because the board is the record: a warning
                    // that only ever existed in a view is a warning nobody
                    // reading the transcript afterwards can see.
                    for warning in &next.shape_warnings {
                        engine.post(
                            BoardEntry::new("engine", BoardKind::Note, warning.message())
                                .about("run"),
                        );
                    }
                } else {
                    let mut reasons: Vec<String> =
                        violations.iter().map(Violation::message).collect();
                    if let Some(reason) = &one_wide {
                        reasons.push(reason.clone());
                    }
                    for reason in &reasons {
                        engine.post(
                            BoardEntry::new("engine", BoardKind::Escalation, reason.clone())
                                .about("run"),
                        );
                    }
                    if next.plan_attempts < PLAN_ATTEMPTS {
                        // Send it back. The lead is a live session, so a revision
                        // is one more turn — not a new process, and not a new
                        // plan from scratch.
                        next.plan_attempts += 1;
                        next.lead.state = MemberState::Working;
                        next.lead.last_at = Some(now());
                        engine.post(
                            BoardEntry::new(
                                "engine",
                                BoardKind::Note,
                                format!(
                                    "plan refused — sent back to the lead (revision {} of {PLAN_ATTEMPTS})",
                                    next.plan_attempts
                                ),
                            )
                            .about("run"),
                        );
                        let inbox = inbox_dir(&next.id, "lead");
                        nudge_member(
                            &sink,
                            &next,
                            "lead",
                            roles::revision_brief(&reasons.join("\n  - "), &inbox.to_string_lossy()),
                        );
                    } else {
                        next.state = RunState::Failed;
                        next.failure = Some(reasons.join("; "));
                    }
                }
                next.touch();
                let _ = next.save();
                write_run(&next);
                emit(&sink, &next);
                changed = true;
            } else if member_is_silent("lead", &run.lead) && run.lead.nudges < NUDGE_ATTEMPTS {
                // No plan, and the lead is not thinking. Ask again rather than
                // waiting out the idle limit on a turn that already ended.
                let idle = silence_secs(&run.lead).unwrap_or(LEAD_NUDGE_SECS);
                let mut next = run.clone();
                next.plan_attempts += 1;
                let why = charge_nudge(&mut next, "lead");
                next.touch();
                let _ = next.save();
                write_run(&next);
                emit(&sink, &next);
                engine.post(match &why {
                    Some(why) => BoardEntry::new(
                        "engine",
                        BoardKind::Conflict,
                        format!(
                            "the lead has written no plan for {idle}s and cannot be asked again — {why}"
                        ),
                    )
                    .about("run"),
                    None => BoardEntry::new(
                        "engine",
                        BoardKind::Note,
                        format!(
                            "no plan for {idle}s — asked the lead again (nudge {} of {NUDGE_ATTEMPTS}, plan revision {} of {PLAN_ATTEMPTS})",
                            next.lead.nudges, next.plan_attempts
                        ),
                    )
                    .about("run"),
                });
                if why.is_none() {
                    let inbox = inbox_dir(&next.id, "lead");
                    nudge_member(
                        &sink,
                        &next,
                        "lead",
                        roles::reminder_brief(&inbox.to_string_lossy()),
                    );
                }
                changed = true;
            }
        }

        // Evidence and verdicts move tasks.
        if run.state == RunState::Running {
            let mut next = run.clone();

            // A worker that has posted evidence is done implementing. Hand the
            // task to a verifier. The guard is the task state, not a flag: once
            // it is `Verifying` the same entry cannot re-fire on the next tick.
            for entry in &board {
                if entry.kind != BoardKind::Evidence {
                    continue;
                }
                let Some(task_id) = entry.task.as_deref() else {
                    continue;
                };
                let Some(task) = next.task(task_id) else {
                    continue;
                };
                if task.state != TaskState::Running {
                    continue;
                }
                // Evidence from a previous attempt does not count. Without this
                // the engine re-reads the refused attempt's evidence the instant
                // a retry starts and hands the task straight back to a verifier:
                // the second live run's t2 was re-verified one second after its
                // retry began, with no work done, and failed on the same evidence.
                if !entry_is_current(entry, task) {
                    continue;
                }
                // What the worker actually committed, against what the plan
                // granted it. Read from the branch rather than asked of the
                // worker, because the worker is the one that broke the rule.
                // `changed_paths` skips merge commits, so the dependency
                // branches `prepare_worker` merged in are not counted as the
                // worker's work.
                let (committed, out_of_grant) = {
                    let path = worktree::worktree_path(&next.id, &format!("w-{task_id}"));
                    let base = task
                        .base_commit
                        .clone()
                        .unwrap_or_else(|| next.integration.base.clone());
                    match worktree::changed_paths(&CommandGit, &path, &base) {
                        Ok(committed) => {
                            let outside = worktree::outside_grant(&committed, &task.files);
                            (committed, outside)
                        }
                        // An unreadable branch is reported, not swallowed. An
                        // empty list here is not "nothing outside the grant", it
                        // is "no observation" — and the deliverable check below
                        // treats it as the absence it is.
                        Err(why) => {
                            engine.post(
                                BoardEntry::new(
                                    "engine",
                                    BoardKind::Conflict,
                                    format!("w-{task_id}'s branch could not be read: {why}"),
                                )
                                .about(task_id),
                            );
                            (Vec::new(), Vec::new())
                        }
                    }
                };
                if !out_of_grant.is_empty() {
                    engine.post(
                        BoardEntry::new(
                            "engine",
                            BoardKind::Escalation,
                            format!(
                                "w-{task_id} committed {} outside its grant: {} — kept on the branch, the verifier rules on it",
                                if out_of_grant.len() == 1 {
                                    "a file"
                                } else {
                                    "files"
                                },
                                out_of_grant.join(", ")
                            ),
                        )
                        .about(task_id),
                    );
                }
                // A branch that contains none of the granted paths has not
                // delivered this task, whatever else it contains, and the engine
                // knows that without asking anyone. Handing it to a verifier is
                // what produced the audited run's worst defect: v-t2 was given a
                // branch whose three granted files did not exist, wrote all three
                // herself, committed them and repointed the worker's branch at
                // her own commit so her tests would pass — then returned a
                // failing verdict on a branch that now held her work.
                //
                // Nothing is deleted and no branch moves: the worker is told
                // which paths the plan granted it, and the ceiling is the one a
                // refused verdict answers to, because this is the same kind of
                // event.
                if worktree::delivered_nothing(&task.files, &committed) {
                    let untouched = worktree::untouched(&task.files, &committed);
                    let worker = format!("w-{task_id}");
                    end_member_turn(
                        &worker,
                        next.member(&worker).and_then(|m| m.session_id.clone()),
                    );
                    let reason = format!(
                        "none of the {} files the plan granted this task were touched — missing: {}; committed instead: {}",
                        task.files.len(),
                        untouched.join(", "),
                        if committed.is_empty() {
                            "nothing".to_string()
                        } else {
                            committed.join(", ")
                        }
                    );
                    engine.post(
                        BoardEntry::new(
                            "engine",
                            BoardKind::Escalation,
                            format!(
                                "w-{task_id} delivered none of its granted files, so the handoff was refused — {reason}"
                            ),
                        )
                        .about(task_id),
                    );
                    let refused = Verdict {
                        passed: false,
                        reason,
                        evidence: committed.clone(),
                        command: None,
                        base_exit: None,
                        change_exit: None,
                        failing_first: false,
                        base_dirtied: false,
                        at: now(),
                    };
                    let ceiling = next.max_attempts;
                    if let Some(task) = next.task_mut(task_id) {
                        task.out_of_grant = out_of_grant;
                        let _ = schedule::apply_verdict(task, refused, ceiling);
                    }
                    next.touch();
                    let _ = next.save();
                    write_run(&next);
                    emit(&sink, &next);
                    continue;
                }
                // Pin the tree the verdict will be about. The verifier reads the
                // branch, and a branch is a pointer anything with a shell can
                // move: the audited run's v-t2 repointed its worker's branch at
                // its own commit after returning a failing verdict. Capturing the
                // tip here is what makes the comparison at verdict time mean
                // something.
                let judged = {
                    let path = worktree::worktree_path(&next.id, &format!("w-{task_id}"));
                    worktree::head(&CommandGit, &path).ok()
                };
                // What the branch actually holds, recorded where the engine is
                // already reading it. `state: working` said nothing about
                // progress in the audited run: five of eight worktrees held two
                // files and one commit after thirty minutes, and the only way to
                // find out was to open them by hand.
                let (commits, last_commit) = {
                    let path = worktree::worktree_path(&next.id, &format!("w-{task_id}"));
                    let base = task
                        .base_commit
                        .clone()
                        .unwrap_or_else(|| next.integration.base.clone());
                    (
                        worktree::commit_count(&CommandGit, &path, &base).unwrap_or(0),
                        worktree::last_commit_at(&CommandGit, &path, &base).ok(),
                    )
                };
                if let Some(task) = next.task_mut(task_id) {
                    task.state = TaskState::Verifying;
                    task.verifier = Some(format!("v-{task_id}"));
                    task.out_of_grant = out_of_grant;
                    task.verified_commit = judged;
                    task.files_changed = committed.len();
                    task.commits = commits;
                    task.last_commit_at = last_commit;
                }
                // The worker's job is over. Its brief says to stop after posting
                // evidence, but a model with momentum keeps going: the first live
                // run had w-t1 re-implement its task and post a second evidence
                // entry while a verifier was already reading the branch. The
                // engine knows the work is in, so it ends the turn itself.
                let worker = format!("w-{task_id}");
                end_member_turn(&worker, next.member(&worker).and_then(|m| m.session_id.clone()));
                engine.post(
                    BoardEntry::new("engine", BoardKind::Note, "handed to a verifier")
                        .about(task_id),
                );
                // A verifier from a previous attempt is not this attempt's
                // verifier. Its session already answered, and a session that is
                // asked again re-emits the answer it gave — the twelfth live run
                // posted a second verdict byte-identical to the first, still
                // saying "the change never touched src/main.js" about a branch
                // whose tip was a commit that did exactly that. Dropping the
                // bridge kills the process; the next spawn gets a fresh session
                // that reads the branch as it is now.
                let verifier = format!("v-{task_id}");
                {
                    let mut inner = engine.inner.lock().unwrap_or_else(|e| e.into_inner());
                    // The bridge goes; the ledger stays. The new session's
                    // counters start at zero, and `Usage::observe` reads a
                    // reading that went down as a new session, so the spend from
                    // the first attempt is kept rather than erased.
                    inner.take_bridge(&verifier);
                }
                if let Some(member) = next.member_mut(&verifier) {
                    member.state = MemberState::Starting;
                    member.session_id = None;
                    member.pid = None;
                    member.nudges = 0;
                    member.last_at = Some(now());
                }
                // The spawn proves the criterion before it writes the brief, so
                // the verifier is handed the engine's result rather than asked
                // to reproduce it. That proof needs the worktrees the spawn
                // creates, which is why it lives there and not here.
                spawn_for_task(sink.clone(), next.id.clone(), task_id.to_string(), true, None);
                changed = true;
            }

            for entry in &board {
                if entry.kind != BoardKind::Verdict {
                    continue;
                }
                let Some(task_id) = entry.task.as_deref() else {
                    continue;
                };
                let Some(task) = next.task(task_id) else {
                    continue;
                };
                if task.state != TaskState::Verifying {
                    continue;
                }
                // A verdict from a previous attempt does not count. The evidence
                // handler has this guard; the verdict handler did not, and the
                // eleventh live run died of it: w-t1 posted evidence at 20:44:28,
                // the task went `Verifying`, and one second later this loop
                // re-read the *first* attempt's verdict, applied it a second time,
                // found `attempts == max_attempts` and failed the task — killing a
                // run whose worker had just delivered.
                if !entry_is_current(entry, task) {
                    continue;
                }
                // The boolean the verifier committed to, and only failing that
                // the first word of its text. A verdict with neither is a
                // failure rather than a pass: a malformed verdict must never be
                // the reason a task is accepted.
                let passed = match verdict_ruling(entry) {
                    Some(passed) => passed,
                    None => {
                        engine.post(
                            BoardEntry::new(
                                "engine",
                                BoardKind::Note,
                                format!(
                                    "the verdict did not start with a ruling, so it was not counted: {}",
                                    entry.text.chars().take(120).collect::<String>()
                                ),
                            )
                            .about(task_id),
                        );
                        false
                    }
                };

                // The engine runs the command itself, on both sides. A verdict
                // that says "failing first" is a claim; two exit codes are an
                // observation, and only the observation counts.
                //
                // The base side is the task's own base commit — the run's base
                // with its dependencies merged in — because that is the tree the
                // criterion was written for. Against the run's base a dependent
                // task's test fails only because its dependency is missing, which
                // is a proof of nothing.
                // Read the baseline's dirtiness *before* the reset below. The
                // reset exists so a verifier's leftover edit cannot make a real
                // failure look like a pass — and because it ran first, the check
                // that followed it always found a clean tree. `base_dirtied` is
                // the clause that says "this verdict measured a baseline the
                // verifier changed"; `Verdict::is_earned` refuses on it, and it
                // had no way to become true except in the seconds between the
                // reset and the proof.
                let (base_checkout, base_dirtied_before) = base_checkout_for(&next, task);
                let change = worktree::worktree_path(&next.id, &format!("w-{task_id}"));
                // The criterion is the command. A verifier that names no
                // `command` is not exempt from the failing-first check: the
                // task's own acceptance is the command the plan wrote, and
                // proving it is what the engine does anyway. Without this the
                // sixth live run's t1 was refused for a criterion that passes on
                // the base — correctly — and the engine could not see it,
                // because the verifier had left `command` out and the proof was
                // recorded as unattempted.
                let named = entry.command.as_deref().map(str::to_string);
                let own = task.acceptance.trim().to_string();
                let mut command = named.clone().unwrap_or_else(|| own.clone());
                let mut proof = if base_checkout.is_dir() && change.is_dir() {
                    prove::prove(&base_checkout, &change, &command, prove::PROOF_TIMEOUT)
                } else {
                    prove::Proof::unattempted(
                        "the base checkout or the change is missing, so the command could not be run on both",
                    )
                };
                // A verifier cannot weaken the criterion by naming a different
                // command.
                //
                // The plan's acceptance is the authority; the verifier's `command`
                // is a convenience for saying which one it ran. A verifier that
                // names a command which passes on the base commit has proved
                // nothing, and accepting that as "the criterion is unprovable"
                // would blame the plan for the verifier's mistake — the lead would
                // rewrite a criterion that was already right, and the amendment
                // would be refused for changing nothing.
                //
                // So when the named command passes on the base and the task's own
                // criterion is a different command, the engine proves the task's
                // own criterion instead and says so on the board.
                if proof.base_exit == Some(0)
                    && named.as_deref() != Some(own.as_str())
                    && !own.is_empty()
                    && plan::prose_marker(&own).is_none()
                    && base_checkout.is_dir()
                    && change.is_dir()
                {
                    let second = prove::prove(&base_checkout, &change, &own, prove::PROOF_TIMEOUT);
                    if second.base_exit != Some(0) {
                        engine.post(
                            BoardEntry::new(
                                "engine",
                                BoardKind::Note,
                                format!(
                                    "the verdict proved `{command}`, which passes on the base commit too, so it proves nothing. The engine proved the task's own criterion `{own}` instead — the plan's acceptance is the authority, not the command the verifier chose."
                                ),
                            )
                            .about(task_id),
                        );
                        command = own;
                        proof = second;
                    }
                }
                if named.is_none() {
                    engine.post(
                        BoardEntry::new(
                            "engine",
                            BoardKind::Note,
                            format!(
                                "the verdict named no command, so the engine proved the task's own criterion: `{command}`"
                            ),
                        )
                        .about(task_id),
                    );
                }

                // A verifier that wrote to the base checkout has changed the
                // thing it was measuring. Detected, not prevented: git cannot
                // make a checkout read-only. Two observations, because there are
                // two windows — what it left behind on an earlier turn (read
                // before the reset above) and what it wrote during this one.
                let base_dirtied =
                    base_dirtied_before || worktree::is_dirty(&CommandGit, &base_checkout).unwrap_or(false);

                if let Some(note) = &proof.note {
                    engine.post(
                        BoardEntry::new("engine", BoardKind::Note, note.clone()).about(task_id),
                    );
                }
                if base_dirtied {
                    engine.post(
                        BoardEntry::new(
                            "engine",
                            BoardKind::Conflict,
                            "the base checkout is dirty — this verdict measured a baseline the verifier changed",
                        )
                        .about(task_id),
                    );
                }

                // A criterion that passes on the base commit cannot tell the
                // change from the starting point, so no worker can ever satisfy
                // it. That is the plan's writing, not the worker's, and the lead
                // is who rewrites it — so this is an escalation, not a retry.
                // The fifteenth live run spent three worker attempts and three
                // verifier turns on exactly this before dying.
                if proof.base_exit == Some(0) {
                    let escalation = BoardEntry::new(
                        format!("v-{task_id}"),
                        BoardKind::Escalation,
                        format!(
                            "{task_id}'s criterion `{}` passes on the base commit too, so it cannot tell the change from the base. The worker cannot fix that — the criterion is the plan's. Rewrite it to exercise the behaviour the change introduces.",
                            entry.command.as_deref().unwrap_or("(none)")
                        ),
                    )
                    .about(task_id);
                    engine.post(escalation.clone());
                    // The engine posts this itself, so it never passes through
                    // the inbox drain that `escalation_stopping` reads. Hand it
                    // to the same transition directly: a channel that looks like
                    // it stops the run and does not is worse than no channel at
                    // all, and this one exists because three worker attempts and
                    // three verifier turns were spent on a criterion no worker
                    // could satisfy.
                    match escalate(&sink, &next, &escalation, Question::CriterionUnprovable) {
                        Some(stopped) => {
                            next = stopped;
                            changed = true;
                            break;
                        }
                        None => return,
                    }
                }

                // A verdict that asks for a decision is not a retry.
                //
                // The verifier can find a change that is correct and still cannot
                // be finished by the task that made it: the caller it has to
                // update lives in a file the task does not own. Retrying spends
                // the attempt on something the worker cannot do, and the sixth
                // live run's v-t1 said so in as many words — "the worker cannot
                // fix this ... escalating rather than failing" — but wrote the
                // escalation into its verdict instead of a second file, so the
                // engine retried anyway. The field is on the entry now.
                if let Some(why) = entry.escalate.as_deref().filter(|w| !w.trim().is_empty()) {
                    let escalation = BoardEntry::new(
                        format!("v-{task_id}"),
                        BoardKind::Escalation,
                        format!("{task_id}: {why}"),
                    )
                    .about(task_id);
                    engine.post(escalation.clone());
                    match escalate(&sink, &next, &escalation, Question::PlanNeedsChanging) {
                        Some(stopped) => {
                            next = stopped;
                            changed = true;
                            break;
                        }
                        None => return,
                    }
                }

                // A verdict is about one tree. If the branch moved while the
                // verifier was reading it, the verdict describes a tree that
                // cannot be integrated, and merging the new tip would integrate
                // work nobody judged.
                let moved = {
                    let pinned = next.task(task_id).and_then(|t| t.verified_commit.clone());
                    let branch = next.task(task_id).and_then(|t| t.branch.clone());
                    match (pinned, branch) {
                        (Some(pinned), Some(branch)) => {
                            let repo = PathBuf::from(&next.workspace);
                            match worktree::rev_parse(&CommandGit, &repo, &branch) {
                                Ok(tip) if tip != pinned => Some(format!(
                                    "the branch moved while the verdict was being earned: pinned {pinned}, now {tip}"
                                )),
                                _ => None,
                            }
                        }
                        _ => None,
                    }
                };
                let verdict = Verdict {
                    passed: passed && moved.is_none(),
                    reason: match &moved {
                        Some(why) => format!("{} ({why})", entry.text),
                        None => entry.text.clone(),
                    },
                    evidence: entry.evidence.clone(),
                    command: entry.command.clone(),
                    base_exit: proof.base_exit,
                    change_exit: proof.change_exit,
                    failing_first: proof.failing_first,
                    base_dirtied,
                    at: entry.at.clone(),
                };
                let ceiling = next.max_attempts;
                let mut refused = false;
                if let Some(task) = next.task_mut(task_id) {
                    let transition = schedule::apply_verdict(task, verdict, ceiling);
                    // A refused verdict is not only this task's problem. Any task
                    // that was waiting on it — started on its commit, or unblocked
                    // and waiting for a slot — built on work that has just been
                    // refused, and its branch has to go. The flag is read before
                    // the borrow ends because the rollback needs the whole run.
                    refused = matches!(transition, Transition::Fail { .. });
                    if let Transition::Complete { .. } = transition {
                        engine.post(
                            BoardEntry::new("engine", BoardKind::Note, "task verified")
                                .about(task_id),
                        );
                        // The worker has nothing left to do. Parking it is what
                        // makes an idle member cheap: a parked member is not
                        // burning a turn, and the idle watchdog must not read its
                        // silence as a wedge. The first live run failed a whole
                        // run because w-t1 sat silent for 601s after its task had
                        // already been verified.
                        let worker = format!("w-{task_id}");
                        if let Some(member) = next.member_mut(&worker) {
                            member.state = MemberState::Parked;
                            member.parked_at = Some(now());
                        }
                    }
                    // The verifier has said its piece either way: its bridge is
                    // dropped and stopped, and the member parked.
                    retire_verifier(&mut next, task_id).await;
                }
                if refused {
                    // Every task that was waiting on this one built on work that
                    // has just been refused. Its branch is discarded and it goes
                    // back to `Blocked`; it re-runs when this task is verified
                    // again. `attempts` is not touched — a dependency's failure is
                    // not the dependent's attempt.
                    invalidate_built_on(&mut next, task_id).await;
                }
                changed = true;
            }

            // A verifier whose turn ended without a verdict leaves the task in
            // `Verifying` with nothing coming. The same recovery as the lead's:
            // ask again, twice, then let the idle limit be the honest answer.
            let silent: Vec<String> = next
                .tasks
                .iter()
                .filter(|task| task.state == TaskState::Verifying)
                .filter_map(|task| {
                    let id = format!("v-{}", task.id);
                    let member = next.member(&id)?;
                    if member.nudges >= NUDGE_ATTEMPTS {
                        return None;
                    }
                    member_is_silent(&id, member).then_some(task.id.clone())
                })
                .collect();
            for task_id in silent {
                let verifier = format!("v-{task_id}");
                let Some(task) = next.task(&task_id).cloned() else {
                    continue;
                };
                let idle = next
                    .member(&verifier)
                    .and_then(silence_secs)
                    .unwrap_or(LEAD_NUDGE_SECS);
                let why = charge_nudge(&mut next, &verifier);
                engine.post(match &why {
                    Some(why) => BoardEntry::new(
                        "engine",
                        BoardKind::Conflict,
                        format!(
                            "{verifier} has ruled on nothing for {idle}s and cannot be asked again — {why}"
                        ),
                    )
                    .about(task_id.clone()),
                    None => BoardEntry::new(
                        "engine",
                        BoardKind::Note,
                        format!("no verdict for {idle}s — asked {verifier} again"),
                    )
                    .about(task_id.clone()),
                });
                if why.is_some() {
                    changed = true;
                    continue;
                }
                let inbox = inbox_dir(&next.id, &verifier);
                nudge_member(
                    &sink,
                    &next,
                    &verifier,
                    roles::verifier_reminder_brief(&task, &inbox.to_string_lossy()),
                );
                changed = true;
            }

            // A worker whose turn ended without posting evidence is the one role
            // with no recovery. The lead has a nudge, the verifier has a nudge,
            // and a worker had only the ten-minute idle limit — which fails the
            // run. The eighth live run's w-t1 finished its turn, posted nothing,
            // and the run died ten minutes later with "a member stopped
            // responding" while the work sat committed on its branch.
            let stalled: Vec<String> = next
                .tasks
                .iter()
                .filter(|task| task.state == TaskState::Running)
                .filter_map(|task| {
                    let id = format!("w-{}", task.id);
                    let member = next.member(&id)?;
                    if member.nudges >= NUDGE_ATTEMPTS {
                        return None;
                    }
                    member_is_silent(&id, member).then_some(task.id.clone())
                })
                .collect();
            for task_id in stalled {
                let worker = format!("w-{task_id}");
                let Some(task) = next.task(&task_id).cloned() else {
                    continue;
                };
                // Measured, and named. The board used to print the threshold it
                // tripped — "no evidence after 180s" — as though it were the
                // reading, so a worker silent for a thousand seconds and one
                // silent for two hundred looked identical on the board.
                let idle = next
                    .member(&worker)
                    .and_then(silence_secs)
                    .unwrap_or(LEAD_NUDGE_SECS);
                let why = charge_nudge(&mut next, &worker);
                engine.post(match &why {
                    Some(why) => BoardEntry::new(
                        "engine",
                        BoardKind::Conflict,
                        format!(
                            "{worker} has posted no evidence for {idle}s and cannot be asked again — {why}"
                        ),
                    )
                    .about(task_id.clone()),
                    None => BoardEntry::new(
                        "engine",
                        BoardKind::Note,
                        format!(
                            "no evidence for {idle}s — asked {worker} again (evidence clock {LEAD_NUDGE_SECS}s, wedged limit {MEMBER_IDLE_LIMIT_SECS}s)"
                        ),
                    )
                    .about(task_id.clone()),
                });
                if why.is_some() {
                    changed = true;
                    continue;
                }
                let inbox = inbox_dir(&next.id, &worker);
                nudge_member(
                    &sink,
                    &next,
                    &worker,
                    roles::worker_reminder_brief(&task, &inbox.to_string_lossy()),
                );
                changed = true;
            }

            // Fill the slots.
            let concurrency = schedule::useful_concurrency(&next.tasks, next.concurrency);
            let transitions = schedule::advance(&mut next.tasks, concurrency);
            // A dispatch is a change. Without this the run was never saved, so the
            // next tick saw the same task still `Blocked`, dispatched it again and
            // posted another "started" claim — 700 of them in three minutes, and a
            // task that never actually advanced.
            if !transitions.is_empty() {
                changed = true;
            }
            for transition in &transitions {
                if let Transition::Start { task } = transition {
                    let member_id = format!("w-{task}");
                    if let Some(member) = next.member_mut(&member_id) {
                        member.task = Some(task.clone());
                        member.state = MemberState::Starting;
                    }
                    // The task records its owner so the panel can name the worker
                    // without joining against the roster, and so the worker's own
                    // brief can say who it is.
                    if let Some(record) = next.task_mut(&task) {
                        record.owner = Some(member_id.clone());
                    }
                    engine.post(
                        BoardEntry::new(member_id.as_str(), BoardKind::Claim, "started")
                            .about(task.clone()),
                    );
                    // A retry reuses the member. Its session still holds the
                    // worktree and the context that produced the work, so the
                    // cheapest correct thing is to tell it what the verifier
                    // said. Spawning would return early anyway — the bridge is
                    // still in the map — and the task would sit with no worker.
                    let already = {
                        let inner = engine.inner.lock().unwrap_or_else(|e| e.into_inner());
                        inner.bridges.contains_key(&member_id)
                    };
                    if already {
                        let reason = next
                            .task(&task)
                            .and_then(|t| t.verdict.as_ref())
                            .map(|v| v.reason.clone())
                            .unwrap_or_else(|| "the verifier refused the work".into());
                        // Drop the old bridge before spawning. Its session was
                        // cancelled when the worker posted its evidence, and a
                        // cancelled session does not take another turn — it
                        // accepts the prompt, posts a claim, and then goes silent
                        // until the idle limit fails the run. Dropping the bridge
                        // kills the process; the worktree and the commits stay.
                        {
                            let mut inner =
                                engine.inner.lock().unwrap_or_else(|e| e.into_inner());
                            inner.take_bridge(&member_id);
                        }
                        if let Some(member) = next.member_mut(&member_id) {
                            member.state = MemberState::Starting;
                            member.session_id = None;
                            member.pid = None;
                            // A fresh process is a fresh budget. The nudge budget
                            // is about "this process is not talking", not "this
                            // member has ever been nudged": the sixth live run's
                            // w-t1 spent both nudges on attempt 1, and attempt 3
                            // then sat silent with no nudge left and wedged the
                            // run at 601s.
                            member.nudges = 0;
                            member.last_at = Some(now());
                        }
                        spawn_for_task(
                            sink.clone(),
                            next.id.clone(),
                            task.clone(),
                            false,
                            Some(reason),
                        );
                        changed = true;
                        continue;
                    }
                    // Prepared here, not in the spawned member: the base commit
                    // has to be on the run this loop is about to save.
                    match prepare_worker(&next, &task) {
                        Ok(prep) => {
                            let WorkerPrep {
                                base,
                                branch,
                                resolved,
                                built_on,
                            } = prep;
                            // A conflict the plan resolved is still a decision
                            // the user has to be able to see: one branch's work
                            // was dropped in favour of the owner's.
                            let mut reconcile: Vec<String> = Vec::new();
                            for one in &resolved {
                                if one.owner == task.as_str() {
                                    reconcile.push(one.file.clone());
                                }
                                engine.post(
                                    BoardEntry::new(
                                        "engine",
                                        BoardKind::Note,
                                        format!(
                                            "{} and its dependencies disagreed about {} — kept {}'s version, because the plan gives it that file",
                                            task, one.file, one.owner
                                        ),
                                    )
                                    .about(task.clone()),
                                );
                            }
                            if let Some(record) = next.task_mut(&task) {
                                record.base_commit = Some(base);
                                // The branch the worktree is actually on. A
                                // re-run after a rollback creates it again from
                                // the deterministic name, and without this the
                                // record stays `None` and integration refuses
                                // the task as "verified but has no branch".
                                record.branch = Some(branch);
                                record.reconcile = reconcile;
                                // What this task was built on, so a failed
                                // verdict upstream can find it again.
                                record.built_on = built_on;
                            }
                            spawn_for_task(sink.clone(), next.id.clone(), task.clone(), false, None);
                        }
                        Err(error) => {
                            // A dependency that will not merge is a plan
                            // question, not a dead task. Two branches that both
                            // verified still disagree about a file, and only the
                            // lead can say which one owns it — add an ordering
                            // edge, narrow a grant, or re-scope the task. The
                            // twelfth live run died here: t1 and t2 both
                            // verified, t3 could not merge t2's branch over t1's
                            // on `src/player.js`, and the run failed with "no
                            // task can make progress" while the lead sat parked
                            // with amendment budget unspent.
                            //
                            // The half-built worktree goes first.
                            // `prepare_worker` short-circuits on a directory
                            // that exists, so leaving it would hand the retry a
                            // tree with none of its dependencies merged in — the
                            // same failure one attempt later, with a worse
                            // reason.
                            let workspace = PathBuf::from(&next.workspace);
                            let path = worktree::worktree_path(&next.id, &member_id);
                            let branch = next
                                .task(&task)
                                .and_then(|t| t.branch.clone())
                                .unwrap_or_else(|| worktree::branch_name(&next.id, &task));
                            worktree::discard_worktree(&CommandGit, &workspace, &path, &branch);

                            let repairs =
                                next.task(&task).map(|t| t.conflict_repairs).unwrap_or(0);
                            let spent = repairs >= MAX_CONFLICT_REPAIRS;
                            if let Some(member) = next.member_mut(&member_id) {
                                member.state = if spent {
                                    MemberState::Failed
                                } else {
                                    MemberState::Parked
                                };
                                member.error = Some(error.clone());
                            }
                            if let Some(record) = next.task_mut(&task) {
                                record.conflict = Some(error.clone());
                                record.conflict_repairs += 1;
                                record.state = if spent {
                                    TaskState::Failed
                                } else {
                                    TaskState::Conflict
                                };
                            }
                            let text = format!("{task} could not start: {error}");
                            engine.post(
                                BoardEntry::new("engine", BoardKind::Escalation, text.clone())
                                    .about(task.clone()),
                            );
                            if spent {
                                // The lead has had its chances. Fail the task
                                // and let `is_stuck` end the run with a reason
                                // that names the conflict rather than the
                                // scheduler.
                                if let Some(record) = next.task_mut(&task) {
                                    record.state = TaskState::Failed;
                                }
                                changed = true;
                                continue;
                            }
                            // Save the conflict before the escalation writes its
                            // own copy of the run, or the reason the lead is
                            // being asked is lost from the run on disk.
                            next.touch();
                            let _ = next.save();
                            write_run(&next);
                            emit(&sink, &next);
                            match escalate(
                                &sink,
                                &next,
                                &BoardEntry::new("engine", BoardKind::Escalation, text)
                                    .about(task.clone()),
                                Question::PlanNeedsChanging,
                            ) {
                                Some(escalated) => {
                                    next = escalated;
                                    changed = false;
                                }
                                // The lead is out of amendment budget, so the
                                // question is the user's and the loop ends.
                                None => return,
                            }
                            break;
                        }
                    }
                }
            }

            if next.all_verified() {
                next.state = RunState::Integrating;
                changed = true;
            }
            // A run waiting on the lead is not stuck, it is waiting. The lead was
            // asked because a task could not start, and that task is `Conflict`
            // until the amendment lands — which is exactly the shape `is_stuck`
            // reads as "nothing can move". Checking the state as well as the
            // tasks is what keeps the escalation from being undone by the
            // scheduler one line later.
            if next.state == RunState::Running && schedule::is_stuck(&next.tasks) {
                next.state = RunState::Failed;
                next.failure = Some("no task can make progress".into());
                changed = true;
            }

            if changed {
                next.touch();
                let _ = next.save();
                write_run(&next);
                emit(&sink, &next);
            }
        }

        // Integration.
        if run.state == RunState::Integrating {
            let mut next = run.clone();
            let workspace = PathBuf::from(&next.workspace);
            match integrate::integrate(&CommandGit, &workspace, &mut next) {
                IntegrationOutcome::Merged { order } => {
                    engine.post(
                        BoardEntry::new(
                            "engine",
                            BoardKind::Note,
                            format!("merged {} branches in order", order.len()),
                        )
                        .about("run"),
                    );
                    // Every task green and the combination red is the failure
                    // mode a swarm is most likely to produce, and it was never
                    // checked: the plan declared a suite, the panel showed it,
                    // and nothing ran it, so a run could report `done` over a
                    // tree whose own tests were failing. The suite is the only
                    // thing that proves the merged result works, so it runs
                    // here and a red one fails the run.
                    let suite = next.integration.suite.clone().unwrap_or_default();
                    if suite.trim().is_empty() {
                        next.state = RunState::Reflecting;
                    } else {
                        match prove::run_capture(&workspace, &suite, SUITE_TIMEOUT) {
                            Ok((code, output)) => {
                                let passed = code == 0;
                                integrate::record_suite(&mut next, &suite, passed);
                                if passed {
                                    engine.post(
                                        BoardEntry::new(
                                            "engine",
                                            BoardKind::Note,
                                            format!("the integrated suite passed: `{suite}`"),
                                        )
                                        .about("run"),
                                    );
                                    next.state = RunState::Reflecting;
                                } else {
                                    let blamed = integrate::attribute_failure(&next, &output);
                                    let who = match blamed.as_deref() {
                                        Some(task) => format!(" — most likely {task}"),
                                        None => String::new(),
                                    };
                                    engine.post(
                                        BoardEntry::new(
                                            "engine",
                                            BoardKind::Verdict,
                                            format!(
                                                "the integrated suite failed{who}. `{suite}` exited {code}. Tail: {}",
                                                output.chars().take(600).collect::<String>()
                                            ),
                                        )
                                        .about(blamed.clone().unwrap_or_else(|| "run".into())),
                                    );

                                    // A red suite is not the end of the run. Every
                                    // task passed and the combination did not,
                                    // which is the failure a swarm is most likely
                                    // to produce and the one a human would fix by
                                    // hand in the merged tree. So the run does
                                    // that: a repair task starts from the merged
                                    // tree, owns the suite as its criterion, and
                                    // merges back. The ceiling is what stops a
                                    // suite that cannot pass from eating the
                                    // budget.
                                    if next.integration.repairs < MAX_REPAIR_ROUNDS {
                                        let task =
                                            repair_task(&next, &suite, &output, blamed.as_deref());
                                        let id = task.id.clone();
                                        let worker_model = next
                                            .members
                                            .iter()
                                            .find(|m| m.role == Role::Worker)
                                            .map(|m| m.model.clone())
                                            .unwrap_or_else(|| next.lead.model.clone());
                                        let verifier_model = next
                                            .members
                                            .iter()
                                            .find(|m| m.role == Role::Verifier)
                                            .map(|m| m.model.clone())
                                            .unwrap_or_else(|| worker_model.clone());
                                        next.members.push(Member::new(
                                            format!("w-{id}"),
                                            Role::Worker,
                                            worker_model,
                                        ));
                                        next.members.push(Member::new(
                                            format!("v-{id}"),
                                            Role::Verifier,
                                            verifier_model,
                                        ));
                                        next.tasks.push(task);
                                        next.integration.repairs += 1;
                                        next.integration.state = IntegrationState::Pending;
                                        next.state = RunState::Running;
                                        engine.post(
                                            BoardEntry::new(
                                                "engine",
                                                BoardKind::Note,
                                                format!(
                                                    "round {} of {MAX_REPAIR_ROUNDS}: {id} starts from the merged tree and owns the suite as its criterion",
                                                    next.integration.repairs
                                                ),
                                            )
                                            .about(id),
                                        );
                                    } else {
                                        next.state = RunState::Failed;
                                        next.failure = Some(format!(
                                            "the integrated suite failed{who} after {} repair round(s): `{suite}` exited {code}",
                                            next.integration.repairs
                                        ));
                                    }
                                }
                            }
                            Err(error) => {
                                next.state = RunState::Failed;
                                next.failure = Some(format!("the integrated suite could not run: {error}"));
                            }
                        }
                    }
                }
                IntegrationOutcome::Conflict { task, files, .. } => {
                    next.state = RunState::Failed;
                    next.failure = Some(format!(
                        "merging {task} conflicted on {}",
                        files.join(", ")
                    ));
                    engine.post(
                        BoardEntry::new(
                            "engine",
                            BoardKind::Conflict,
                            format!("conflict on {}", files.join(", ")),
                        )
                        .about(task),
                    );
                }
                IntegrationOutcome::Error(error) => {
                    next.state = RunState::Failed;
                    next.failure = Some(error);
                }
            }
            next.touch();
            let _ = next.save();
            write_run(&next);
            emit(&sink, &next);
        }

        // Reflection, then done.
        //
        // The pass is a member, not a post-processing step: it reads the board
        // and the repository in a session of its own and writes one file. Three
        // ticks at most — put it on the roster, start it, read what it wrote —
        // and every way it can fail ends in a named degraded reflection rather
        // than a run that hangs on its own bookkeeping.
        if run.state == RunState::Reflecting {
            let mut next = run.clone();
            let board = Board::load_at(&next.dir());

            match read_reflection(&inbox_dir(&next.id, REFLECTOR_ID)) {
                ReflectionRead::Draft(mut draft) => {
                    // Taken before `accept` consumes the draft, because the two
                    // halves are accepted by different rules: a gotcha must cite
                    // the board, a skill must be a document somebody could follow.
                    let draft_skills = std::mem::take(&mut draft.skills);
                    let (reflection, dropped) = reflect::accept(draft, &board);
                    // The skill proposals, written into the run's own directory.
                    // Nothing reaches the user's skill store from here: the panel
                    // offers them, and accepting one is a click.
                    let (mut proposals, dropped_skills) = reflect::accept_skills(
                        draft_skills,
                        &next.dir().join("skills"),
                    );
                    // A skill the plan wrote and a member actually opened is a
                    // method this run proved it needed. It is offered for
                    // permanent installation alongside the reflection's own
                    // proposals, because the alternative is that the next run
                    // writes the same skill again from scratch.
                    for skill in next.skills.iter().filter(|skill| skill.scope == skills::RUN_SCOPE) {
                        let used = std::iter::once(&next.lead)
                            .chain(next.members.iter())
                            .any(|member| member.skills_loaded.iter().any(|name| name == &skill.name));
                        if !used || proposals.iter().any(|p| p.name == skill.name) {
                            continue;
                        }
                        proposals.push(skills::Proposal {
                            name: skill.name.clone(),
                            description: skill.description.clone(),
                            body: std::fs::read_to_string(&skill.path)
                                .map(|text| strip_frontmatter(&text))
                                .unwrap_or_default(),
                            because: format!(
                                "written by this run's plan and loaded by a member on it —                                  keeping it means the next run does not write it again"
                            ),
                            path: skill.path.clone(),
                        });
                    }
                    if !dropped_skills.is_empty() {
                        let _ = std::fs::write(
                            inbox_dir(&next.id, REFLECTOR_ID).join("skills.dropped.json"),
                            serde_json::to_string_pretty(&dropped_skills).unwrap_or_default(),
                        );
                    }
                    if !proposals.is_empty() {
                        engine.post(
                            BoardEntry::new(
                                "engine",
                                BoardKind::Note,
                                format!(
                                    "{} skill{} proposed for this machine: {} — review them in the panel",
                                    proposals.len(),
                                    if proposals.len() == 1 { "" } else { "s" },
                                    proposals
                                        .iter()
                                        .map(|p| p.name.as_str())
                                        .collect::<Vec<_>>()
                                        .join(", ")
                                ),
                            )
                            .about("run"),
                        );
                    }
                    next.skill_proposals = proposals;
                    if !dropped.is_empty() {
                        // Kept beside the inbox, for the same reason a malformed
                        // draft is: "six gotchas were discarded" is not a
                        // diagnosis. The file is what tells a bad brief from a
                        // bad model, and the run is over by the time anyone
                        // reads it, so nothing else will.
                        let _ = std::fs::write(
                            inbox_dir(&next.id, REFLECTOR_ID).join("gotchas.dropped.json"),
                            serde_json::to_string_pretty(&dropped).unwrap_or_default(),
                        );
                        engine.post(
                            BoardEntry::new(
                                "engine",
                                BoardKind::Note,
                                format!(
                                    "{} gotcha{} discarded ({}) — see gotchas.dropped.json",
                                    dropped.len(),
                                    if dropped.len() == 1 { "" } else { "s" },
                                    reflect::drop_summary(&dropped)
                                ),
                            )
                            .about("run"),
                        );
                    }
                    next.reflection = Some(reflection);
                    next.state = RunState::Done;
                    if let Some(member) = next.member_mut(REFLECTOR_ID) {
                        member.state = MemberState::Done;
                    }
                    next.touch();
                    let _ = next.save();
                    write_run(&next);
                    emit(&sink, &next);
                }
                ReflectionRead::Malformed(error) => {
                    next.reflection = Some(reflect::degraded(
                        format!("the reflection pass wrote something unreadable: {error}"),
                        &board,
                    ));
                    next.state = RunState::Done;
                    if let Some(member) = next.member_mut(REFLECTOR_ID) {
                        member.state = MemberState::Failed;
                        member.error = Some(error);
                    }
                    next.touch();
                    let _ = next.save();
                    write_run(&next);
                    emit(&sink, &next);
                }
                ReflectionRead::Missing => {
                    let reflector = next.member(REFLECTOR_ID).cloned();
                    match reflector {
                        // First tick in this state: put the pass on the roster
                        // and start it. The next tick reads what it wrote.
                        None => {
                            // The workers' model, which is the cheapest tier the
                            // run already chose. Reflection is summarisation over
                            // a digest, so it does not need the lead's tier.
                            let model = next
                                .members
                                .iter()
                                .find(|m| m.role == Role::Worker)
                                .map(|m| m.model.clone())
                                .unwrap_or_else(|| next.lead.model.clone());
                            let mut member = Member::new(REFLECTOR_ID, Role::Reflector, model);
                            // Stamped now, not when the process starts: a spawn
                            // that never lands must still time out, or the run
                            // waits on a member that does not exist.
                            member.last_at = Some(now());
                            next.members.push(member);
                            next.touch();
                            let _ = next.save();
                            write_run(&next);
                            emit(&sink, &next);
                            spawn_reflector(sink.clone(), next.id.clone());
                        }
                        // Still working, or wedged. A pass that failed or ran
                        // past its limit degrades the reflection rather than
                        // holding a finished run open.
                        Some(member) => {
                            let reason = if member.state == MemberState::Failed {
                                Some(
                                    member
                                        .error
                                        .clone()
                                        .unwrap_or_else(|| "the reflection pass failed".into()),
                                )
                            } else if member.state.is_turning() {
                                // `is_turning`, not `is_live`: the question here
                                // is whether the reflection pass is still
                                // running, and a reflector whose turn is over has
                                // stopped running whether or not its process is
                                // still there.
                                member
                                    .last_at
                                    .as_deref()
                                    .and_then(|at| {
                                        chrono::DateTime::parse_from_rfc3339(at).ok().map(|at| {
                                            (chrono::Utc::now()
                                                - at.with_timezone(&chrono::Utc))
                                            .num_seconds()
                                        })
                                    })
                                    .filter(|idle| *idle > REFLECTION_LIMIT_SECS)
                                    .map(|idle| {
                                        format!(
                                            "the reflection pass was still running after {idle}s"
                                        )
                                    })
                            } else {
                                None
                            };

                            if let Some(reason) = reason {
                                next.reflection = Some(reflect::degraded(reason, &board));
                                next.state = RunState::Done;
                                if let Some(member) = next.member_mut(REFLECTOR_ID) {
                                    member.state = MemberState::Failed;
                                }
                                next.touch();
                                let _ = next.save();
                                write_run(&next);
                                emit(&sink, &next);
                            }
                        }
                    }
                }
            }
        }

        // Budget.
        {
            let mut next = run.clone();
            next.recompute_spend();
            if next.budget.exceeded() || next.budget.out_of_time() {
                next.state = RunState::Failed;
                next.failure = Some(if next.budget.exceeded() {
                    // Name the ceiling and what was spent against it, not the
                    // traffic. "budget exceeded: 2318483 tokens" told the user
                    // nothing about whether the run was expensive or merely
                    // long: 2.09M of those were cache reads, which cost a
                    // fraction of fresh input. The ceiling counts the billable
                    // tokens, so the message reports the same number the check
                    // used and keeps the traffic figure beside it.
                    let spent = next.budget.spent_tokens;
                    let used = next.budget.used_tokens;
                    let cached = next.budget.cached_tokens;
                    // The money figure rides along when the models are priced: it
                    // is the number that says whether the run was expensive, and a
                    // token count cannot.
                    let money = match next.budget.spend_micros {
                        Some(micros) => format!(
                            ", ${:.4} at rates v{}",
                            micros as f64 / 1_000_000.0,
                            next.budget.rates_version
                        ),
                        None => ", spend unpriced".to_string(),
                    };
                    format!(
                        "budget exceeded: {spent} tokens of a {} ceiling ({used} crossed the wire, {cached} of them cache reads{money})",
                        next.budget.max_tokens
                    )
                } else {
                    "wall-clock budget exceeded".into()
                });
                next.touch();
                let _ = next.save();
                write_run(&next);
                emit(&sink, &next);
                return;
            }
        }

        // A member whose process is gone is restarted, not failed.
        //
        // This runs before `wedge` on purpose: a restart resets `last_at`, so the
        // wedge check that follows sees a member that has just started rather
        // than one that has been silent for ten minutes. The ceiling is what
        // keeps a member that dies on spawn from looping.
        if let Some((next, restarts)) = respawn_dead(&sink, &run, &turn_in_flight) {
            for restart in restarts {
                engine.post(
                    BoardEntry::new(
                        "engine",
                        BoardKind::Note,
                        format!(
                            "{} stopped responding — restarted it on {}",
                            restart.member, restart.task
                        ),
                    )
                    .about(&restart.task),
                );
                spawn_for_task(
                    sink.clone(),
                    next.id.clone(),
                    restart.task,
                    restart.verify,
                    restart.reason,
                );
            }
            tokio::time::sleep(TICK).await;
            continue;
        }

        // A member that has been silent past the limit is wedged, not working.
        //
        // Silence only means a wedge when the member still has something to do.
        // A worker whose task is verified, or a verifier whose verdict is in, is
        // idle by design — and a run that fails because a finished member stopped
        // talking is a run that punishes the swarm for working.
        if let Some(next) = wedge(&run, &turn_in_flight, &pulse_of) {
            let _ = next.save();
            write_run(&next);
            emit(&sink, &next);
            return;
        }

        tokio::time::sleep(TICK).await;
    }
}

/// The task that repairs an integrated suite which failed.
///
/// Every task green and the combination red is the failure a swarm is most
/// likely to produce, and it was terminal: the suite ran, failed, and the run
/// died with the work merged and nothing left to do about it. The repair is not
/// a new mechanism — it is a task. It depends on every verified task, so
/// `prepare_worker` merges them into its base and it starts from exactly the
/// tree the suite failed on; its criterion is the suite itself, so the verifier
/// proves the whole rather than a part; and it merges back into the integration
/// branch like any other task, after which the suite runs again.
///
/// Pure, so the shape is testable without a repository.
fn repair_task(run: &Run, suite: &str, output: &str, blamed: Option<&str>) -> Task {
    let round = run.integration.repairs + 1;
    let id = format!("fix-{round}");

    // The blamed task's files are the best guess at where the seam is. Empty is
    // honest when nothing could be attributed: the ownership map then constrains
    // nothing, which is right for a task that may have to touch anything.
    let files: Vec<String> = blamed
        .and_then(|task| run.task(task))
        .map(|task| task.files.clone())
        .unwrap_or_default();

    let deps: Vec<String> = run
        .tasks
        .iter()
        .filter(|task| task.state.is_verified())
        .map(|task| task.id.clone())
        .collect();

    let who = match blamed {
        Some(task) => format!(" The failure was attributed to {task}."),
        None => String::new(),
    };
    let tail: String = output.chars().take(1200).collect();

    let detail = format!(
        "Every task passed its own criterion and the integrated result does not.          The suite `{suite}` was run on the merged tree and exited non-zero.{who}

         This tree is the merged work of every task, so the bug is in how the parts          fit together, not in any one of them. Fix it here. Do not weaken the suite          and do not delete a test to make it pass: the suite is the criterion this          task is judged by, and a repair that edits the criterion has repaired          nothing.

         The suite said:

{tail}"
    );

    let mut task = Task::new(
        id,
        format!("repair the integrated suite (round {round})"),
        detail,
        suite,
    );
    task.files = files;
    task.deps = deps;
    task.state = TaskState::Ready;
    task
}

/// The run after the lead's amendment was applied.
///
/// Two state changes, and both were wrong in the thirteenth live run. The lead
/// was left `Working`, so the wedge detector expected it to keep talking: it had
/// answered the question and had nothing more to say, and 601s later the run
/// failed with "a member stopped responding: lead" while both of its tasks were
/// mid-verification. And the worker that had escalated was put back to work
/// *after* the run was saved, so the change lived only in that tick's copy and
/// the next tick read the worker as still parked.
///
/// Pure, so both are testable without a live agent.
fn after_amendment(run: &Run, stopped: Option<&str>) -> Run {
    let mut next = run.clone();
    next.state = RunState::Running;
    // A coordinator that has finished coordinating is parked, not silent.
    if let Some(lead) = next.member_mut("lead") {
        lead.state = MemberState::Parked;
        lead.parked_at = Some(now());
    }
    if let Some(member_id) = stopped {
        // A verifier that escalated is not going back to work: its finding was
        // about the criterion, so the *worker* has to run again under the new
        // one. Returning the verifier to `Working` would leave a member burning
        // with nothing to do, which is the wedge the amendment path exists to
        // avoid.
        let is_verifier = next
            .member(member_id)
            .map(|m| m.role == Role::Verifier)
            .unwrap_or(false);
        if let Some(record) = next.member_mut(member_id) {
            if is_verifier {
                record.state = MemberState::Parked;
                record.parked_at = Some(now());
            } else {
                record.state = MemberState::Working;
                record.last_at = Some(now());
            }
        }
        if is_verifier {
            let task_id = next.member(member_id).and_then(|m| m.task.clone());
            if let Some(task_id) = task_id {
                if let Some(task) = next.task_mut(&task_id) {
                    if task.state != TaskState::Verified {
                        task.state = TaskState::Ready;
                    }
                }
            }
        }
    }
    // A task that could not start because its dependencies would not merge is
    // waiting on the plan, and the plan just changed. Put it back in the queue:
    // the amendment may have added the ordering edge or narrowed the grant that
    // made the merge impossible, and retrying is the only way to find out. The
    // per-task repair ceiling is what stops a lead that keeps re-planning the
    // same collision from looping the run.
    for task in next.tasks.iter_mut() {
        if task.state == TaskState::Conflict {
            task.state = TaskState::Blocked;
            task.conflict = None;
        }
    }
    next.touch();
    next
}

/* ------------------------------------------------------------------ *
 * Skills
 * ------------------------------------------------------------------ */

/// The catalogue this run plans against, the goal's shortlist, and the section
/// the lead reads.
///
/// Loaded once, at start, and stored on the run. The catalogue is the authority
/// the plan's `skills` names are validated against, so the lead is never refused
/// for naming something it was shown — which is what would happen if the panel
/// validated against a catalogue re-read after the user installed a skill.
fn plan_skills(goal: &str, workspace: &Path) -> (Catalogue, Vec<skills::Match>, String) {
    // The switch is read here rather than at the call site so there is one place
    // that decides whether a run is handed the method. Off means an empty
    // catalogue, which every downstream step already handles: no shortlist, no
    // assignment, no section in any brief.
    if !crate::settings::load().swarm_skills {
        return (Catalogue::default(), Vec::new(), String::new());
    }
    let catalogue = Catalogue::load(Some(workspace));
    if catalogue.is_empty() {
        return (catalogue, Vec::new(), String::new());
    }
    let ranked = skills::rank(&catalogue, goal, SKILL_SHORTLIST);
    let section = skills::render_for_lead(&catalogue, &ranked, SKILL_CATALOGUE_IN_BRIEF);
    (catalogue, ranked, section)
}

/// A `SKILL.md` without its frontmatter, for a proposal that reuses a file.
///
/// The proposal renders its own frontmatter from the name and description, so
/// keeping the original would produce two blocks and a body that starts with
/// `---`. A file with no frontmatter is returned whole.
fn strip_frontmatter(text: &str) -> String {
    let trimmed = text.trim_start();
    if !trimmed.starts_with("---") {
        return trimmed.to_string();
    }
    match trimmed[3..].find("
---") {
        Some(end) => trimmed[3 + end + 4..].trim_start().to_string(),
        None => trimmed.to_string(),
    }
}

/// Where a skill the plan writes lives: inside the run's own directory.
///
/// Not `~/.vibe/skills`. A run that could write there could change how every
/// future run behaves, and that is not a side effect to take on trust. The run's
/// directory is deleted with the run, so a skill written here is scoped to the
/// work that needed it, and keeping one is a click.
fn run_skill_path(run_id: &str, name: &str) -> PathBuf {
    super::run_dir(run_id).join("skills").join(name).join("SKILL.md")
}

/// A draft as a catalogue entry, without writing anything.
///
/// The gate and the ranking need to see the skill the plan writes before the
/// file exists, so that a task the lead left bare can be assigned it and a task
/// that names it is not refused as unknown. The path is computed the same way
/// the write computes it, so the brief a worker reads points at the file that
/// will be there by the time the worker starts.
fn draft_skill(run_id: &str, draft: &reflect::SkillDraft) -> Skill {
    Skill {
        name: draft.name.clone(),
        description: if draft.description.trim().is_empty() {
            draft
                .body
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty() && !line.starts_with('#'))
                .unwrap_or("")
                .to_string()
        } else {
            draft.description.trim().to_string()
        },
        path: run_skill_path(run_id, &draft.name).to_string_lossy().to_string(),
        scope: skills::RUN_SCOPE.to_string(),
        bundle: None,
        enabled: true,
        // The draft's own body, so a skill the plan wrote ranks on the same
        // evidence as one that was already installed.
        body: draft.body.clone(),
    }
}

/// Write the skills the plan authored, and return them as catalogue entries.
///
/// Called only after the plan has passed the gate, so a draft that would be
/// refused leaves no file behind. A write that fails is reported on the board
/// rather than failing the run: the tasks can still be attempted without the
/// method, and a run that stops because a directory could not be made is a run
/// that wasted the lead turn that wrote the plan.
fn write_plan_skills(run_id: &str, plan: &Plan) -> (Vec<Skill>, Vec<String>) {
    let mut written = Vec::new();
    let mut failed = Vec::new();
    for draft in &plan.skills_to_write {
        let skill = draft_skill(run_id, draft);
        let path = PathBuf::from(&skill.path);
        let body = format!(
            "---\nname: {}\ndescription: {}\n---\n\n{}\n",
            skill.name,
            skill.description.replace('\n', " "),
            draft.body.trim()
        );
        let result = path
            .parent()
            .map(std::fs::create_dir_all)
            .unwrap_or_else(|| Ok(()))
            .and_then(|_| std::fs::write(&path, body));
        match result {
            Ok(()) => written.push(skill),
            Err(error) => failed.push(format!("{}: {error}", skill.name)),
        }
    }
    (written, failed)
}

/// Give every task the skills it should have, and report what was filled.
///
/// A task the lead left bare is ranked on its own text — title, detail and the
/// files it owns — rather than on the run's goal, because two tasks in one run
/// are usually about different things. A task the lead named keeps exactly what
/// it named: the lead knows what the task is, and the ranking reads its title.
///
/// Returns the ids that were filled, so the board can say which assignments were
/// the engine's rather than the lead's.
fn settle_task_skills(catalogue: &Catalogue, plan: &mut Plan) -> Vec<String> {
    if catalogue.is_empty() {
        return Vec::new();
    }
    let mut filled = Vec::new();
    for task in plan.tasks.iter_mut() {
        if !task.skills.is_empty() {
            // Deduplicated in place: a lead that names the same skill twice gets
            // one line in the brief, not two.
            let mut seen = std::collections::BTreeSet::new();
            task.skills.retain(|name| seen.insert(name.clone()));
            continue;
        }
        let text = skills::task_text(&task.title, &task.detail, &task.files);
        let assigned = skills::assign(catalogue, &text, SKILLS_PER_TASK);
        if !assigned.is_empty() {
            filled.push(format!("{} → {}", task.id, assigned.join(", ")));
            task.skills = assigned;
        }
    }
    filled
}

/// The skills a task was given, resolved against the run's catalogue.
///
/// A name the catalogue does not hold is dropped here rather than rendered: the
/// plan gate refuses unknown names, so reaching this with one means the run was
/// loaded from a file written by an older build, and a brief that tells a worker
/// to load a skill that is not installed wastes its first turn.
fn task_skills(run: &Run, task: &Task) -> Vec<Skill> {
    task.skills
        .iter()
        .filter_map(|name| {
            run.skills
                .iter()
                .find(|skill| &skill.name == name && skill.enabled)
                .cloned()
        })
        .collect()
}

/// The worker's skill section.
fn worker_skills(run: &Run, task: &Task) -> String {
    let picked = task_skills(run, task);
    let refs: Vec<&Skill> = picked.iter().collect();
    skills::render_for_worker(&refs)
}

/// The verifier's skill section: the same skills, plus what the worker loaded.
fn verifier_skills(run: &Run, task: &Task) -> String {
    let picked = task_skills(run, task);
    let refs: Vec<&Skill> = picked.iter().collect();
    let loaded = task
        .owner
        .as_deref()
        .and_then(|owner| run.member(owner))
        .map(|member| member.skills_loaded.clone())
        .unwrap_or_default();
    skills::render_for_verifier(&refs, &loaded)
}

/// Read the lead's plan, if it has written one.
fn read_plan(run: &Run) -> Option<Plan> {
    let path = inbox_dir(&run.id, "lead").join("plan.json");
    let body = std::fs::read_to_string(&path).ok()?;
    let plan: Plan = serde_json::from_str(&body).ok()?;
    // Move it aside so a rejected plan does not re-fire every tick. The suffix is
    // deliberately not `.json`: the drain loop reads every `.json` in this folder,
    // and a consumed plan must not come back as a board entry.
    //
    // The destination is removed first because `rename` on Windows refuses to
    // replace an existing file — and a revised plan is the second plan in the
    // same run. If the move fails anyway, the source is removed: a plan that
    // cannot be set aside must not be re-read on every tick.
    let done = inbox_dir(&run.id, "lead").join("plan.done");
    let _ = std::fs::remove_file(&done);
    if std::fs::rename(&path, &done).is_err() {
        let _ = std::fs::remove_file(&path);
    }
    Some(plan)
}

/// The tree a task's criterion is proven against on the base side, and whether it
/// was already dirty when this call found it.
///
/// The dirtiness has to be read *before* the reset, not after: `ensure_detached`
/// throws away whatever the verifier left in the checkout, and that leftover is
/// the evidence. Checking afterwards found a clean tree every time, so
/// `Verdict::is_earned`'s `base_dirtied` clause could only fire if the verifier
/// wrote during the proof itself.
fn base_checkout_for(run: &Run, task: &Task) -> (PathBuf, bool) {
    match task.base_commit.clone() {
        Some(commit) => {
            let path = worktree::task_base_path(&run.id, &task.id);
            let workspace = PathBuf::from(&run.workspace);
            let dirtied = worktree::is_dirty(&CommandGit, &path).unwrap_or(false);
            // Creates it, or resets it if the verifier left something in it: a
            // base carrying the verifier's own edit is a base that can make a
            // real failure look like a pass.
            let _ = worktree::ensure_detached(&CommandGit, &workspace, &path, &commit);
            (path, dirtied)
        }
        // A run adopted from disk before per-task bases existed. Its checkout is
        // not reset here, so the caller's later read is the one that counts.
        None => (worktree::base_checkout_path(&run.id), false),
    }
}

/// The criteria that already pass on the commit the run starts from.
///
/// `plan::validate` is pure — it reads the plan and nothing else — so the one plan
/// check that needs the repository lives here instead. It is the same fact the
/// verdict proof establishes, moved to the only point where acting on it is cheap:
/// before a worker is spawned, on the plan's own budget, rather than after three
/// attempts and two amendments.
///
/// The thirteenth live run is why. Its t2 criterion was
/// `node --test test/gates.test.js`, which passed on the base commit, so no worker
/// could ever make it distinguish the change from the base. The engine saw it at
/// the verdict — correctly — but by then the lead had spent both amendments and
/// the question fell to the user, and the run stopped with three tasks unfinished.
/// One run of the criterion against the base tree answers the same question in a
/// second, before anything is spawned.
///
/// A criterion the engine cannot run — a timeout, a missing shell — is not a
/// violation. The engine refuses a plan it has *observed* to be unprovable, not
/// one it failed to observe. A criterion that is prose or empty is skipped here
/// because `plan::validate` has already refused it for that, and naming the same
/// task twice for two different reasons reads as two problems.
fn criteria_passing_on_base(
    run_id: &str,
    workspace: &Path,
    base: &str,
    plan: &Plan,
) -> Vec<Violation> {
    if base.trim().is_empty() {
        return Vec::new();
    }
    let checkout = worktree::base_checkout_path(run_id);
    // `ensure_detached` resets a checkout that is already there, which is what
    // this needs: the verifier is allowed to enter this tree, so it may have left
    // something in it, and a base carrying an edit is a base that can make a real
    // failure look like a pass.
    if worktree::ensure_detached(&CommandGit, workspace, &checkout, base).is_err() {
        // No base tree, so no observation. The verdict proof will report the same
        // gap when it gets there, and refusing a plan over a missing worktree
        // would blame the lead for the engine's own failure.
        return Vec::new();
    }
    let mut violations = Vec::new();
    for task in &plan.tasks {
        let criterion = task.acceptance.trim();
        if criterion.is_empty() || plan::prose_marker(criterion).is_some() {
            continue;
        }
        if let Ok(0) = prove::run_in(&checkout, criterion, PLAN_PROOF_TIMEOUT) {
            violations.push(Violation::CriterionPassesOnBase {
                task: task.id.clone(),
                command: criterion.to_string(),
            });
        }
    }
    violations
}

/// The one shape warning worth a revision rather than a note, if the plan has it.
///
/// A one-wide first rank means the run begins single-threaded however many
/// workers it is given, and that is the lead's to fix — but only once. A plan
/// that genuinely starts with one foundation task is legitimate, so a retry is
/// accepted whatever its shape: `plan_attempts > 0` means the lead has already
/// been sent back once.
///
/// A plan whose widest rank is one task is not sent back at all: the chain *is*
/// the plan, so there is no wider shape to revise towards. The live rehearsal
/// made the cost visible — a two-task chain, one slot of real parallelism
/// available and none possible, was refused, and the lead spent a whole revision
/// turn proving the engine's warning did not apply to it. The warning is still
/// recorded by `shape_warnings` and shown to the user; only the revision is
/// withheld, because a revision can only return the same plan or invent
/// parallelism the dependency graph forbids.
fn shape_send_back(plan_attempts: u32, plan: &Plan) -> Option<String> {
    if plan_attempts > 0 {
        return None;
    }
    if plan::shape(plan).max_parallel <= 1 {
        return None;
    }
    plan::shape_warnings(plan)
        .into_iter()
        .find_map(|warning| match warning {
            plan::ShapeWarning::OneWideFirstRank { .. } => Some(warning.message()),
            _ => None,
        })
}

/// Extracted from the drive loop so the transition is testable, because it had a
/// bug that only a live run could find: the integration suite was not carried
/// onto the run, so the panel's Approve button — which rebuilds the plan from the
/// run — sent `integrationSuite: null` and the gate refused the same plan a
/// second time. The suite is the lead's own acceptance criterion; dropping it
/// between the two gates made approval impossible.
fn accept_plan(run: &Run, plan: &Plan) -> Run {
    let mut next = run.clone();
    next.tasks = plan.tasks.clone();
    next.integration.suite = plan.integration_suite.clone();
    // The shape is a warning, not a refusal, so it is recorded here rather than
    // returned by the gate: the plan is accepted with it and the user decides.
    next.shape_warnings = plan::shape_warnings(plan);
    next.state = RunState::AwaitingApproval;
    next.lead.state = MemberState::Parked;
    next.lead.parked_at = Some(now());
    next.touch();
    next
}

/// The run as it stands when the user approves.
///
/// Extracted for the same reason as `accept_plan`: this transition dropped the
/// integration suite. `Integration::new` builds a fresh record for the branch, so
/// assigning it over the old one threw away the lead's acceptance criterion for
/// the integrated result — and a run with no suite has no final gate at all.
///
/// This is also where the wall clock is derived from the plan. It used to be a
/// flat hour that never looked at the plan at all, and the observed run was a
/// five-deep chain whose own projection was longer than that: the engine would
/// have failed it mid-task and discarded every worktree. The budget is now the
/// plan's projection with a retry allowance, floored at the hour it always was.
///
/// `measured` is the median seconds one task took in this workspace, from
/// [`history::History::median_task_secs`]. It is a parameter rather than a read
/// so this stays a pure function of the run and the plan: the caller is the one
/// that knows which workspace it is costing, and a test can cost a plan without
/// a directory behind it.
fn start_running(
    run: &Run,
    plan: &Plan,
    base: &str,
    concurrency: usize,
    history: Option<&history::History>,
) -> Run {
    let mut next = run.clone();
    next.tasks = plan.tasks.clone();
    for task in &mut next.tasks {
        task.state = TaskState::Blocked;
        task.branch = Some(worktree::branch_name(&next.id, &task.id));
    }
    let suite = plan
        .integration_suite
        .clone()
        .or_else(|| run.integration.suite.clone());
    next.integration = super::Integration::new(format!("swarm/{}", next.id), base.to_string());
    next.integration.suite = suite;
    next.concurrency = concurrency.max(1);
    // The rehearsal at the concurrency the run will actually use, so the budget
    // describes this run rather than the plan's best case. A run given two
    // workers takes more steps than one given six, and its clock has to say so.
    let rehearsal = schedule::rehearse(&next.tasks, next.concurrency, history);
    next.budget.max_wall_secs = rehearsal.budget_wall_secs;
    next.shape_warnings = plan::shape_warnings(plan);
    next.state = RunState::Running;
    next.touch();
    next
}

/// End a member's turn because the engine already has what it needed.
///
/// Fire and forget: the cancel is a request to the agent, and the drive loop must
/// not block on a member that may take a second to notice.
fn end_member_turn(member_id: &str, session_id: Option<String>) {
    // The turn is over whether or not there is a process left to tell.
    stop_turn_clock(member_id);
    {
        let engine = engine();
        let mut inner = engine.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(run) = inner.run.as_mut() {
            if let Some(member) = run.member_mut(member_id) {
                if let Some(state) = state_after_turn(member) {
                    member.state = state;
                }
            }
        }
    }
    let Some(session_id) = session_id else {
        return;
    };
    let bridge = {
        let engine = engine();
        let inner = engine.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.bridges.get(member_id).cloned()
    };
    let Some(bridge) = bridge else {
        return;
    };
    tokio::spawn(async move {
        let _ = bridge.cancel(&session_id).await;
    });
}

/// The state a member is in when the engine ends its turn.
///
/// `Idle` is the turn being over: the process is alive, nothing is being spent,
/// and the member is not expected to say anything until it is asked. That is true
/// of a member with a task as much as one without — a worker whose evidence has
/// landed has finished its part, and a roster that goes on saying `working` for
/// the minutes a verifier takes is describing a turn that has ended.
///
/// `Parked` is the narrower state and the engine's to give: suspended
/// deliberately, holding a session it is not spending on. So a member the
/// engine has already decided about keeps its word, because the decision is the
/// more specific fact. `after_amendment` parks a lead that has answered and has
/// nothing more to say, and a cancel that put it back to `idle` would make the
/// roster disagree with the engine about the same member.
fn state_after_turn(member: &Member) -> Option<MemberState> {
    let decided = matches!(
        member.state,
        MemberState::Parked | MemberState::Done | MemberState::Failed | MemberState::Lost
    );
    (!decided).then_some(MemberState::Idle)
}

/// Retire a task's verifier once its verdict is in.
///
/// A verifier is per-task and has nothing to do after its verdict. Ending its
/// turn is not enough: a live session asked again re-emits the answer it gave,
/// and run 3 posted three byte-identical verdicts from one verifier minutes
/// apart. So the bridge is dropped and the process stopped, the same way a
/// worker handoff does, and the member is parked — suspended deliberately,
/// not silent.
async fn retire_verifier(next: &mut Run, task_id: &str) {
    let verifier = format!("v-{task_id}");
    end_member_turn(
        &verifier,
        next.member(&verifier).and_then(|m| m.session_id.clone()),
    );
    let bridge = {
        let engine = engine();
        let mut inner = engine.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.take_bridge(&verifier)
    };
    if let Some(bridge) = bridge {
        let _ = bridge.stop().await;
    }
    if let Some(member) = next.member_mut(&verifier) {
        member.state = MemberState::Parked;
        member.parked_at = Some(now());
    }
}

/// The turn is over: the panel's clock stops.
///
/// Two callers, one owner. `drive_turn` reaches here when the agent answers, and
/// `end_member_turn` when the engine cancels a turn it no longer needs. Both are
/// the end of a turn, and a clock left running is a panel that says a member has
/// been working for forty minutes on a turn that ended in the first one.
///
/// The state is not touched here. `drive_turn` ends the lead's turns too, and a
/// lead put to `Idle` between turns is a lead the wedge detector stops watching —
/// which is the failure `after_amendment` was written to fix. `end_member_turn`
/// sets the state itself, after this, because it is the one caller that knows the
/// engine has no further use for the turn.
fn stop_turn_clock(member_id: &str) {
    if let Some(watch) = watch_of(member_id) {
        watch.turn_ended();
    }
}

/// Whether a member's agent is mid-turn.
///
/// A member with no bridge never started, which is a different failure and not
/// one a clock can diagnose.
fn turn_in_flight(member_id: &str) -> bool {
    let engine = engine();
    let inner = engine.inner.lock().unwrap_or_else(|e| e.into_inner());
    inner
        .bridges
        .get(member_id)
        .map(|bridge| bridge.busy())
        .unwrap_or(false)
}

/// The watch for a member, if it has one.
///
/// The watch is created when the member's process starts and dropped when the run
/// is cleared, so a member that is working always has one. A member that does not
/// is a member that never started, and the caller treats a missing reading as "no
/// evidence" rather than as silence.
fn watch_of(member_id: &str) -> Option<Arc<super::watch::MemberWatch>> {
    let engine = engine();
    let inner = engine.inner.lock().unwrap_or_else(|e| e.into_inner());
    inner.watches.get(member_id).cloned()
}

/// What a member's stream says about its liveness.
///
/// A member with no watch reports no reading, which the wedge check treats as
/// "fall back to the clock" — the behaviour every run before this one had.
fn pulse_of(member_id: &str) -> super::watch::Pulse {
    match watch_of(member_id) {
        Some(watch) => watch.pulse(),
        None => super::watch::Pulse::default(),
    }
}

/// How long a member has been silent, in seconds, and `None` when it has no
/// clock to read.
///
/// The one place the idleness of a member is computed. Three separate
/// `last_at` deltas had grown in the loop, and the board lines that reported them
/// printed the *threshold* instead of the reading — "no evidence after 180s" on
/// every nudge, whatever the member had actually been quiet for.
fn silence_secs(member: &Member) -> Option<i64> {
    let last = member.last_at.as_deref()?;
    let at = chrono::DateTime::parse_from_rfc3339(last).ok()?;
    Some((chrono::Utc::now() - at.with_timezone(&chrono::Utc)).num_seconds())
}

/// Why a nudge to this member cannot be delivered, and `None` when it can.
fn undeliverable(run: &Run, member_id: &str) -> Option<String> {
    let engine = engine();
    let inner = engine.inner.lock().unwrap_or_else(|e| e.into_inner());
    if !inner.bridges.contains_key(member_id) {
        return Some("its agent process is not running".to_string());
    }
    if run
        .member(member_id)
        .and_then(|m| m.session_id.as_ref())
        .is_none()
    {
        return Some("it has no session to answer in".to_string());
    }
    None
}

/// Charge one nudge against a member, and reset its silence clock only when the
/// nudge can actually reach it.
///
/// The clock is the point. `last_at` is a member's liveness evidence, and stamping
/// it for a message nobody received is what let a dead agent look busy: the board
/// said "asked again" while nothing had been sent, and the silence counter went
/// back to zero every tick. A nudge that cannot be delivered also stops being
/// retried — the answer to "the agent is not running" is a respawn, not another
/// prompt — so the budget is spent here and the member is handed to that path.
fn charge_nudge(run: &mut Run, member_id: &str) -> Option<String> {
    let why = undeliverable(run, member_id);
    if let Some(member) = run.member_mut(member_id) {
        member.state = MemberState::Working;
        match why {
            None => {
                member.nudges += 1;
                member.last_at = Some(now());
            }
            Some(_) => member.nudges = NUDGE_ATTEMPTS,
        }
    }
    why
}

fn member_is_silent(member_id: &str, member: &Member) -> bool {
    let Some(idle) = silence_secs(member) else {
        return false;
    };
    if idle < LEAD_NUDGE_SECS {
        return false;
    }
    let engine = engine();
    let inner = engine.inner.lock().unwrap_or_else(|e| e.into_inner());
    match inner.bridges.get(member_id) {
        Some(bridge) => !bridge.busy(),
        // No bridge at all: the member never started, which is a different
        // failure and not one a prompt can fix.
        None => false,
    }
}

/// Send the lead a turn that asks it to fix its plan.
///
/// The violations are the prompt. A lead that is told "no integration suite" can
/// add one; a lead that is told "the plan was refused" cannot. The turn is not
/// awaited — the drive loop owns the run's progress, and a turn can take minutes.
fn nudge_member(sink: &Arc<dyn EventSink>, run: &Run, member_id: &str, text: String) {
    let (bridge, session_id) = {
        let engine = engine();
        let inner = engine.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(bridge) = inner.bridges.get(member_id).cloned() else {
            return;
        };
        let Some(session_id) = run.member(member_id).and_then(|m| m.session_id.clone()) else {
            return;
        };
        (bridge, session_id)
    };

    let member = member_id.to_string();
    let sink = sink.clone();
    tokio::spawn(async move {
        // `AcpBridge::turn` refuses with `TurnInProgress` while a turn is in
        // flight, and the old code discarded that error. The retry path nudges a
        // worker whose turn was cancelled a second earlier, so the refusal was
        // the common case rather than the rare one: the fifth live run's retry
        // was dropped here, the worker finished its original turn, and the task
        // sat `running` with no worker until the turn ceiling fired half an hour
        // later. Waiting for the turn to end is the whole fix.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(NUDGE_WAIT_SECS);
        while bridge.busy() && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_secs(NUDGE_POLL_SECS)).await;
        }
        if bridge.busy() {
            // Still mid-turn after the wait. Say so rather than dropping the
            // nudge in silence: the wedge detector will fail the member, and a
            // board line is what tells the user which member and why.
            engine().post(
                BoardEntry::new(
                    "engine",
                    BoardKind::Note,
                    format!(
                        "{member} was still mid-turn after {NUDGE_WAIT_SECS}s — the nudge was not delivered"
                    ),
                )
                .about("run"),
            );
            return;
        }
        drive_turn(&sink, &bridge, &member, &session_id, &text).await;
    });
}

fn write_run(run: &Run) {
    let engine = engine();
    let mut inner = engine.inner.lock().unwrap_or_else(|e| e.into_inner());
    // The in-memory copy is what the panel reads, so it needs the end time too.
    // `save_at` stamps the file; this stamps the copy the snapshot is built
    // from, and both are idempotent.
    let mut run = run.clone();
    if run.state.is_terminal() {
        run.finish();
    }
    inner.run = Some(run);
}

/// Stop every member's agent process and forget it.
///
/// The bridges live in the engine's map, which is `'static`, so a run that
/// finished without this left one `vibe-acp` process per member alive until the
/// app itself exited. Eight members per run and a handful of runs is dozens of
/// idle processes, and the orphaned ones hold their worktree directories open,
/// which is why cleaning a project between runs failed with "Permission denied"
/// until the app was restarted.
///
/// Two callers, one owner: the stop path, and the drive loop when it finds the
/// run terminal. The second is the one that matters for a headless run — the
/// process could not exit while its members were still alive, and the buffered
/// final report was never flushed.
/// Release every process and checkout a finished run no longer needs.
///
/// Two callers, one owner: the drive loop when it finds the run terminal, and the
/// headless runner, whose own poll loop returns from `main` and drops the runtime
/// — which cancels the drive loop before its next tick can release. That race lost
/// once: the run reported `done` with four members still running and no process
/// left to stop them. Releasing twice is safe; the second call releases nothing.
pub async fn end_of_run() -> usize {
    {
        let mut inner = engine().inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(id) = inner.run.as_ref().map(|run| run.id.clone()) else {
            return 0;
        };
        if inner.ended.as_deref() == Some(id.as_str()) {
            return 0;
        }
        inner.ended = Some(id);
    }
    let released = release_bridges().await;
    if released > 0 {
        engine().post(
            BoardEntry::new(
                "engine",
                BoardKind::Note,
                format!(
                    "run finished — released {released} agent process{}",
                    if released == 1 { "" } else { "es" }
                ),
            )
            .about("run"),
        );
    }
    release_checkouts();
    released
}

pub async fn release_bridges() -> usize {
    let bridges: Vec<(String, Arc<AcpBridge>)> = {
        let mut inner = engine().inner.lock().unwrap_or_else(|e| e.into_inner());
        let ids: Vec<String> = inner.bridges.keys().cloned().collect();
        // Through `take_bridge`: a release this engine performed must be recorded,
        // or the awaiting turn answers it by spawning a replacement.
        let taken = ids
            .into_iter()
            .filter_map(|id| inner.take_bridge(&id).map(|bridge| (id, bridge)))
            .collect();
        inner.watches.clear();
        taken
    };
    let released = bridges.len();
    for (_, bridge) in bridges {
        // `stop_process` kills the child before taking the stdin lock, so a
        // write blocked on a full pipe cannot hold the stop open. `stop`
        // still bounds the child's exit at five seconds.
        let _ = bridge.stop().await;
    }
    released
}

/// Remove the checkouts a finished run no longer needs.
///
/// Called once, from the drive loop's exit path — after every member process has
/// been stopped. It used to run at the `Done` transition, which is before the
/// bridges are released, so on Windows every run left its directories behind and
/// posted "could not be removed: the process cannot access the file because it is
/// being used by another process". The directories were empty by then; the
/// handles were the member `vibe-acp` processes, still alive.
///
/// Only a `Done` run releases. A run that failed or was interrupted keeps its
/// checkouts on purpose: they are the only place the half-finished work exists,
/// and a post-mortem needs them.
///
/// A failure to remove is posted rather than swallowed. A checkout that survives
/// makes the next run's `worktree add` on that path fail, which is a thing the
/// user needs to see now, not a mystery later.
fn release_checkouts() {
    // This run's own checkouts first, then the retained runs behind it.
    //
    // The order matters twice over. `reap_checkouts` treats a `Done` run as
    // reapable, so reaping first reported this run's own directories as an
    // "older run no longer kept". And the reap path swallows removal failures,
    // where `release_run` returns them, so the run that matters most was the one
    // whose failure went unreported.
    let run = {
        let inner = engine().inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.run.clone()
    };
    let done = run.filter(|run| run.state == RunState::Done);
    let mut problems = Vec::new();
    if let Some(run) = &done {
        let workspace = PathBuf::from(&run.workspace);
        problems = worktree::release_run(&CommandGit, &workspace, &run.id);
        // The brief names this directory to every member, so it is where scratch
        // work lands; it goes with the checkouts.
        let scratch = super::run_dir(&run.id).join("scratch");
        if scratch.is_dir() && std::fs::remove_dir_all(&scratch).is_err() {
            problems.push(format!(
                "the scratch directory {} could not be removed",
                scratch.display()
            ));
        }
    }
    // Bounded retention, on every exit whatever this run's state: a failed or
    // interrupted run keeps its checkouts for a post-mortem, but only the ten
    // most recent ones.
    let reaped = worktree::reap_checkouts(&CommandGit);
    if !reaped.is_empty() {
        engine().post(
            BoardEntry::new(
                "engine",
                BoardKind::Note,
                format!(
                    "reaped the checkouts of {} older run{} no longer kept: {}",
                    reaped.len(),
                    if reaped.len() == 1 { "" } else { "s" },
                    reaped.join(", ")
                ),
            )
            .about("run"),
        );
    }
    if !problems.is_empty() {
        engine().post(
            BoardEntry::new(
                "engine",
                BoardKind::Note,
                format!(
                    "{} checkout{} could not be removed: {}",
                    problems.len(),
                    if problems.len() == 1 { "" } else { "s" },
                    problems.join("; ")
                ),
            )
            .about("run"),
        );
    }
}

fn emit(sink: &Arc<dyn EventSink>, run: &Run) {
    sink.emit(
        EVENT_SWARM,
        json!({
            "runId": run.id,
            "state": run.state,
            "tasks": run.tasks.len(),
            "verified": run.tasks.iter().filter(|t| t.state.is_verified()).count(),
        }),
    );
}

/* ------------------------------------------------------------------ *
 * Views
 * ------------------------------------------------------------------ */

/// A violation, flattened for the panel.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ViolationView {
    pub key: String,
    pub message: String,
}

impl From<&Violation> for ViolationView {
    fn from(violation: &Violation) -> Self {
        Self {
            key: violation.key().to_string(),
            message: violation.message(),
        }
    }
}

/// What the rehearsal returns to the panel.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RehearsalView {
    pub violations: Vec<ViolationView>,
    pub shape: plan::PlanShape,
    pub rehearsal: schedule::Rehearsal,
    pub concurrency: usize,
    /// What each role's turns have actually cost in this workspace.
    ///
    /// Empty until two runs have finished here. One observation is not a median,
    /// and a review that showed a single run's number under that word would be
    /// presenting an anecdote as a measurement — which is the mistake this phase
    /// exists to stop making about the budget. The observed run's verifier model
    /// took 20.7 minutes against the worker's 6, and a user choosing a verifier
    /// model should see that number before the run rather than after it.
    pub role_medians: Vec<history::RoleMedian>,
}

impl RehearsalView {
    pub fn valid(&self) -> bool {
        self.violations.is_empty()
    }
}

/// The gotchas a future run would receive for a set of files.
pub fn gotchas_for(gotchas: &[Gotcha], files: &[String]) -> Vec<Gotcha> {
    reflect::relevant(gotchas, files)
        .into_iter()
        .cloned()
        .collect()
}

/// The capability keys the swarm depends on, for the panel's dim state.
pub fn required_capabilities() -> Vec<&'static str> {
    CapabilityId::ALL.iter().map(|id| id.key()).collect()
}

/// The MCP-backed capabilities whose server is not declared to a member.
///
/// Most capabilities run on a boundary VibeDesk owns — the model request, the
/// transcript — so a member gets them by existing. The rest arrive only over
/// MCP, and a member gets them only if the server is in the list its session is
/// opened with. A capability that is switched on but not declared is the failure
/// this function exists to name: the panel would otherwise certify it and the run
/// would never reach it.
///
/// The channeler is MCP-backed but has no external server to look for — it is
/// the app's own loopback server, declared to every session — so `server()`
/// returns `None` for it and it is never reported here.
pub fn unreachable_capabilities(cwd: Option<&Path>) -> Vec<&'static str> {
    let declared: Vec<String> = crate::config::mcp_servers_for_acp(cwd)
        .iter()
        .filter_map(|entry| {
            entry
                .get("name")
                .and_then(|name| name.as_str())
                .map(str::to_string)
        })
        .collect();
    unreachable_from(&declared)
}

/// The same check against a list of server names, so it can be tested.
fn unreachable_from(declared: &[String]) -> Vec<&'static str> {
    CapabilityId::ALL
        .into_iter()
        .filter(|id| id.boundary() == capabilities::Boundary::McpServer)
        .filter(|id| {
            capabilities::external::server(*id)
                .map(|server| !declared.iter().any(|name| name == server.name))
                .unwrap_or(false)
        })
        .map(|id| id.key())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swarm::{Integration, Reflection, Task};

    /// Twenty minutes without a commit is `stalled`, not `slow`.
    ///
    /// The audited run reported `working` for five worktrees that held README.md
    /// and .git after thirty minutes, so the status payload could not tell a slow
    /// worker from an idle one. Every row below is a case that run produced, or a
    /// case that would have hidden the finding.
    #[test]
    fn twenty_minutes_without_a_commit_is_reported_as_stalled_not_slow() {
        let head = "a".repeat(40);
        let task = |age: i64| {
            let mut task = Task::new("t1", "t", "d", "a");
            task.state = TaskState::Running;
            task.base_commit = Some(head.clone());
            task.started_at = Some(
                (chrono::Utc::now() - chrono::Duration::seconds(age))
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            );
            task
        };

        // A worker past the threshold with its branch still at base: report it,
        // and report how long it has actually been silent.
        let age = stalled_secs(&task(STALL_SECS + 60), &head).expect("stalled");
        assert!((age - (STALL_SECS + 60)).abs() <= 2, "reported {age}s");

        // Inside the threshold it is still just working.
        assert!(stalled_secs(&task(60), &head).is_none());
        assert!(stalled_secs(&task(STALL_SECS - 1), &head).is_none());

        // One commit moves the tip off base, and one commit is work: a task that
        // is slow but committing is never called stalled.
        assert!(stalled_secs(&task(STALL_SECS * 3), &"b".repeat(40)).is_none());

        // Reported once. A second tick does not repeat the note.
        let mut reported = task(STALL_SECS * 2);
        reported.stalled = true;
        assert!(stalled_secs(&reported, &head).is_none());

        // A task that never started has no age to be silent for.
        let mut unstarted = task(0);
        unstarted.started_at = None;
        assert!(stalled_secs(&unstarted, &head).is_none());

        // A verified task is not running, whatever its branch tip says.
        let mut verified = task(STALL_SECS * 2);
        verified.state = TaskState::Verified;
        assert!(stalled_secs(&verified, &head).is_none());
    }

    /// The report that failed the audited run and could never have passed.
    ///
    /// `base_checkout_for` resets the base checkout before the criterion is
    /// proven, so a verifier's leftover edit can never make a real failure look
    /// like a pass. The `base_dirtied` read that followed the reset found a clean
    /// tree every time, which made `Verdict::is_earned`'s clause dead: the only
    /// way it could fire was a write during the proof itself. Reading before the
    /// reset is what gives the clause a way to be true.
    #[test]
    fn a_base_the_verifier_left_dirty_is_reported_before_the_reset_hides_it() {
        use std::process::Command;

        let workspace =
            std::env::temp_dir().join(format!("vd-dirtied-base-{}", super::super::new_run_id()));
        std::fs::create_dir_all(&workspace).unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(&workspace)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "base@local"]);
        git(&["config", "user.name", "base"]);
        git(&["config", "commit.gpgsign", "false"]);
        git(&["config", "core.autocrlf", "false"]);
        std::fs::write(workspace.join("base.txt"), "base\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "base"]);
        let base = git(&["rev-parse", "HEAD"]);

        let run = Run::new(
            "dirtied base",
            workspace.to_string_lossy().to_string(),
            Member::new("lead", Role::Lead, "m"),
        );
        let mut task = Task::new("t1", "One", "Do it.", "node --test test/a.test.js")
            .with_files(&["src/a.js"]);
        task.base_commit = Some(base.clone());

        // The verifier entered its base checkout — it has to, the failing-first
        // proof runs there — and left an edit behind.
        let checkout = worktree::task_base_path(&run.id, "t1");
        worktree::ensure_detached(&CommandGit, &workspace, &checkout, &base).unwrap();
        std::fs::write(checkout.join("planted.txt"), "the verifier wrote this\n").unwrap();
        assert!(
            worktree::is_dirty(&CommandGit, &checkout).unwrap(),
            "the checkout starts dirty"
        );

        let (path, dirtied) = super::base_checkout_for(&run, &task);

        assert!(dirtied, "the dirty baseline was observed before the reset");
        assert_eq!(path, checkout, "and it is the task's own base");
        assert!(
            !worktree::is_dirty(&CommandGit, &path).unwrap(),
            "the reset still happened, so the proof measures the commit"
        );
        assert!(
            !path.join("planted.txt").exists(),
            "the verifier's file is gone from the baseline"
        );

        let problems = worktree::release_run(&CommandGit, &workspace, &run.id);
        assert!(problems.is_empty(), "{problems:?}");
    }

    /// A verifier's escalation is about the criterion, so the *worker* runs
    /// again — the verifier does not go back to work.
    ///
    /// Returning the verifier to `Working` would leave a member burning with
    /// nothing to do, which is the wedge the amendment path exists to avoid.
    #[test]
    fn an_amendment_after_a_verifier_escalation_parks_the_verifier_and_requeues_the_task() {
        let mut run = planning_run("criterion");
        let mut task = Task::new("t1", "one", "do it", "node --test test/a.test.js");
        task.state = TaskState::Ready;
        run.tasks.push(task);
        run.members.push(Member::new("w-t1", Role::Worker, "m"));
        let mut verifier = Member::new("v-t1", Role::Verifier, "m");
        verifier.task = Some("t1".into());
        verifier.state = MemberState::Working;
        run.members.push(verifier);
        run.state = RunState::Replanning;

        let next = after_amendment(&run, Some("v-t1"));

        assert_eq!(next.state, RunState::Running);
        assert_eq!(next.lead.state, MemberState::Parked);
        assert_eq!(
            next.member("v-t1").unwrap().state,
            MemberState::Parked,
            "a verifier that escalated has nothing to do until the work is back"
        );
        assert_eq!(
            next.member("w-t1").unwrap().state,
            MemberState::Queued,
            "the worker is the one that has to run again, and it has no process yet"
        );
        assert_eq!(
            next.task("t1").unwrap().state,
            TaskState::Ready,
            "the task is requeued so the drive loop dispatches it"
        );
    }

    /* -------------------------------------------------------------- *
     * The integrated suite: a red one is repaired, not fatal
     * -------------------------------------------------------------- */

    /// A run whose tasks are all verified, with a suite that just failed.
    fn merged_run(repairs: u32) -> Run {
        let mut run = Run::new("goal", "/tmp", Member::new("lead", Role::Lead, "m"));
        run.state = RunState::Integrating;
        run.integration.suite = Some("npm test".into());
        run.integration.repairs = repairs;
        for (id, files) in [("t1", vec!["src/a.js"]), ("t2", vec!["src/b.js"])] {
            let mut task = Task::new(id, id, "do it", "node --test");
            task.files = files.iter().map(|f| f.to_string()).collect();
            task.state = TaskState::Verified;
            task.branch = Some(format!("swarm/run-x/{id}"));
            run.tasks.push(task);
        }
        run
    }

    /// The repair is a task, so it inherits the whole machinery: the deps make
    /// `prepare_worker` merge every verified branch into its base, which is the
    /// tree the suite failed on, and the suite is its criterion, so the verifier
    /// proves the whole rather than a part.
    #[test]
    fn a_failed_suite_becomes_a_task_that_starts_from_the_merged_tree() {
        let run = merged_run(0);
        let task = repair_task(&run, "npm test", "AssertionError: boom", Some("t2"));

        assert_eq!(task.id, "fix-1");
        assert_eq!(task.state, TaskState::Ready, "it must be dispatchable");
        assert_eq!(task.acceptance, "npm test", "the suite is the criterion");
        assert_eq!(
            task.deps,
            vec!["t1".to_string(), "t2".to_string()],
            "every verified task, so the base is the merged tree"
        );
        assert_eq!(task.files, vec!["src/b.js".to_string()], "the blamed task's files");
        assert!(task.detail.contains("AssertionError: boom"), "{}", task.detail);
        assert!(task.detail.contains("t2"), "{}", task.detail);
        assert!(
            task.detail.contains("Do not weaken the suite"),
            "a repair that edits the criterion has repaired nothing: {}",
            task.detail
        );
    }

    /// The round number comes from the run, so a second repair is `fix-2` and
    /// the ceiling counts what actually happened.
    #[test]
    fn a_second_repair_round_is_numbered_from_the_run() {
        let run = merged_run(1);
        let task = repair_task(&run, "npm test", "still red", None);
        assert_eq!(task.id, "fix-2");
        assert!(task.title.contains("round 2"), "{}", task.title);
        // Nothing was attributed, so nothing is claimed: an empty file list
        // constrains no ownership, which is right for a task that may have to
        // touch anything.
        assert!(task.files.is_empty());
        assert!(!task.detail.contains("attributed to"), "{}", task.detail);
    }

    /// The ceiling is a ceiling: past it the run fails and says how many rounds
    /// it spent, rather than looping on a suite that cannot pass.
    #[test]
    fn the_repair_ceiling_is_two_rounds() {
        assert_eq!(MAX_REPAIR_ROUNDS, 2);
        let run = merged_run(MAX_REPAIR_ROUNDS);
        assert!(
            run.integration.repairs >= MAX_REPAIR_ROUNDS,
            "the guard in the loop is `repairs < MAX_REPAIR_ROUNDS`"
        );
    }

    /* -------------------------------------------------------------- *
     * Adoption: a run this process did not start
     * -------------------------------------------------------------- */

    /// Hold the driver's lock the way another process would.
    ///
    /// Windows locks are per handle, so a second handle in this process conflicts
    /// with the first exactly as another process would. That is what makes these
    /// tests real rather than a simulation of one.
    fn another_driver(dir: &Path) -> std::fs::File {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(dir.join(DRIVER_LOCK))
            .unwrap();
        file.try_lock().unwrap();
        file
    }

    /// An engine already showing a run, adopted or owned.
    ///
    /// The adoption tests drive the mechanism directly rather than through
    /// `list_runs`: every test in this binary writes into the same data
    /// directory, so "which live run wins" is a scheduling question. What is not
    /// a scheduling question is what the engine does with the run it has.
    fn engine_showing(run: &Run, adopted: bool) -> Engine {
        let engine = Engine::default();
        {
            let mut inner = engine.inner.lock().unwrap();
            inner.run = Some(run.clone());
            inner.board = Board::load_at(&run.dir());
            inner.adopted = adopted;
        }
        engine
    }

    fn planning_run(goal: &str) -> Run {
        let mut run = Run::new(goal, "/tmp", Member::new("lead", Role::Lead, "m"));
        run.state = RunState::Planning;
        run.save().unwrap();
        run
    }

    #[test]
    fn a_lock_nobody_holds_means_no_driver() {
        let run = planning_run("no driver");
        assert!(!driver_is_alive(&run.dir()));
        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    #[test]
    fn a_held_lock_means_a_driver_and_dropping_it_means_none() {
        let run = planning_run("held");
        let driver = another_driver(&run.dir());
        assert!(driver_is_alive(&run.dir()), "a held lock is a live driver");
        drop(driver);
        assert!(!driver_is_alive(&run.dir()), "a dropped lock is a dead driver");
        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    /// No bridge, so no turn is in flight. The tests that care about a live turn
    /// pass their own closure.
    fn never_busy(_: &str) -> bool {
        false
    }

    /// A member whose stream says nothing about its liveness.
    ///
    /// Every test written before the pulse existed passes this, which is the
    /// point: a member with no reading must behave exactly as it did when the
    /// ceiling was the only clock.
    fn no_pulse(_: &str) -> super::super::watch::Pulse {
        super::super::watch::Pulse::default()
    }

    /// A member whose model call has said nothing for `quiet` seconds, with no
    /// tool running.
    fn quiet_for(quiet: i64) -> impl Fn(&str) -> super::super::watch::Pulse {
        move |_| super::super::watch::Pulse {
            quiet: Some(quiet),
            tool: false,
        }
    }

    /// A member that has been quiet for `quiet` seconds with a tool running.
    fn busy_tool(quiet: i64) -> impl Fn(&str) -> super::super::watch::Pulse {
        move |_| super::super::watch::Pulse {
            quiet: Some(quiet),
            tool: true,
        }
    }

    /// The board line reports the silence it measured, not the threshold it
    /// tripped.
    ///
    /// The audited run's board carries three entries for one worker in one
    /// second, two of them restarts, and every nudge line reads "no evidence
    /// after 180s" — because 180 was `LEAD_NUDGE_SECS` interpolated into the
    /// sentence. The worker's real silence was far longer, and the number that
    /// would have shown it was never printed or even computed at the call site.
    #[test]
    fn silence_is_measured_rather_than_assumed() {
        let mut member = Member::new("w-t1", Role::Worker, "m");
        assert_eq!(silence_secs(&member), None, "no clock, no reading");

        member.last_at = Some(
            (chrono::Utc::now() - chrono::Duration::seconds(1004)).to_rfc3339_opts(
                chrono::SecondsFormat::Millis,
                true,
            ),
        );
        let idle = silence_secs(&member).unwrap();
        assert!((1003..=1005).contains(&idle), "{idle}");
        assert!(
            idle > LEAD_NUDGE_SECS,
            "1004s of silence must not be reported as the 180s threshold"
        );
    }

    /// A nudge no one can receive does not reset the liveness clock.
    ///
    /// `last_at` is a member's liveness evidence. Stamping it before the send was
    /// the difference between "the agent is not running" and "the agent is
    /// thinking", and the audited run read the second when it was the first:
    /// "w-t3 stopped responding — restarted it on t3" and "w-t3 could not get an
    /// answer: the Vibe agent is not running" landed in the same second as the
    /// nudge that had supposedly been delivered.
    #[test]
    fn a_nudge_that_cannot_be_delivered_does_not_reset_the_clock() {
        let ago = (chrono::Utc::now() - chrono::Duration::seconds(1004)).to_rfc3339_opts(
            chrono::SecondsFormat::Millis,
            true,
        );
        let mut run = planning_run("undeliverable");
        let mut worker = Member::new("w-t1", Role::Worker, "m");
        worker.state = MemberState::Working;
        worker.last_at = Some(ago.clone());
        worker.session_id = Some("session-1".into());
        run.members = vec![worker];

        // No bridge is registered for this member in a test process, which is
        // exactly the state the audited run's w-t3 was in.
        let why = charge_nudge(&mut run, "w-t1");

        assert!(why.is_some(), "the member cannot be reached");
        let worker = run.member("w-t1").unwrap();
        assert_eq!(
            worker.last_at.as_deref(),
            Some(ago.as_str()),
            "the clock is untouched, so the silence is still 1004s"
        );
        assert_eq!(
            worker.nudges, NUDGE_ATTEMPTS,
            "and the budget is spent, so the tick does not retry a dead member"
        );
    }

    /// A run with one task in `state`, owned by a worker that has been silent
    /// since `ago` seconds.
    fn run_with_silent_worker(state: TaskState, ago: i64) -> Run {
        let mut run = planning_run("wedge");
        run.state = RunState::Running;
        let mut task = Task::new("t1", "do the thing", "the detail", "the criterion");
        task.state = state;
        task.owner = Some("w-t1".into());
        run.tasks = vec![task];

        let mut worker = Member::new("w-t1", Role::Worker, "m");
        worker.task = Some("t1".into());
        worker.state = MemberState::Working;
        worker.last_at = Some(
            (chrono::Utc::now() - chrono::Duration::seconds(ago)).to_rfc3339_opts(
                chrono::SecondsFormat::Millis,
                true,
            ),
        );
        run.members = vec![worker];
        run
    }

    /// A member whose model call failed is restarted, not left `working`.
    ///
    /// The turn result used to be discarded, so a provider that refused every
    /// request left the member `working` until the thirty-minute ceiling and the
    /// run then died with "a turn in flight with no sign of life" — a sentence
    /// that names neither the provider nor the error.
    #[test]
    fn a_member_whose_turn_failed_is_restarted_with_the_reason() {
        let mut run = run_with_silent_worker(TaskState::Running, 0);
        let restart = after_turn_error(
            &mut run,
            "w-t1",
            RestartCause::Provider,
            "agent request failed (500): upstream answered 400",
        )
            .expect("a worker with restarts left is restarted");

        assert_eq!(restart.member, "w-t1");
        assert_eq!(restart.task, "t1");
        assert!(!restart.verify);
        assert!(
            restart
                .reason
                .as_deref()
                .unwrap_or_default()
                .contains("upstream answered 400"),
            "the retry has to carry the provider's own words: {restart:?}"
        );
        let worker = run.member("w-t1").unwrap();
        assert_eq!(worker.state, MemberState::Starting);
        assert_eq!(worker.respawns, 1);
        assert_eq!(worker.session_id, None, "a fresh process gets a fresh session");
        assert!(worker.error.as_deref().unwrap_or_default().contains("400"));
        assert_eq!(
            run.task("t1").unwrap().state,
            TaskState::Running,
            "one failed turn is not a failed task"
        );
    }

    /// A run that has ended restarts nothing. The release wakes every awaiting
    /// turn with a closed connection, and one verdict-verified task was respawned.
    #[test]
    fn a_finished_run_restarts_nothing_and_charges_nothing() {
        let mut run = run_with_silent_worker(TaskState::Verified, 0);
        run.state = RunState::Done;
        assert!(
            after_turn_error(
                &mut run,
                "w-t1",
                RestartCause::Provider,
                "the agent closed the connection",
            )
            .is_none(),
            "a run that has ended has no member left to restart"
        );
        let worker = run.member("w-t1").unwrap();
        assert_eq!(
            worker.state,
            MemberState::Parked,
            "the roster stops describing a task that ended"
        );
        assert_eq!(worker.respawns, 0, "nothing was restarted, so nothing is charged");
        assert_eq!(worker.restarts_infra, 0);
        assert_eq!(worker.restarts_provider, 0);
        assert!(worker.error.as_deref().unwrap_or_default().contains("closed"));
        assert_eq!(run.task("t1").unwrap().state, TaskState::Verified);
    }

    /// The restart is bounded, so a provider that is down cannot loop forever.
    #[test]
    fn a_member_at_the_respawn_ceiling_fails_its_task_with_the_reason() {
        let mut run = run_with_silent_worker(TaskState::Running, 0);
        run.member_mut("w-t1").unwrap().respawns = MAX_RESPAWNS;

        assert!(
            after_turn_error(&mut run, "w-t1", RestartCause::Provider, "upstream answered 400").is_none(),
            "the ceiling is what stops the loop"
        );
        assert_eq!(run.member("w-t1").unwrap().state, MemberState::Failed);
        assert_eq!(run.task("t1").unwrap().state, TaskState::Failed);
        assert!(run
            .member("w-t1")
            .unwrap()
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("400"));
    }

    /// A lead that cannot be reached has no plan and no way to get one.
    #[test]
    fn a_lead_whose_turn_failed_fails_the_run_with_the_reason() {
        let mut run = planning_run("lead-turn");
        run.lead.state = MemberState::Working;

        assert!(
            after_turn_error(&mut run, "lead", RestartCause::Provider, "upstream answered 400").is_none(),
            "a lead has no task to restart"
        );
        assert_eq!(run.state, RunState::Failed);
        assert_eq!(run.lead.state, MemberState::Failed);
        let failure = run.failure.clone().unwrap_or_default();
        assert!(failure.contains("the lead could not get an answer"), "{failure}");
        assert!(failure.contains("400"), "{failure}");
        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    #[test]
    fn a_silent_worker_on_a_running_task_is_a_wedge() {
        let run = run_with_silent_worker(TaskState::Running, MEMBER_IDLE_LIMIT_SECS + 60);
        let wedged = wedge(&run, &never_busy, &no_pulse).expect("a silent worker with work left is a wedge");
        assert_eq!(wedged.state, RunState::Failed);
        let failure = wedged.failure.clone().unwrap_or_default();
        assert!(
            failure.starts_with("a member stopped responding"),
            "{failure}"
        );
        assert!(
            failure.contains("w-t1"),
            "the reason has to name the member: {failure}"
        );
        assert_eq!(wedged.members[0].state, MemberState::Failed);
        assert!(wedged.members[0].error.as_deref().unwrap().contains("silent"));
        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    #[test]
    fn one_death_charges_one_restart() {
        // Both the failed-turn path and the idle path reach a dead member. One
        // exit must spend one slot: the audited run's w-t1 logged the nudge, the
        // idle restart, the failed nudge and the error restart inside 29 ms, and
        // ended at `respawns = 2` having restarted once — so it was never
        // restarted again and sat `Working` with no process.
        let mut run = run_with_silent_worker(TaskState::Running, MEMBER_IDLE_LIMIT_SECS + 60);

        let first = after_turn_error(
            &mut run,
            "w-t1",
            RestartCause::Infra,
            "the Vibe agent is not running",
        );
        assert!(first.is_some(), "the first death is restartable");
        let member = run.member("w-t1").unwrap();
        assert_eq!(member.respawns, 1, "one death, one slot");
        assert_eq!(member.state, MemberState::Starting);

        // The other path arriving at the same death must not charge again.
        let second = after_turn_error(
            &mut run,
            "w-t1",
            RestartCause::Infra,
            "the Vibe agent is not running",
        );
        assert!(
            second.is_none(),
            "a restart already in flight must not be charged twice"
        );
        assert_eq!(
            run.member("w-t1").unwrap().respawns,
            1,
            "the budget is spent once per death"
        );

        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    /// The role is set on whichever config option the session lists it on.
    ///
    /// A real run against `vibe-acp` 3.1.4 failed at the first spawn because the
    /// engine set the role on `mode`, which on that version is the permission mode
    /// (`ask`, `accept-edits`, `auto-approve`). The agent is a separate option. The
    /// 3.1.2 layout put the agent in `mode`, so the choice is read from the session
    /// rather than assumed, and neither list having the name is an error with both
    /// lists in it.
    #[test]
    fn the_agent_is_set_on_whichever_option_the_session_lists_it() {
        let modern = json!({ "configOptions": [
            { "id": "mode", "options": [{ "value": "ask" }, { "value": "auto-approve" }] },
            { "id": "agent", "options": [{ "value": "vibedesk-swarm-worker" }] },
        ]});
        assert_eq!(agent_option(&modern, "vibedesk-swarm-worker"), Some("agent"));

        let older = json!({ "configOptions": [
            { "id": "mode", "options": [{ "value": "ask" }, { "value": "vibedesk-swarm-worker" }] },
        ]});
        assert_eq!(agent_option(&older, "vibedesk-swarm-worker"), Some("mode"));

        // Neither list has it, so the caller refuses rather than leaving the
        // member on the session default — Vibe's coding prompt.
        assert_eq!(agent_option(&modern, "vibedesk-swarm-lead"), None);
        assert_eq!(agent_option(&json!({}), "vibedesk-swarm-worker"), None);
    }

    /// Which clock fired is a fact about the error, not a sentence to parse.
    ///
    /// The audit's third fault: the evidence nudge, the wedged limit and driver
    /// death all arrived as "restarted it on t3", so a dead process and a slow
    /// model were the same event in the log. Classification is by variant, so
    /// rewording a message cannot reclassify a death.
    #[test]
    fn a_restart_is_classified_by_the_error_that_caused_it() {
        use crate::error::AppError;

        assert_eq!(
            RestartCause::of(&AppError::AgentNotRunning),
            RestartCause::Infra
        );
        assert_eq!(
            RestartCause::of(&AppError::AgentExited("exit code 1".into())),
            RestartCause::Infra
        );
        assert_eq!(RestartCause::of(&AppError::NoSession), RestartCause::Infra);
        assert_eq!(
            RestartCause::of(&AppError::BinaryNotFound("vibe-acp".into())),
            RestartCause::Infra
        );
        assert_eq!(
            RestartCause::of(&AppError::TurnStalled(600)),
            RestartCause::Timeout
        );
        assert_eq!(RestartCause::of(&AppError::Timeout(120)), RestartCause::Timeout);
        assert_eq!(
            RestartCause::of(&AppError::Rpc {
                code: -32603,
                message: "upstream answered 400".into(),
            }),
            RestartCause::Provider
        );

        // The three remedies differ, so the sentences differ.
        assert_ne!(RestartCause::Infra.label(), RestartCause::Provider.label());
        assert_ne!(RestartCause::Infra.label(), RestartCause::Timeout.label());
    }

    /// Each cause has its own counter, and the budget stays one number.
    #[test]
    fn a_restart_is_counted_against_the_clock_that_fired() {
        let mut member = Member::new("w-t1", Role::Worker, "m");
        member.state = MemberState::Working;

        assert_eq!(
            charge_restart(&mut member, RestartCause::Infra),
            Charge::Allowed
        );
        assert_eq!((member.respawns, member.restarts_infra), (1, 1));

        member.state = MemberState::Working;
        assert_eq!(
            charge_restart(&mut member, RestartCause::Timeout),
            Charge::Allowed
        );
        assert_eq!((member.respawns, member.restarts_timeout), (2, 1));

        // The budget is spent: a third charge is refused whatever the cause, and
        // the counter it would have moved stays where it was.
        member.state = MemberState::Working;
        assert_eq!(
            charge_restart(&mut member, RestartCause::Provider),
            Charge::Exhausted
        );
        assert_eq!((member.respawns, member.restarts_provider), (2, 0));
        assert_eq!(member.restarts_infra, 1);
    }

    /// The run stops when the agent process keeps dying.
    ///
    /// Retrying is the right answer to a slow model and the wrong one to a process
    /// that is gone: every restart spends a member's budget and re-buys the
    /// context the task had accumulated, which is where the audited run's 51.5M
    /// cached tokens came from. The ceiling is small because the cause is shared —
    /// one process death is every worker's problem.
    #[test]
    fn the_run_stops_when_the_agent_process_keeps_dying() {
        let mut run = run_with_silent_worker(TaskState::Running, 0);
        assert_eq!(infra_overloaded(&run), None, "nothing has died yet");

        run.member_mut("w-t1").unwrap().restarts_infra = MAX_INFRA_RESTARTS;
        assert_eq!(
            infra_overloaded(&run),
            None,
            "the ceiling is the last restart that is still worth trying"
        );

        // Reached through the restart path, the run is failed rather than retried.
        run.member_mut("w-t1").unwrap().state = MemberState::Working;
        let restart = after_turn_error(
            &mut run,
            "w-t1",
            RestartCause::Infra,
            "the Vibe agent is not running",
        );
        assert!(restart.is_none(), "a dead process is not restarted again");
        assert_eq!(run.state, RunState::Failed);
        let failure = run.failure.clone().unwrap_or_default();
        assert!(
            failure.contains("died 4 times"),
            "the failure has to name the count: {failure}"
        );
        assert_eq!(run.member("w-t1").unwrap().state, MemberState::Failed);
        assert_eq!(run.task("t1").unwrap().state, TaskState::Failed);

        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    /// A real repo with one commit, and the sha of that commit.
    fn repo_with_a_commit(name: &str) -> (std::path::PathBuf, String) {
        let repo = std::env::temp_dir().join(format!(
            "vd-progress-{name}-{}",
            crate::swarm::new_run_id()
        ));
        std::fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(&repo)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "progress@local"]);
        git(&["config", "user.name", "progress"]);
        git(&["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join("a.txt"), "a\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "one"]);
        let head = git(&["rev-parse", "HEAD"]);
        (repo, head)
    }

    /// A worker that commits while saying nothing is working, not silent.
    ///
    /// The board is the only signal that carries evidence, and it is not the only
    /// one that carries work. The fifth live run nudged w-t1 twice while it was
    /// committing; the sixth failed a worker for silence after it had already
    /// committed. Both are the same blindness.
    #[test]
    fn a_worker_that_commits_is_working_not_silent() {
        let (repo, first) = repo_with_a_commit("commit");
        let mut run = run_with_silent_worker(TaskState::Running, LEAD_NUDGE_SECS + 60);
        {
            let worker = run.member_mut("w-t1").unwrap();
            worker.worktree = Some(repo.to_string_lossy().to_string());
            worker.head = Some(first.clone());
        }
        let before = run.member("w-t1").unwrap().last_at.clone();

        std::fs::write(repo.join("b.txt"), "b\n").unwrap();
        for args in [&["add", "-A"][..], &["commit", "-qm", "two"][..]] {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(&repo)
                .output()
                .unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        }

        let sink: Arc<dyn EventSink> = Arc::new(crate::acp::bridge::NullSink);
        let after = refresh_progress(&sink, run.clone());

        let worker = after.member("w-t1").unwrap();
        assert_ne!(
            worker.head.as_deref(),
            Some(first.as_str()),
            "the new commit is recorded"
        );
        assert_ne!(worker.last_at, before, "the silence clock moved forward");
        assert!(
            wedge(&after, &never_busy, &no_pulse).is_none(),
            "a committing worker is not wedged"
        );
        std::fs::remove_dir_all(&repo).ok();
        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    /// A worker that committed once and then stopped is still a wedge.
    ///
    /// The rescue is a moving HEAD, not a HEAD. A member that commits and then
    /// dies must still fail the run, or the worktree signal becomes a way to
    /// hang forever.
    /// A member that has never been probed is due.
    #[test]
    fn a_member_that_has_never_been_probed_is_due() {
        let member = Member::new("w-t1", Role::Worker, "m");
        assert!(head_probe_due(&member));
    }

    /// A member probed a moment ago is not due again.
    ///
    /// The probe is a `git rev-parse` at 46ms against a 400ms tick, so probing
    /// every tick was most of the loop's budget for a member that was quietly
    /// reading files.
    #[test]
    fn a_member_probed_a_moment_ago_is_not_due() {
        let mut member = Member::new("w-t1", Role::Worker, "m");
        member.head_at = Some(now());
        assert!(!head_probe_due(&member));
    }

    /// A member probed longer ago than the interval is due again.
    #[test]
    fn a_member_probed_past_the_interval_is_due() {
        let mut member = Member::new("w-t1", Role::Worker, "m");
        member.head_at = Some(
            (chrono::Utc::now() - chrono::Duration::seconds(HEAD_PROBE_SECS + 1))
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        );
        assert!(head_probe_due(&member));
    }

    /// A timestamp that cannot be parsed is probed rather than skipped.
    ///
    /// Failing open is the safe direction: a probe that runs too often costs
    /// 46ms, and a probe that never runs costs the run a worker it failed for
    /// silence after it had already committed.
    #[test]
    fn an_unparseable_probe_time_is_due_rather_than_skipped() {
        let mut member = Member::new("w-t1", Role::Worker, "m");
        member.head_at = Some("not a timestamp".into());
        assert!(head_probe_due(&member));
    }

    #[test]
    fn a_worker_that_committed_once_and_stopped_is_still_a_wedge() {
        let (repo, first) = repo_with_a_commit("stopped");
        let mut run = run_with_silent_worker(TaskState::Running, MEMBER_IDLE_LIMIT_SECS + 60);
        {
            let worker = run.member_mut("w-t1").unwrap();
            worker.worktree = Some(repo.to_string_lossy().to_string());
            worker.head = Some(first.clone());
        }

        let sink: Arc<dyn EventSink> = Arc::new(crate::acp::bridge::NullSink);
        let after = refresh_progress(&sink, run.clone());

        assert_eq!(
            after.member("w-t1").unwrap().head.as_deref(),
            Some(first.as_str()),
            "no new commit, so nothing to record"
        );
        assert!(
            wedge(&after, &never_busy, &no_pulse).is_some(),
            "a worker that stopped is still wedged"
        );
        std::fs::remove_dir_all(&repo).ok();
        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    /// A worker whose process is gone is restarted, not failed.
    ///
    /// The sixth live run's w-t1 sat at 0.015s of CPU for half an hour while the
    /// run showed `running`: its turn had stalled, `busy` stayed set, and the
    /// engine waited its 1800s turn ceiling for a reply that was never coming.
    /// The worktree held two commits. Failing there throws them away.
    #[test]
    fn a_silent_worker_on_a_running_task_is_restarted_not_failed() {
        let run = run_with_silent_worker(TaskState::Running, MEMBER_IDLE_LIMIT_SECS + 60);
        let sink: Arc<dyn EventSink> = Arc::new(crate::acp::bridge::NullSink);

        let (after, restarts) =
            respawn_dead(&sink, &run, &never_busy).expect("a dead worker is restarted");

        assert_eq!(restarts.len(), 1);
        assert_eq!(restarts[0].member, "w-t1");
        assert_eq!(restarts[0].task, "t1");
        assert!(!restarts[0].verify, "a worker is not a verifier");
        assert!(
            restarts[0].reason.is_some(),
            "a restarted worker needs to be told what to do"
        );
        let worker = after.member("w-t1").unwrap();
        assert_eq!(worker.respawns, 1);
        assert_eq!(worker.state, MemberState::Starting);
        assert_eq!(worker.nudges, 0, "a fresh process gets a fresh nudge budget");
        assert!(
            wedge(&after, &never_busy, &no_pulse).is_none(),
            "a restarted member is not wedged"
        );
        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    /// The restart is bounded, so a member that dies on spawn cannot loop.
    #[test]
    fn a_member_at_the_respawn_ceiling_is_left_to_the_wedge() {
        let mut run = run_with_silent_worker(TaskState::Running, MEMBER_IDLE_LIMIT_SECS + 60);
        run.member_mut("w-t1").unwrap().respawns = MAX_RESPAWNS;
        let sink: Arc<dyn EventSink> = Arc::new(crate::acp::bridge::NullSink);

        assert!(
            respawn_dead(&sink, &run, &never_busy).is_none(),
            "the ceiling is what stops the loop"
        );
        assert!(
            wedge(&run, &never_busy, &no_pulse).is_some(),
            "past the ceiling the run fails, and names the member"
        );
        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    /// A turn in flight is work, not a dead process.
    #[test]
    fn a_member_mid_turn_is_not_restarted() {
        let run = run_with_silent_worker(TaskState::Running, MEMBER_IDLE_LIMIT_SECS + 60);
        let sink: Arc<dyn EventSink> = Arc::new(crate::acp::bridge::NullSink);
        let in_flight = |_: &str| true;

        assert!(
            respawn_dead(&sink, &run, &in_flight).is_none(),
            "a turn in flight is not a dead process"
        );
        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    /// A worker whose task is already verified is idle by design.
    #[test]
    fn a_finished_worker_is_not_restarted() {
        let run = run_with_silent_worker(TaskState::Verified, MEMBER_IDLE_LIMIT_SECS + 60);
        let sink: Arc<dyn EventSink> = Arc::new(crate::acp::bridge::NullSink);

        assert!(
            respawn_dead(&sink, &run, &never_busy).is_none(),
            "a finished worker has nothing to restart for"
        );
        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    /// The lead is parked once it has answered, and the worker that asked is
    /// back at work — in the copy that gets saved.
    #[test]
    fn an_amendment_parks_the_lead_and_returns_the_worker_to_work() {
        let mut run = planning_run("amendment");
        run.state = RunState::Replanning;
        run.lead.state = MemberState::Working;
        let mut worker = Member::new("w-t1", Role::Worker, "m");
        worker.task = Some("t1".into());
        worker.state = MemberState::Parked;
        run.members = vec![worker];

        let next = after_amendment(&run, Some("w-t1"));

        assert_eq!(next.state, RunState::Running);
        assert_eq!(
            next.lead.state,
            MemberState::Parked,
            "a lead that has answered is parked, not silent"
        );
        assert!(next.lead.parked_at.is_some());
        assert_eq!(
            next.members[0].state,
            MemberState::Working,
            "the member that escalated carries on"
        );
        assert!(next.members[0].last_at.is_some());

        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    /// A task that could not start because its dependencies would not merge is
    /// waiting on the plan, and the amendment is the plan changing. It goes back
    /// in the queue; leaving it `Conflict` would leave it there forever, because
    /// the scheduler only unblocks `Blocked`.
    #[test]
    fn an_amendment_puts_a_conflicted_task_back_in_the_queue() {
        let mut run = planning_run("conflict-reset");
        run.state = RunState::Replanning;
        let mut task = Task::new("t3", "Three", "Do it.", "node --test test/step.test.js")
            .with_files(&["src/logic/step.js"])
            .with_deps(&["t1", "t2"]);
        task.state = TaskState::Conflict;
        task.conflict = Some("t2 conflicts on src/player.js".into());
        task.conflict_repairs = 1;
        run.tasks = vec![task];

        let next = after_amendment(&run, None);

        assert_eq!(next.state, RunState::Running);
        assert_eq!(
            next.tasks[0].state,
            TaskState::Blocked,
            "the scheduler only unblocks `Blocked`, so `Conflict` would never retry"
        );
        assert!(
            next.tasks[0].conflict.is_none(),
            "the reason is stale once the plan has changed"
        );
        assert_eq!(
            next.tasks[0].conflict_repairs, 1,
            "the repair budget is spent, not refunded"
        );

        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    /// A run waiting on the lead is not stuck. The conflicted task is exactly
    /// the shape `is_stuck` reads as "nothing can move", so the state has to be
    /// part of the check or the escalation is undone one line later.
    #[test]
    fn a_run_waiting_on_the_lead_is_not_failed_as_stuck() {
        let mut run = planning_run("waiting");
        run.state = RunState::Replanning;
        let mut conflicted = Task::new("t3", "Three", "Do it.", "node --test test/step.test.js")
            .with_files(&["src/logic/step.js"])
            .with_deps(&["t1", "t2"]);
        conflicted.state = TaskState::Conflict;
        let mut blocked = Task::new("t4", "Four", "Do it.", "node --test test/integration.test.js")
            .with_files(&["src/main.js"])
            .with_deps(&["t3"]);
        blocked.state = TaskState::Blocked;
        run.tasks = vec![conflicted, blocked];

        assert!(
            schedule::is_stuck(&run.tasks),
            "the tasks alone do look stuck — that is why the state is checked"
        );
        assert_ne!(
            run.state,
            RunState::Running,
            "and the run is not `Running`, so the drive loop leaves it alone"
        );

        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    /// `prepare_worker` reads the plan's ownership declaration and hands it to
    /// the merge, so a conflict between two dependencies is resolved from the
    /// plan rather than stopping the run.
    ///
    /// This is the whole mechanical path the twelfth live run died on: t1 and t2
    /// both verified, t1 had committed `src/player.js` outside its grant, t2
    /// owned it, and the run failed with "no task can make progress". The
    /// ownership map is built here from `run.tasks`, which is the part a unit
    /// test of `merge_deps` cannot reach.
    #[test]
    fn prepare_worker_resolves_a_dependency_conflict_from_the_plans_ownership() {
        use std::process::Command;

        let workspace = std::env::temp_dir().join(format!("vd-prep-{}", super::super::new_run_id()));
        std::fs::create_dir_all(&workspace).unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(&workspace)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "prep@local"]);
        git(&["config", "user.name", "prep"]);
        git(&["config", "commit.gpgsign", "false"]);
        git(&["config", "core.autocrlf", "false"]);
        std::fs::write(workspace.join("shared.js"), "base\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "base"]);
        let base = git(&["rev-parse", "HEAD"]);

        let mut run = Run::new("prep", workspace.to_string_lossy().to_string(), Member::new("lead", Role::Lead, "m"));
        run.integration.base = base.clone();

        // Two independent tasks that both changed `shared.js`. t2 owns it.
        let mut tips: BTreeMap<String, String> = BTreeMap::new();
        for (id, text) in [("t1", "t1 wrote this\n"), ("t2", "t2 wrote this\n")] {
            let branch = worktree::branch_name(&run.id, id);
            git(&["branch", &branch, &base]);
            let path = workspace.join(format!("mk-{id}"));
            git(&["worktree", "add", "-q", path.to_str().unwrap(), &branch]);
            std::fs::write(path.join("shared.js"), text).unwrap();
            let out = Command::new("git")
                .args(["commit", "-qam", id])
                .current_dir(&path)
                .output()
                .unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
            tips.insert(id.to_string(), git(&["rev-parse", &branch]));
            git(&["worktree", "remove", "--force", path.to_str().unwrap()]);
        }

        let mut t1 = Task::new("t1", "One", "Do it.", "node --test test/a.test.js")
            .with_files(&["src/gates.js"]);
        t1.state = TaskState::Verified;
        t1.branch = Some(worktree::branch_name(&run.id, "t1"));
        let mut t2 = Task::new("t2", "Two", "Do it.", "node --test test/b.test.js")
            .with_files(&["shared.js"]);
        t2.state = TaskState::Verified;
        t2.branch = Some(worktree::branch_name(&run.id, "t2"));
        let t3 = Task::new("t3", "Three", "Do it.", "node --test test/c.test.js")
            .with_files(&["src/step.js"])
            .with_deps(&["t1", "t2"]);
        run.tasks = vec![t1, t2, t3];

        let prep = prepare_worker(&run, "t3").expect("the conflict is resolved");
        let path = worktree::worktree_path(&run.id, "w-t3");

        assert_eq!(
            prep.resolved,
            vec![worktree::Resolved {
                file: "shared.js".into(),
                owner: "t2".into()
            }],
            "the plan says t2 owns the file, so t2's version is kept"
        );
        assert_eq!(
            std::fs::read_to_string(path.join("shared.js")).unwrap(),
            "t2 wrote this\n",
            "and t1's out-of-grant edit is dropped"
        );
        assert_ne!(prep.base, base, "the base side of the proof moved with the merge");
        assert_eq!(
            prep.built_on, tips,
            "and what it was built on is recorded, so a refused verdict upstream can find it"
        );

        std::fs::remove_dir_all(&workspace).ok();
        std::fs::remove_dir_all(worktree::worktree_root().join(&run.id)).ok();
        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    /// A leftover empty directory is not a worktree, and must not be reused as
    /// one.
    ///
    /// `discard_worktree` removes the registration and the contents, but on
    /// Windows a process that still holds the directory open leaves it behind
    /// empty. `prepare_worker` used to short-circuit on `is_dir()`, so the
    /// re-queued task was handed a tree with no `.git` in it and its first
    /// command failed as "not a git repository" — which escalated the run
    /// instead of retrying the task. The directory has to be replaced, not
    /// reused.
    #[test]
    fn a_leftover_empty_directory_is_replaced_rather_than_reused_as_a_worktree() {
        use std::process::Command;

        let workspace =
            std::env::temp_dir().join(format!("vd-stale-{}", super::super::new_run_id()));
        std::fs::create_dir_all(&workspace).unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(&workspace)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "stale@local"]);
        git(&["config", "user.name", "stale"]);
        git(&["config", "commit.gpgsign", "false"]);
        git(&["config", "core.autocrlf", "false"]);
        std::fs::write(workspace.join("a.txt"), "base\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "base"]);
        let base = git(&["rev-parse", "HEAD"]);

        let mut run = Run::new(
            "stale",
            workspace.to_string_lossy().to_string(),
            Member::new("lead", Role::Lead, "m"),
        );
        run.integration.base = base.clone();
        run.tasks = vec![Task::new("t1", "One", "Do it.", "node --test test/a.test.js")];

        // What `discard_worktree` leaves behind when a process still holds the
        // directory open: the path exists, and there is no worktree in it.
        let path = worktree::worktree_path(&run.id, "w-t1");
        std::fs::create_dir_all(&path).unwrap();
        assert!(path.is_dir(), "the leftover directory is there");
        assert!(
            worktree::head(&CommandGit, &path).is_err(),
            "and it is not a worktree"
        );

        let prep = prepare_worker(&run, "t1").expect("the stale directory is replaced");

        assert_eq!(prep.base, base, "the task starts from the run's base");
        assert_eq!(
            worktree::head(&CommandGit, &path).unwrap(),
            base,
            "and the path is a real worktree at that commit, not an empty directory"
        );
        assert!(
            path.join("a.txt").is_file(),
            "the tree is checked out, so the worker's first command can run"
        );

        std::fs::remove_dir_all(&workspace).ok();
        std::fs::remove_dir_all(worktree::worktree_root().join(&run.id)).ok();
        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    /// A refused verdict discards the dependent's branch, and the re-queued task
    /// builds on the dependency's new tip.
    ///
    /// The whole rollback, on a real repository: t2 started while t1 was still
    /// `Verifying`, so t2's branch contains t1's first commit. When t1's verdict
    /// fails, that commit is refused work — t2's branch has to go, and the tree
    /// has to go with it, because `prepare_worker` reuses a worktree that is
    /// still on disk and would hand the retry the refused commits.
    ///
    /// The second half is the point of `built_on` being a commit rather than a
    /// flag: when t1 is verified again with different work, t2's new base is
    /// t1's *new* tip, and the record says so.
    #[tokio::test]
    async fn a_failed_verdict_discards_the_dependents_branch_and_the_requeued_task_builds_on_the_new_tip(
    ) {
        use std::process::Command;

        let workspace = std::env::temp_dir().join(format!("vd-roll-{}", super::super::new_run_id()));
        std::fs::create_dir_all(&workspace).unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(&workspace)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "roll@local"]);
        git(&["config", "user.name", "roll"]);
        git(&["config", "commit.gpgsign", "false"]);
        git(&["config", "core.autocrlf", "false"]);
        std::fs::write(workspace.join("base.txt"), "base\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "base"]);
        let base = git(&["rev-parse", "HEAD"]);

        let mut run = Run::new(
            "roll",
            workspace.to_string_lossy().to_string(),
            Member::new("lead", Role::Lead, "m"),
        );
        run.integration.base = base.clone();

        // t1's first attempt: one commit on its branch.
        let t1_branch = worktree::branch_name(&run.id, "t1");
        git(&["branch", &t1_branch, &base]);
        let t1_tree = workspace.join("mk-t1");
        git(&["worktree", "add", "-q", t1_tree.to_str().unwrap(), &t1_branch]);
        std::fs::write(t1_tree.join("one.txt"), "one\n").unwrap();
        let out = Command::new("git")
            .args(["add", "-A"])
            .current_dir(&t1_tree)
            .output()
            .unwrap();
        assert!(out.status.success());
        let out = Command::new("git")
            .args(["commit", "-qm", "one"])
            .current_dir(&t1_tree)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let first_tip = git(&["rev-parse", &t1_branch]);
        git(&["worktree", "remove", "--force", t1_tree.to_str().unwrap()]);

        let mut t1 = Task::new("t1", "One", "Do it.", "node --test test/a.test.js")
            .with_files(&["one.txt"]);
        t1.state = TaskState::Verifying;
        t1.attempts = 1;
        t1.branch = Some(t1_branch.clone());
        let mut t2 = Task::new("t2", "Two", "Do it.", "node --test test/b.test.js")
            .with_files(&["two.txt"])
            .with_deps(&["t1"]);
        t2.state = TaskState::Running;
        t2.attempts = 1;
        t2.branch = Some(worktree::branch_name(&run.id, "t2"));
        run.tasks = vec![t1, t2];
        run.members = vec![
            Member::new("w-t1", Role::Worker, "m"),
            Member::new("w-t2", Role::Worker, "m"),
            Member::new("v-t2", Role::Verifier, "m"),
        ];

        // t2 starts speculatively, on t1's unverified commit.
        let prep = prepare_worker(&run, "t2").expect("t2 starts on t1's commit");
        assert_eq!(
            prep.built_on.get("t1"),
            Some(&first_tip),
            "t2 was built on t1's first attempt"
        );
        let first_base = prep.base.clone();
        {
            let record = run.task_mut("t2").unwrap();
            record.base_commit = Some(prep.base.clone());
            record.built_on = prep.built_on.clone();
        }
        let t2_path = worktree::worktree_path(&run.id, "w-t2");
        assert!(t2_path.is_dir(), "the worktree exists before the rollback");

        // t1's verdict fails.
        let transition = schedule::apply_verdict(
            run.task_mut("t1").unwrap(),
            Verdict {
                passed: false,
                reason: "the change breaks an uncovered case".into(),
                evidence: vec!["node --test -> 1 failing".into()],
                command: Some("node --test test/a.test.js".into()),
                base_exit: Some(1),
                change_exit: Some(1),
                failing_first: true,
                base_dirtied: false,
                at: super::super::now(),
            },
            3,
        );
        assert!(matches!(transition, Transition::Fail { .. }));
        let rolled = invalidate_built_on(&mut run, "t1").await;

        assert_eq!(rolled, vec!["t2".to_string()]);
        let t2_record = run.task("t2").unwrap();
        assert_eq!(t2_record.state, TaskState::Blocked, "back in the queue");
        assert!(t2_record.built_on.is_empty());
        assert_eq!(t2_record.branch, None);
        assert_eq!(t2_record.base_commit, None);
        assert_eq!(
            t2_record.attempts, 0,
            "the rollback refunds the attempt: it is not one the dependent spent"
        );
        assert!(
            !t2_path.is_dir(),
            "the worktree is gone, so the retry cannot reuse the refused commits"
        );
        assert!(
            !Command::new("git")
                .args(["rev-parse", "--verify", &worktree::branch_name(&run.id, "t2")])
                .current_dir(&workspace)
                .output()
                .unwrap()
                .status
                .success(),
            "and the branch is gone, so `worktree add -b` can make it again"
        );
        assert_eq!(
            run.member("w-t2").unwrap().state,
            MemberState::Queued,
            "the worker has no process and is waiting for a slot"
        );

        // t1's retry commits different work, and is verified.
        git(&["worktree", "add", "-q", t1_tree.to_str().unwrap(), &t1_branch]);
        std::fs::write(t1_tree.join("two.txt"), "two\n").unwrap();
        let out = Command::new("git")
            .args(["add", "-A"])
            .current_dir(&t1_tree)
            .output()
            .unwrap();
        assert!(out.status.success());
        let out = Command::new("git")
            .args(["commit", "-qm", "two"])
            .current_dir(&t1_tree)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let second_tip = git(&["rev-parse", &t1_branch]);
        git(&["worktree", "remove", "--force", t1_tree.to_str().unwrap()]);
        assert_ne!(second_tip, first_tip, "the retry is different work");
        run.task_mut("t1").unwrap().state = TaskState::Verified;

        // The re-queued task builds on the new tip, not the refused one.
        let again = prepare_worker(&run, "t2").expect("t2 starts again");
        assert_eq!(
            again.built_on.get("t1"),
            Some(&second_tip),
            "the re-queued task builds on t1's new tip"
        );
        assert_ne!(again.base, first_base, "and its base moved with it");
        assert!(
            t2_path.join("two.txt").is_file(),
            "the tree carries the work that was verified, not the work that was refused"
        );
        // The rollback cleared the record's branch, and the re-run creates the
        // branch again from the deterministic name. The prep has to say which
        // branch that is, or the record stays `None` and integration refuses the
        // task as "verified but has no branch" after the work is already done.
        assert_eq!(
            again.branch,
            worktree::branch_name(&run.id, "t2"),
            "the re-run reports the branch it is on, so the record can be restored"
        );
        assert_eq!(
            run.task("t2").unwrap().branch,
            None,
            "and the record is still empty until the caller writes it back"
        );

        std::fs::remove_dir_all(&workspace).ok();
        std::fs::remove_dir_all(worktree::worktree_root().join(&run.id)).ok();
        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    /* -------------------------------------------------------------- *
     * The skill plane
     * -------------------------------------------------------------- */

    #[test]
    fn a_draft_with_no_description_takes_one_from_its_body() {
        // The description is a label; the body is the method. A lead that writes
        // the method and forgets the label should not lose the skill over it.
        let draft = reflect::SkillDraft {
            name: "scene-graph".into(),
            description: String::new(),
            body: "# Scene graph

Parent transforms compose down the tree.".into(),
            because: "t1".into(),
        };
        let skill = draft_skill("run-x", &draft);
        assert_eq!(skill.description, "Parent transforms compose down the tree.");
        assert_eq!(skill.scope, skills::RUN_SCOPE);
        assert!(skill.path.contains("scene-graph"));
        assert!(skill.path.ends_with("SKILL.md"));
        // A run-scoped skill is not in the CLI's catalogue, so the brief must
        // name the file rather than a tool call that would fail.
        assert!(!skill.tool_loadable());
        assert!(skill.load_instruction().contains("read `"));
    }

    #[test]
    fn a_draft_that_has_a_description_keeps_it() {
        let draft = reflect::SkillDraft {
            name: "scene-graph".into(),
            description: "How transforms compose.".into(),
            body: "# Scene graph".into(),
            because: "t1".into(),
        };
        assert_eq!(draft_skill("run-x", &draft).description, "How transforms compose.");
    }

    #[test]
    fn frontmatter_is_stripped_from_a_skill_file_so_a_proposal_does_not_nest_it() {
        let file = "---
name: a
description: b
---

The body.
";
        assert_eq!(strip_frontmatter(file), "The body.
");
        // A file with no frontmatter is returned whole rather than emptied.
        assert_eq!(strip_frontmatter("Just a body."), "Just a body.");
        // An unterminated block is not a block.
        assert_eq!(strip_frontmatter("---
name: a"), "---
name: a");
    }

    fn skill_catalogue() -> Catalogue {
        Catalogue {
            skills: vec![
                Skill {
                    name: "3dviz-pro-max".into(),
                    description: "Build 3D scenes with three.js: geometry, materials, lighting."
                        .into(),
                    path: "C:/skills/3dviz-pro-max/SKILL.md".into(),
                    scope: "mastery".into(),
                    bundle: Some("3Dviz".into()),
                    enabled: true,
                    body: String::new(),
                },
                Skill {
                    name: "assay".into(),
                    description: "The expensive-path discipline for work where being wrong costs."
                        .into(),
                    path: "C:/skills/assay/SKILL.md".into(),
                    scope: "mastery".into(),
                    bundle: Some("Assay".into()),
                    enabled: true,
                    body: String::new(),
                },
            ],
        }
    }

    fn two_task_plan() -> Plan {
        Plan {
            goal: "build a 3D scene".into(),
            integration_suite: Some("npm test".into()),
            notes: String::new(),
            skills_to_write: Vec::new(),
            tasks: vec![
                Task::new("t1", "Build the scene", "geometry and lighting", "npm test")
                    .with_files(&["src/scene.js"]),
                Task::new("t2", "Rename a variable", "a tidy-up in the utility module", "npm test")
                    .with_files(&["src/util.js"]),
            ],
        }
    }

    #[test]
    fn a_task_the_lead_left_bare_is_ranked_on_its_own_text() {
        let mut plan = two_task_plan();
        let filled = settle_task_skills(&skill_catalogue(), &mut plan);
        assert_eq!(plan.tasks[0].skills, vec!["3dviz-pro-max".to_string()]);
        // The second task shares nothing with either skill, so it is given none
        // rather than the goal's — a task that inherits the goal's skills gets
        // the ones the lead already used.
        assert!(plan.tasks[1].skills.is_empty());
        assert_eq!(filled.len(), 1);
        assert!(filled[0].starts_with("t1 → "));
    }

    #[test]
    fn a_task_the_lead_named_keeps_exactly_what_it_named() {
        let mut plan = two_task_plan();
        plan.tasks[0].skills = vec!["assay".into()];
        settle_task_skills(&skill_catalogue(), &mut plan);
        assert_eq!(plan.tasks[0].skills, vec!["assay".to_string()]);
    }

    #[test]
    fn a_skill_named_twice_appears_once_in_the_brief() {
        let mut plan = two_task_plan();
        plan.tasks[0].skills = vec!["assay".into(), "assay".into()];
        settle_task_skills(&skill_catalogue(), &mut plan);
        assert_eq!(plan.tasks[0].skills, vec!["assay".to_string()]);
    }

    #[test]
    fn an_empty_catalogue_fills_nothing_and_breaks_nothing() {
        let mut plan = two_task_plan();
        let filled = settle_task_skills(&Catalogue::default(), &mut plan);
        assert!(filled.is_empty());
        assert!(plan.tasks.iter().all(|task| task.skills.is_empty()));
    }

    #[test]
    fn the_worker_brief_carries_the_mechanism_not_just_the_name() {
        let mut run = Run::new(
            "build a 3D scene",
            "C:/ws",
            Member::new("lead", Role::Lead, "m"),
        );
        run.skills = skill_catalogue().skills;
        let mut task = Task::new("t1", "Build the scene", "geometry", "npm test");
        task.skills = vec!["3dviz-pro-max".into()];

        let section = worker_skills(&run, &task);
        assert!(section.contains("3dviz-pro-max"));
        // A mastery skill is not in the CLI's catalogue and the member session has
        // no mastery MCP server, so the mechanism has to be the file.
        assert!(section.contains("C:/skills/3dviz-pro-max/SKILL.md"));
        assert!(!section.contains("skill { name"));
    }

    #[test]
    fn a_skill_the_run_does_not_hold_is_dropped_from_the_brief() {
        // Reachable by loading a run written by an older build. A brief that tells
        // a worker to load a skill that is not installed wastes its first turn.
        let run = Run::new("g", "C:/ws", Member::new("lead", Role::Lead, "m"));
        let mut task = Task::new("t1", "Build", "d", "npm test");
        task.skills = vec!["ghost".into()];
        assert!(worker_skills(&run, &task).is_empty());
    }

    #[test]
    fn the_verifier_is_told_what_the_worker_loaded() {
        let mut run = Run::new("g", "C:/ws", Member::new("lead", Role::Lead, "m"));
        run.skills = skill_catalogue().skills;
        let mut task = Task::new("t1", "Build", "d", "npm test");
        task.skills = vec!["assay".into()];
        task.owner = Some("w-t1".into());
        let mut worker = Member::new("w-t1", Role::Worker, "m");
        worker.skills_loaded = vec!["assay".into()];
        run.members.push(worker);

        let section = verifier_skills(&run, &task);
        assert!(section.contains("saw this worker load: assay"));
    }

    #[test]
    fn a_gap_is_recomputed_so_a_retry_that_loads_the_skill_closes_it() {
        let mut run = Run::new("g", "C:/ws", Member::new("lead", Role::Lead, "m"));
        let mut task = Task::new("t1", "Build", "d", "npm test");
        task.skills = vec!["assay".into()];
        task.owner = Some("w-t1".into());
        run.tasks.push(task);
        let mut worker = Member::new("w-t1", Role::Worker, "m");
        worker.skills_loaded = vec!["assay".into()];
        run.members.push(worker);

        // The member loaded it, so there is no gap.
        let loaded = run.member("w-t1").unwrap().skills_loaded.clone();
        assert!(skills::unloaded(&run.tasks[0].skills, &loaded).is_empty());
    }

    /// The criterion gate, against a real repository.
    ///
    /// The thirteenth live run spent three worker attempts and two verifier turns
    /// on `node --test test/gates.test.js`, which passed on the base commit, so no
    /// worker could ever make it distinguish the change from the base. The engine
    /// saw it at the verdict — correctly, and far too late: the lead had spent
    /// both amendments and the question fell to the user. The same fact is one
    /// command away before a worker spawns, and this is the check that runs it.
    #[test]
    fn a_criterion_that_passes_on_the_base_commit_is_refused_before_a_worker_spawns() {
        use std::process::Command;

        let workspace =
            std::env::temp_dir().join(format!("vd-crit-{}", super::super::new_run_id()));
        std::fs::create_dir_all(&workspace).unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(&workspace)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "crit@local"]);
        git(&["config", "user.name", "crit"]);
        git(&["config", "commit.gpgsign", "false"]);
        git(&["config", "core.autocrlf", "false"]);
        std::fs::write(workspace.join("present.txt"), "here\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "base"]);
        let base = git(&["rev-parse", "HEAD"]);

        let run_id = format!("crit-{}", super::super::new_run_id());
        let plan = Plan {
            goal: "goal".into(),
            tasks: vec![
                // Passes on the base, so it proves nothing.
                Task::new("t1", "One", "Do it.", "test -f present.txt").with_files(&["a.js"]),
                // Fails on the base, which is what a criterion has to do.
                Task::new("t2", "Two", "Do it.", "test -f absent.txt").with_files(&["b.js"]),
                // Prose is already a violation of its own; naming it twice reads
                // as two problems.
                Task::new("t3", "Three", "Do it.", "the tests pass, and they are green")
                    .with_files(&["c.js"]),
            ],
            integration_suite: Some("test -f present.txt".into()),
            notes: String::new(),
            skills_to_write: Vec::new(),
        };

        let violations = criteria_passing_on_base(&run_id, &workspace, &base, &plan);
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert_eq!(violations[0].key(), "criterion-passes-on-base");
        match &violations[0] {
            Violation::CriterionPassesOnBase { task, command } => {
                assert_eq!(task, "t1");
                assert_eq!(command, "test -f present.txt");
            }
            other => panic!("{other:?}"),
        }
        assert!(
            violations[0].message().contains("cannot tell the change from the base"),
            "the message has to say why, not just that: {}",
            violations[0].message()
        );

        std::fs::remove_dir_all(&workspace).ok();
        std::fs::remove_dir_all(worktree::worktree_root().join(&run_id)).ok();
    }

    /// A base the engine could not check is not a plan the engine refuses.
    ///
    /// The gate exists to catch a criterion the engine has *observed* to be
    /// unprovable. A missing base tree is the engine's own failure, and blaming
    /// the lead for it would refuse every plan on a machine where git could not
    /// make a detached worktree.
    #[test]
    fn a_criterion_gate_with_no_base_tree_refuses_nothing() {
        let plan = Plan {
            goal: "goal".into(),
            tasks: vec![
                Task::new("t1", "One", "Do it.", "test -f present.txt").with_files(&["a.js"]),
                Task::new("t2", "Two", "Do it.", "test -f absent.txt").with_files(&["b.js"]),
            ],
            integration_suite: Some("test -f present.txt".into()),
            notes: String::new(),
            skills_to_write: Vec::new(),
        };
        let workspace = std::env::temp_dir().join("vd-crit-no-base");
        assert!(criteria_passing_on_base("crit-none", &workspace, "", &plan).is_empty());
    }

    /// An entry written straight to `board.jsonl` reaches the drive loop.    ///
    /// The twelfth live run's w-t3 wrote its evidence into the run directory
    /// instead of its inbox. The file had it, the panel's file view had it, and
    /// the engine's memory did not — so the loop never handed the task to a
    /// verifier and the run died ten minutes later of "a member stopped
    /// responding". The file is the record; memory is a cache of it.
    #[test]
    fn an_entry_written_straight_to_the_file_reaches_the_board() {
        let run = planning_run("reconcile");
        let engine = engine_showing(&run, false);

        // Nothing on disk yet: the board is empty and stays empty.
        assert!(engine.board_tail().is_empty());

        let mut foreign = BoardEntry::new("w-t1", BoardKind::Evidence, "cargo test: 12 passed");
        foreign.task = Some("t1".into());
        foreign.at = "2026-11-19T01:03:41.577Z".into();
        foreign.seq = 17;
        Board::append_at(&run.dir(), &foreign).unwrap();

        let tail = engine.board_tail();
        assert_eq!(tail.len(), 1, "the file's entry is the board's entry");
        assert_eq!(tail[0].from, "w-t1");
        assert_eq!(tail[0].kind, BoardKind::Evidence);
        assert_eq!(
            tail[0].at, "2026-11-19T01:03:41.577Z",
            "the moment the member wrote it is kept"
        );
        assert_eq!(tail[0].seq, 1, "the board assigns the number, not the writer");

        // Reading again does not duplicate it.
        assert_eq!(engine.board_tail().len(), 1);

        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    /// A file in an inbox that is not an entry is named, and a reserved one is
    /// left alone.
    #[test]
    fn a_file_in_the_inbox_that_is_not_an_entry_is_reported() {
        let run = planning_run("ignored");
        let inbox = inbox_dir(&run.id, "w-t1");
        std::fs::create_dir_all(&inbox).unwrap();
        std::fs::write(inbox.join("evidence.jsonl"), "{}").unwrap();
        std::fs::write(inbox.join("evidence.done"), "{}").unwrap();
        std::fs::write(inbox.join("plan.done"), "{}").unwrap();
        std::fs::write(inbox.join("entry.json"), "{}").unwrap();
        // A file being written is not a mistake: the lead writes its plan
        // through a temp file and renames it.
        std::fs::write(inbox.join(".plan.json.miob3653.tmp"), "{}").unwrap();
        std::fs::write(inbox.join("entry.json.tmp"), "{}").unwrap();

        let ignored = ignored_inbox_files(&run, "w-t1");
        assert_eq!(
            ignored,
            vec!["evidence.done".to_string(), "evidence.jsonl".to_string()],
            "the two files that are not entries are named, the entry and the reserved marker are not"
        );

        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    #[test]
    fn a_silent_worker_whose_task_is_verified_is_not_a_wedge() {
        // The first live run died here: w-t1 was silent for 601s after its task
        // had already been verified, and the run failed for it.
        let run = run_with_silent_worker(TaskState::Verified, MEMBER_IDLE_LIMIT_SECS + 60);
        assert!(wedge(&run, &never_busy, &no_pulse).is_none(), "a finished worker is idle, not wedged");
        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    #[test]
    fn a_silent_worker_while_its_task_is_being_verified_is_not_a_wedge() {
        // The worker's part is over the moment evidence lands; the verifier owns
        // the task now.
        let run = run_with_silent_worker(TaskState::Verifying, MEMBER_IDLE_LIMIT_SECS + 60);
        assert!(wedge(&run, &never_busy, &no_pulse).is_none());
        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    /// A worker whose task was rolled back is not a wedge.
    ///
    /// A speculative start can be undone: the dependency's verdict failed, so the
    /// dependent's branch was discarded and its task went back to `Blocked`. Its
    /// worker is no longer expected to say anything — the task it was working on
    /// is not `Running` any more — and reading that silence as a dead run would
    /// fail a run that is recovering.
    #[test]
    fn a_silent_worker_whose_task_was_rolled_back_is_not_a_wedge() {
        let run = run_with_silent_worker(TaskState::Blocked, MEMBER_IDLE_LIMIT_SECS + 60);
        assert!(
            wedge(&run, &never_busy, &no_pulse).is_none(),
            "a rolled-back task has no worker to wait for"
        );
        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    #[test]
    fn a_worker_that_is_still_talking_is_not_a_wedge() {
        let run = run_with_silent_worker(TaskState::Running, 5);
        assert!(wedge(&run, &never_busy, &no_pulse).is_none(), "five seconds is not silence");
        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    #[test]
    fn a_parked_member_is_never_a_wedge_however_long_it_has_been_quiet() {
        let mut run = run_with_silent_worker(TaskState::Running, MEMBER_IDLE_LIMIT_SECS * 10);
        run.members[0].state = MemberState::Parked;
        assert!(wedge(&run, &never_busy, &no_pulse).is_none(), "a parked member is not expected to talk");
        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    #[test]
    fn a_member_mid_turn_is_working_not_wedged_however_long_it_has_been_quiet() {
        // `last_at` only moves when a member writes to the board, so a verifier
        // that spends ten minutes running a suite looks exactly like one that
        // died. The first live run failed for it: v-t1 was mid-turn and the idle
        // limit killed the run.
        let run = run_with_silent_worker(TaskState::Running, MEMBER_IDLE_LIMIT_SECS * 2);
        let in_flight = |_: &str| true;
        assert!(
            wedge(&run, &in_flight, &no_pulse).is_none(),
            "a turn in flight is work, not silence"
        );
        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    #[test]
    fn a_turn_in_flight_past_the_ceiling_is_a_hang_not_work() {
        // The bridge keeps `busy` set when a provider stops answering, so a turn
        // with no sign of life forever would hold a worker slot until the
        // wall-clock budget expired.
        let run = run_with_silent_worker(TaskState::Running, MEMBER_TURN_CEILING_SECS + 60);
        let in_flight = |_: &str| true;
        let wedged = wedge(&run, &in_flight, &no_pulse).expect("a turn with no sign of life is a hang");
        assert_eq!(wedged.state, RunState::Failed);
        assert!(
            wedged.members[0]
                .error
                .as_deref()
                .unwrap()
                .contains("in flight"),
            "the failure names the hang, not silence"
        );
        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    /// A model call that has said nothing since its turn began is not busy.
    ///
    /// The twenty-first live run is the shape: two workers claimed their tasks,
    /// emitted no notification at all for twenty-five minutes, and the run could
    /// not tell that from work because `last_at` only moves on a board write. The
    /// proxy was returning 400s the whole time. The stream is the evidence the
    /// board never had, and five minutes of it is enough.
    #[test]
    fn a_model_call_that_has_said_nothing_fails_fast() {
        let run = run_with_silent_worker(TaskState::Running, 0);
        let in_flight = |_: &str| true;
        let wedged = wedge(&run, &in_flight, &quiet_for(MODEL_SILENCE_SECS + 1))
            .expect("a model call that has not answered is a wedge");
        assert_eq!(wedged.state, RunState::Failed);
        let why = wedged.members[0].error.as_deref().unwrap();
        assert!(
            why.contains("has not answered"),
            "the failure says the model did not answer, not that the member was quiet: {why}"
        );
        assert!(
            why.starts_with("m has not answered"),
            "the failure names the model that did not answer: {why}"
        );
        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    /// Silence with a tool running is the tool's silence.
    ///
    /// A worker eight minutes into a test suite emits nothing and is working
    /// perfectly. Failing it would punish the swarm for doing the thing it was
    /// asked to do, which is the failure mode the ceiling was written to avoid in
    /// the first place.
    #[test]
    fn a_quiet_member_with_a_tool_running_is_working() {
        let run = run_with_silent_worker(TaskState::Running, 0);
        let in_flight = |_: &str| true;
        assert!(
            wedge(&run, &in_flight, &busy_tool(MODEL_SILENCE_SECS * 4)).is_none(),
            "a tool in flight is work, not a dead model call"
        );
        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    /// A turn that is still talking is not silent, however long it has been going.
    #[test]
    fn a_turn_that_is_still_talking_is_not_a_wedge() {
        let run = run_with_silent_worker(TaskState::Running, 0);
        let in_flight = |_: &str| true;
        assert!(
            wedge(&run, &in_flight, &quiet_for(MODEL_SILENCE_SECS - 1)).is_none(),
            "under the silence limit is a slow answer, not a dead one"
        );
        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    /// A member with no watch falls back to the ceiling, exactly as before.
    ///
    /// The pulse is an improvement, not a replacement: a member whose stream
    /// cannot be read must still be caught by the clock, or the fix would trade a
    /// hang for a run that never notices one.
    #[test]
    fn a_member_with_no_pulse_reading_falls_back_to_the_ceiling() {
        let run = run_with_silent_worker(TaskState::Running, MEMBER_TURN_CEILING_SECS - 60);
        let in_flight = |_: &str| true;
        assert!(
            wedge(&run, &in_flight, &no_pulse).is_none(),
            "no reading is not silence"
        );
        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    #[test]
    fn a_silent_lead_is_a_wedge_because_the_run_is_waiting_on_it() {        let mut run = planning_run("silent lead");
        run.lead.state = MemberState::Working;
        run.lead.last_at = Some(
            (chrono::Utc::now() - chrono::Duration::seconds(MEMBER_IDLE_LIMIT_SECS + 60))
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        );
        let wedged = wedge(&run, &never_busy, &no_pulse).expect("the lead has no task, so it is always expected");
        assert_eq!(wedged.lead.state, MemberState::Failed);
        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    #[test]
    fn an_orphan_is_interrupted_once_and_its_members_are_lost() {
        let mut run = planning_run("orphan");
        run.state = RunState::Running;
        run.lead.state = MemberState::Working;
        run.save().unwrap();
        let mut board = Board::new();

        let orphan = interrupt_orphan(run.clone(), &mut board);
        assert_eq!(orphan.state, RunState::Interrupted);
        assert_eq!(orphan.lead.state, MemberState::Lost);
        assert!(orphan.failure.is_some(), "the run says why it stopped");
        assert_eq!(board.len(), 1, "the board records the interruption");
        assert_eq!(board.entries()[0].kind, BoardKind::Escalation);

        // Written, not just returned: the next process reads it as interrupted.
        assert_eq!(Run::load(&run.id).unwrap().state, RunState::Interrupted);

        // And the entry is on disk, so a second read does not stack another.
        let reloaded = Board::load_at(&run.dir());
        assert_eq!(reloaded.len(), 1);

        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    #[test]
    fn an_adopted_run_is_reported_as_external() {
        let run = planning_run("adopted");
        let _driver = another_driver(&run.dir());
        let snapshot = engine_showing(&run, true).snapshot(None, None);
        assert!(snapshot.external, "an adopted run is not driven here");
        assert!(snapshot.live);
        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    #[test]
    fn an_owned_run_is_not_reported_as_external() {
        let run = planning_run("owned");
        let snapshot = engine_showing(&run, false).snapshot(None, None);
        assert!(!snapshot.external, "a run this process drives is not external");
        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    /* -------------------------------------------------------------- *
     * The feed, as the snapshot scopes it
     * -------------------------------------------------------------- */

    /// A run with `workers` workers on it, saved so a snapshot can read it.
    fn crowded_run(goal: &str, workers: usize) -> Run {
        let mut run = Run::new(goal, "/tmp", Member::new("lead", Role::Lead, "m"));
        for index in 1..=workers {
            run.members
                .push(Member::new(format!("w-{index}"), Role::Worker, "m"));
        }
        run.save().unwrap();
        run
    }

    /// A watch whose feed holds `lines` tool calls.
    fn busy_watch(lines: usize) -> Arc<super::super::watch::MemberWatch> {
        let watch = Arc::new(super::super::watch::MemberWatch::new(Arc::new(
            crate::acp::bridge::RecordingSink::default(),
        )));
        for index in 0..lines {
            watch.emit(
                "acp://notification/swarm/x",
                json!({
                    "method": "session/update",
                    "params": { "update": {
                        "sessionUpdate": "tool_call",
                        "toolCallId": format!("c{index}"),
                        "title": "bash",
                    }}
                }),
            );
        }
        watch
    }

    /// A watch whose feed holds `lines` tool calls and `readings` usage readings.
    ///
    /// The readings are what the rate series is sampled from, so a watch built
    /// without them has an empty series — which is the honest state of a member
    /// between turns, and the wrong state for a test about payload size.
    fn busy_watch_with_usage(lines: usize, readings: usize) -> Arc<super::super::watch::MemberWatch> {
        let watch = busy_watch(lines);
        for index in 0..readings {
            watch.emit(
                "acp://usage/swarm/x",
                json!({ "input": 1_000 * (index + 1), "output": 10, "cached": 0 }),
            );
        }
        watch
    }

    /// Every member's ring is full and one member is selected.
    fn crowded_engine(run: &Run, lines: usize) -> Engine {
        let engine = engine_showing(run, false);
        {
            let mut inner = engine.inner.lock().unwrap();
            for member in run.roster() {
                inner.watches.insert(member.id.clone(), busy_watch(lines));
            }
        }
        engine
    }

    /// The same, with every member's rate series full as well.
    fn crowded_engine_with_usage(run: &Run, lines: usize, readings: usize) -> Engine {
        let engine = engine_showing(run, false);
        {
            let mut inner = engine.inner.lock().unwrap();
            for member in run.roster() {
                inner
                    .watches
                    .insert(member.id.clone(), busy_watch_with_usage(lines, readings));
            }
        }
        engine
    }

    #[test]
    fn the_snapshot_carries_a_feed_for_the_selected_member_alone() {
        // The bound the phase depends on. `current` goes to everyone, because the
        // roster's question is "what is each of them doing"; `recent` goes to the
        // selected member, because the panel polls every two seconds and a
        // twenty-one member run would otherwise carry four thousand items per
        // poll for a pane that can only show one.
        let run = crowded_run("scoped", 20);
        let engine = crowded_engine(&run, 5);

        let snapshot = engine.snapshot(None, Some("w-7"));
        let shown = snapshot.run.expect("the run is shown");
        for member in shown.roster() {
            assert!(
                member.activity.current.is_some(),
                "{} has no current tool",
                member.id
            );
            if member.id == "w-7" {
                assert_eq!(member.activity.recent.len(), 5, "the selected member's feed");
            } else {
                assert!(
                    member.activity.recent.is_empty(),
                    "{} carried a feed it was not asked for",
                    member.id
                );
            }
        }

        // Reading the snapshot must not write the feed to disk. The rings are
        // live data, and a saved one would be a snapshot of a moment presented as
        // history — and would grow `run.json` by two hundred items per member on
        // every save the drive loop makes.
        let saved = Run::load(&run.id).expect("the run is still on disk");
        assert!(
            saved.roster().all(|m| m.activity.is_empty()),
            "the snapshot wrote the feed into the record"
        );
        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    #[test]
    fn nothing_selected_means_no_feed_at_all() {
        // A panel with no member selected is showing the board, not a member. The
        // rings are still there; nothing asks for them.
        let run = crowded_run("unscoped", 3);
        let engine = crowded_engine(&run, 5);
        let snapshot = engine.snapshot(None, None);
        let shown = snapshot.run.expect("the run is shown");
        assert!(shown.roster().all(|m| m.activity.recent.is_empty()));
        assert!(shown.roster().all(|m| m.activity.current.is_some()));
        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    #[test]
    fn a_twenty_one_member_snapshot_stays_under_a_measured_bound() {
        // The proof the phase asks for, measured rather than asserted. Every ring
        // is full — two hundred lines each, four thousand two hundred in total —
        // and one member is selected. The payload must carry one feed.
        //
        // The rate series is the other half of the same bound, and it is the half
        // that grows with the roster: `recent` is one member's, `series` is
        // everyone's. Forty readings on twenty-one members is the worst case it
        // can reach — every member in a turn at once, which the concurrency
        // ceiling makes impossible in a real run and therefore the right thing to
        // bound.
        let run = crowded_run("bounded", 20);
        let engine = crowded_engine_with_usage(&run, 200, super::super::RATE_KEPT);

        let snapshot = engine.snapshot(None, Some("w-3"));
        let shown = snapshot.run.as_ref().expect("the run is shown");
        let items: usize = shown.roster().map(|m| m.activity.recent.len()).sum();
        assert_eq!(items, 200, "one feed, not twenty-one");
        for member in shown.roster() {
            assert_eq!(
                member.activity.series.len(),
                super::super::RATE_KEPT,
                "{} has no rate series to draw a sparkline from",
                member.id
            );
        }

        let payload = serde_json::to_string(&snapshot).expect("the snapshot serialises");
        // Measured on this machine: 35,671 bytes before the rate series, of which
        // the feed is about 30 KB; 76,978 bytes with a full series on every
        // member, which is the 41 KB the series costs at its worst. The same run
        // with every feed attached is 489,451 bytes — 4200 items, every two
        // seconds, for a pane that can show one member. The bound is set above
        // the measurement so a field added to the run document does not fail this
        // test, and well below the unscoped figure so a regression that stopped
        // scoping would.
        assert!(
            payload.len() < 96 * 1024,
            "a 21-member snapshot with one feed and every series is {} bytes",
            payload.len()
        );
        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    /// The adopted copy follows the writer instead of freezing at adoption.
    ///
    /// This is the difference between a viewer and a screenshot: the process
    /// driving the run is the only writer, so every read has to go back to disk.
    #[test]
    fn an_adopted_run_is_re_read_so_it_does_not_freeze() {
        let run = planning_run("follow me");
        let _driver = another_driver(&run.dir());
        let engine = engine_showing(&run, true);
        assert_eq!(engine.snapshot(None, None).run.unwrap().state, RunState::Planning);

        // The other process moves the run on.
        let mut moved = Run::load(&run.id).unwrap();
        moved.state = RunState::Running;
        moved.save().unwrap();

        let second = engine.snapshot(None, None).run.expect("still adopted");
        assert_eq!(second.id, run.id);
        assert_eq!(second.state, RunState::Running, "the panel saw a stale copy");

        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    /// A driver that dies while the panel is watching is noticed.
    ///
    /// The adopted branch used to re-read the run and never re-check the lock, so
    /// a run adopted once stayed live on screen forever — the phantom run again,
    /// one layer down.
    #[test]
    fn an_adopted_run_whose_driver_dies_is_interrupted() {
        let run = planning_run("dies mid-watch");
        let driver = another_driver(&run.dir());
        let engine = engine_showing(&run, true);
        assert!(engine.snapshot(None, None).external, "adopted while the driver lives");

        drop(driver);

        let after = engine.snapshot(None, None);
        assert_eq!(
            after.run.as_ref().map(|r| r.state),
            Some(RunState::Interrupted),
            "the panel kept showing a run nobody was driving"
        );
        assert!(!after.external, "nobody is driving it now");
        assert!(!after.live);

        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    /// A plan shaped the way a lead actually writes one is readable.
    ///
    /// The second half of the same live bug: `Task` required the engine's own
    /// runtime fields (`state`, `owner`, `attempts`, …), so every plan a real lead
    /// wrote failed to parse and `read_plan` returned `None` without a word. The
    /// fixture here is a plan with only the fields a lead can know.
    #[test]
    fn a_plan_with_only_the_fields_a_lead_knows_is_readable() {
        let run = Run::new("plan", "/tmp", Member::new("lead", Role::Lead, "m"));
        let dir = inbox_dir(&run.id, "lead");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("plan.json"),
            r#"{
              "goal": "a playable game",
              "integrationSuite": "node --test test/",
              "notes": "two modules, no DOM",
              "tasks": [
                {
                  "id": "t1",
                  "title": "input",
                  "detail": "map WASD",
                  "files": ["src/input.js"],
                  "deps": [],
                  "acceptance": "node --test test/input.test.js exits 0",
                  "priority": "high"
                },
                {
                  "id": "t2",
                  "title": "coins",
                  "detail": "scatter coins",
                  "files": ["src/coins.js"],
                  "deps": ["t1"],
                  "acceptance": "node --test test/coins.test.js exits 0"
                }
              ]
            }"#,
        )
        .unwrap();

        let plan = read_plan(&run).expect("a lead-shaped plan parses");
        assert_eq!(plan.tasks.len(), 2);
        assert_eq!(plan.tasks[0].id, "t1");
        assert_eq!(plan.tasks[0].state, TaskState::Blocked, "a new task has not started");
        assert_eq!(plan.tasks[1].deps, vec!["t1".to_string()]);
        assert_eq!(plan.integration_suite.as_deref(), Some("node --test test/"));
        // Consumed, so a rejected plan does not re-fire every tick.
        assert!(!dir.join("plan.json").exists());
        assert!(dir.join("plan.done").exists());

        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    /// An adopted run whose directory disappears is forgotten, not shown forever.
    #[test]
    fn an_adopted_run_that_is_deleted_elsewhere_stops_being_shown() {
        let run = planning_run("deleted elsewhere");
        let _driver = another_driver(&run.dir());
        let engine = engine_showing(&run, true);
        assert!(engine.snapshot(None, None).run.is_some());

        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();

        let after = engine.snapshot(None, None);
        assert!(after.run.is_none(), "a deleted run is not a ghost");
        assert!(!after.external);
    }

    /// An accepted plan carries its integration suite onto the run.
    ///
    /// The panel rebuilds the plan from the run when the user approves, so a
    /// suite that is dropped here is a plan the gate refuses a second time — which
    /// is exactly what happened: the lead's plan was accepted, and then approval
    /// failed with "no integration suite".
    #[test]
    fn an_accepted_plan_carries_its_suite_onto_the_run() {
        let run = Run::new("g", "/tmp", Member::new("lead", Role::Lead, "m"));
        let mut t1 = Task::new("t1", "a", "", "x");
        t1.files = vec!["src/a.js".into()];
        let mut t2 = Task::new("t2", "b", "", "y");
        t2.files = vec!["src/b.js".into()];
        t2.deps = vec!["t1".into()];
        let plan = Plan {
            goal: "g".into(),
            tasks: vec![t1, t2],
            integration_suite: Some("node --test test/".into()),
            notes: String::new(),
            skills_to_write: Vec::new(),
        };

        let accepted = accept_plan(&run, &plan);
        assert_eq!(accepted.state, RunState::AwaitingApproval);
        assert_eq!(accepted.tasks.len(), 2);
        assert_eq!(
            accepted.integration.suite.as_deref(),
            Some("node --test test/"),
            "the suite has to survive to the approval gate"
        );
        assert_eq!(accepted.lead.state, MemberState::Parked);

        // And the plan the panel would rebuild from the run passes the gate.
        let rebuilt = Plan {
            goal: accepted.goal.clone(),
            tasks: accepted.tasks.clone(),
            integration_suite: accepted.integration.suite.clone(),
            notes: String::new(),
            skills_to_write: Vec::new(),
        };
        assert!(
            plan::validate(&rebuilt).is_ok(),
            "the rebuilt plan was refused: {:?}",
            plan::validate(&rebuilt).err()
        );
    }

    /// Approving keeps the integration suite.
    ///
    /// The third live bug in the same seam: `Integration::new` builds a fresh
    /// record, so assigning it over the old one dropped the suite and the run had
    /// no final gate.
    #[test]
    fn approving_keeps_the_integration_suite() {
        let run = Run::new("g", "/tmp", Member::new("lead", Role::Lead, "m"));
        let mut t1 = Task::new("t1", "a", "", "x");
        t1.files = vec!["src/a.js".into()];
        let plan = Plan {
            goal: "g".into(),
            tasks: vec![t1],
            integration_suite: Some("node --test test/".into()),
            notes: String::new(),
            skills_to_write: Vec::new(),
        };

        let running = start_running(&run, &plan, "abc123", 3, None);
        assert_eq!(running.state, RunState::Running);
        assert_eq!(running.concurrency, 3);
        assert_eq!(
            running.integration.suite.as_deref(),
            Some("node --test test/"),
            "the suite was dropped at approval"
        );
        assert_eq!(running.integration.base, "abc123");
        assert_eq!(running.tasks[0].state, TaskState::Blocked);
        assert!(running.tasks[0].branch.is_some(), "a task gets its branch");
    }

    /// A five-deep chain, which is the shape the observed run had.
    fn five_deep_plan() -> Plan {
        let mut tasks = Vec::new();
        for index in 0..5 {
            let id = format!("t{}", index + 1);
            let file = format!("src/{id}.rs");
            let deps: Vec<String> = if index == 0 {
                Vec::new()
            } else {
                vec![format!("t{index}")]
            };
            let dep_refs: Vec<&str> = deps.iter().map(String::as_str).collect();
            tasks.push(
                Task::new(&id, "a", "", "cargo test")
                    .with_files(&[file.as_str()])
                    .with_deps(&dep_refs),
            );
        }
        Plan {
            goal: "g".into(),
            tasks,
            integration_suite: Some("cargo test".into()),
            notes: String::new(),
            skills_to_write: Vec::new(),
        }
    }

    /// One foundation task, then three that all depend on it: a first rank of
    /// one and a widest rank of three. The shape a revision can fix, and the
    /// shape a plan has when only its first rank is serial.
    fn foundation_then_three() -> Plan {
        let mut tasks = vec![
            Task::new("t1", "a", "", "cargo test").with_files(&["src/one.rs"]),
        ];
        for id in ["t2", "t3", "t4"] {
            let file = format!("src/{id}.rs");
            tasks.push(
                Task::new(id, "a", "", "cargo test")
                    .with_files(&[file.as_str()])
                    .with_deps(&["t1"]),
            );
        }
        Plan {
            goal: "g".into(),
            tasks,
            integration_suite: Some("cargo test".into()),
            notes: String::new(),
            skills_to_write: Vec::new(),
        }
    }

    /// The budget follows the plan instead of a constant.
    ///
    /// The observed run was a five-deep chain given a flat hour, so its own
    /// projection was longer than its budget and the engine would have failed it
    /// mid-task with every worktree discarded.
    #[test]
    fn approving_derives_the_wall_clock_from_the_plan() {
        let run = Run::new("g", "/tmp", Member::new("lead", Role::Lead, "m"));
        assert_eq!(run.budget.max_wall_secs, 3600, "the flat hour it starts with");

        let running = start_running(&run, &five_deep_plan(), "abc123", 4, None);
        assert_eq!(
            running.budget.max_wall_secs, 6000,
            "five steps at 600s each, doubled for retries"
        );
        assert!(
            running.budget.max_wall_secs > running.budget.elapsed_secs(),
            "a budget the run has already spent is not a budget"
        );
    }

    /// A short plan keeps the hour it always had.
    #[test]
    fn approving_a_short_plan_keeps_the_flat_hour() {
        let run = Run::new("g", "/tmp", Member::new("lead", Role::Lead, "m"));
        let plan = roles::example_plan();
        let running = start_running(&run, &plan, "abc123", 2, None);
        assert_eq!(running.budget.max_wall_secs, 3600);
    }

    /// The review shows the projection and the budget side by side.
    #[test]
    fn the_rehearsal_view_shows_the_projection_and_the_budget() {
        let engine = Engine::default();
        let view = engine.rehearse(&five_deep_plan(), 4);
        assert_eq!(view.rehearsal.parallel_steps, 5);
        assert_eq!(view.rehearsal.projected_wall_secs, 3000);
        assert_eq!(view.rehearsal.budget_wall_secs, 6000);
        // The same number the run will be given, so the review cannot promise a
        // budget the approval does not set.
        let run = Run::new("g", "/tmp", Member::new("lead", Role::Lead, "m"));
        let running = start_running(&run, &five_deep_plan(), "abc123", 4, None);
        assert_eq!(running.budget.max_wall_secs, view.rehearsal.budget_wall_secs);
    }

    /// The shape is a warning the plan is accepted with, not a refusal.
    #[test]
    fn a_one_wide_plan_is_accepted_with_a_shape_warning() {
        let run = Run::new("g", "/tmp", Member::new("lead", Role::Lead, "m"));
        let plan = five_deep_plan();
        assert!(
            plan::validate(&plan).is_ok(),
            "a chain is a valid plan: {:?}",
            plan::validate(&plan).err()
        );

        let accepted = accept_plan(&run, &plan);
        assert_eq!(accepted.state, RunState::AwaitingApproval);
        assert!(
            accepted
                .shape_warnings
                .iter()
                .any(|w| matches!(w, plan::ShapeWarning::OneWideFirstRank { task } if task == "t1")),
            "{:?}",
            accepted.shape_warnings
        );
        assert!(
            accepted
                .shape_warnings
                .iter()
                .any(|w| matches!(w, plan::ShapeWarning::NotWorthIt { .. })),
            "a five-deep chain is 1.0×: {:?}",
            accepted.shape_warnings
        );

        // And approving it anyway carries the warning onto the running run.
        let running = start_running(&accepted, &plan, "abc123", 4, None);
        assert_eq!(running.state, RunState::Running);
        assert_eq!(running.shape_warnings, accepted.shape_warnings);
    }

    /// A one-wide first rank is sent back to the lead once; the retry is
    /// accepted whatever its shape, and a wide first rank is accepted
    /// immediately. Run 3's board carried the warning as a note and accepted
    /// the plan anyway.
    ///
    /// The plan here is one foundation task followed by three tasks that all
    /// depend on it: a first rank of one, a widest rank of three. That is the
    /// shape the revision can fix. A pure chain has no wider rank to revise
    /// towards, and is covered by `a_chain_is_not_sent_back`.
    #[test]
    fn a_one_wide_first_rank_is_a_one_shot_send_back() {
        let one_wide = foundation_then_three();
        let reason = shape_send_back(0, &one_wide).expect("the first attempt is sent back");
        assert!(
            reason.contains("only t1 can start"),
            "the reason names the shape: {reason}"
        );
        assert!(
            shape_send_back(1, &one_wide).is_none(),
            "the retry is accepted whatever its shape"
        );

        let mut wide = five_deep_plan();
        wide.tasks.truncate(3);
        for task in &mut wide.tasks {
            task.deps.clear();
        }
        assert!(
            !plan::shape_warnings(&wide)
                .iter()
                .any(|w| matches!(w, plan::ShapeWarning::OneWideFirstRank { .. })),
            "a three-wide first rank produces no one-wide warning"
        );
        assert!(
            shape_send_back(0, &wide).is_none(),
            "a three-wide first rank is accepted immediately"
        );
    }

    /// The engine's own stop is recorded, and a new process makes it stale. It has
    /// to be set by the removal itself: the roster is updated after the stop.
    #[test]
    fn the_engines_own_stop_is_recorded_and_a_new_bridge_clears_it() {
        let mut inner = Inner {
            run: None,
            board: Board::new(),
            bridges: BTreeMap::new(),
            usage: BTreeMap::new(),
            watches: BTreeMap::new(),
            cancel: Arc::new(AtomicBool::new(false)),
            driving: false,
            adopted: false,
            driver_lock: None,
            board_bytes: 0,
            stopped: BTreeSet::new(),
            ended: None,
        };
        let bridge = Arc::new(AcpBridge::with_sink(Arc::new(
            crate::acp::bridge::NullSink,
        )));
        inner.put_bridge("v-t1", bridge.clone());
        assert!(
            !inner.stopped_deliberately("v-t1"),
            "a live process is not a stopped one"
        );

        assert!(inner.take_bridge("v-t1").is_some());
        assert!(
            inner.stopped_deliberately("v-t1"),
            "the engine took the process away, so the close is the engine's"
        );
        assert!(inner.take_bridge("v-t1").is_none(), "the bridge is gone once");

        inner.put_bridge("v-t1", bridge);
        assert!(
            !inner.stopped_deliberately("v-t1"),
            "a fresh process is not covered by the earlier stop"
        );
    }

    /// A pure chain is never sent back: the dependency graph is the plan, so
    /// "widen the first rank" is not an instruction it can obey.
    #[test]
    fn a_chain_is_not_sent_back() {
        let chain = five_deep_plan();
        assert_eq!(plan::shape(&chain).max_parallel, 1);
        assert!(
            shape_send_back(0, &chain).is_none(),
            "a chain has no wider rank to revise towards"
        );
        // The warning is still recorded, so the user still sees the shape; only
        // the revision is withheld.
        assert!(
            plan::shape_warnings(&chain)
                .iter()
                .any(|w| matches!(w, plan::ShapeWarning::OneWideFirstRank { .. })),
            "the shape is still reported as a warning"
        );
    }

    /// A run.json written before the field existed still loads.
    #[test]
    fn a_run_without_shape_warnings_still_loads() {
        let run = Run::new("g", "/tmp", Member::new("lead", Role::Lead, "m"));
        let mut body = serde_json::to_value(&run).unwrap();
        body.as_object_mut().unwrap().remove("shapeWarnings");
        let back: Run = serde_json::from_value(body).unwrap();
        assert!(back.shape_warnings.is_empty());
    }

    /// A revised plan replaces the first one.
    ///
    /// `rename` on Windows refuses to replace an existing file, so the second
    /// plan of a run could not be set aside — and a plan that cannot be set aside
    /// is re-read on every tick, forever.
    #[test]
    fn a_second_plan_replaces_the_first() {
        let run = Run::new("revise", "/tmp", Member::new("lead", Role::Lead, "m"));
        let dir = inbox_dir(&run.id, "lead");
        std::fs::create_dir_all(&dir).unwrap();

        let plan = |goal: &str| {
            format!(
                r#"{{"goal":"{goal}","integrationSuite":"node --test","notes":"","tasks":[
                    {{"id":"t1","title":"a","detail":"","files":["a"],"deps":[],"acceptance":"x"}},
                    {{"id":"t2","title":"b","detail":"","files":["b"],"deps":["t1"],"acceptance":"y"}}
                ]}}"#
            )
        };

        std::fs::write(dir.join("plan.json"), plan("first")).unwrap();
        assert_eq!(read_plan(&run).unwrap().goal, "first");

        std::fs::write(dir.join("plan.json"), plan("second")).unwrap();
        assert_eq!(
            read_plan(&run).unwrap().goal,
            "second",
            "the revised plan was not readable"
        );

        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    /// The revision brief carries every reason, because a lead cannot fix a rule
    /// it was not told it broke.
    #[test]
    fn a_revision_brief_names_every_reason_and_the_file_to_rewrite() {
        let brief = roles::revision_brief(
            "no integration suite — every task can pass and the result still be broken\n  - t1 and t2 both claim src/a.js",
            "C:/runs/r1/inbox/lead",
        );
        assert!(brief.contains("no integration suite"));
        assert!(brief.contains("both claim src/a.js"));
        assert!(brief.contains("C:/runs/r1/inbox/lead/plan.json"));
        assert!(brief.contains("integrationSuite"), "the rules are restated");
    }

    /// A verifier that ended its turn without a verdict gets one short reminder,
    /// not a second brief.
    ///
    /// A live run found this: v-t1's turn ended with nothing written, the task sat
    /// in `Verifying` for 601s, and the idle limit failed the whole run — after a
    /// worker had already produced verified work.
    #[test]
    fn a_verifier_reminder_names_the_task_the_inbox_and_the_required_fields() {
        let mut task = Task::new("t3", "scatter coins", "detail", "criterion");
        task.verifier = Some("v-t3".into());
        let brief = roles::verifier_reminder_brief(&task, "C:/runs/r1/inbox/v-t3");

        assert!(brief.contains("C:/runs/r1/inbox/v-t3"), "the inbox is named");
        assert!(brief.contains("`t3`"), "the task is named");
        assert!(brief.contains("\"kind\": \"verdict\""), "the shape is given");
        assert!(brief.contains("\"command\""), "the command is required");
        assert!(brief.contains("\"evidence\""), "the evidence is required");
        // A reminder, not a brief: it must not re-explain the whole protocol.
        assert!(
            brief.len() < 900,
            "a reminder that recites the brief spends the context it saves: {}",
            brief.len()
        );
    }

    /// The lead's plan is not a board entry, and draining the inbox must not eat
    /// it.
    ///
    /// A live run found this: the drive loop drains every inbox before it reads
    /// the plan, so `plan.json` was rejected as a malformed entry and deleted,
    /// and the run sat in `planning` until its timeout.
    #[test]
    fn draining_the_lead_inbox_leaves_the_plan_alone() {
        let run = Run::new("plan", "/tmp", Member::new("lead", Role::Lead, "m"));
        let dir = inbox_dir(&run.id, "lead");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("plan.json"),
            r#"{"goal":"g","tasks":[],"integrationSuite":null,"notes":""}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("note.json"),
            r#"{"kind":"note","text":"the floor is a plane"}"#,
        )
        .unwrap();

        let entries = drain_inbox(&run, "lead");
        assert_eq!(entries.len(), 1, "only the note is a board entry");
        assert_eq!(entries[0].text, "the floor is a plane");
        assert!(dir.join("plan.json").exists(), "the plan is still there");
        assert!(!dir.join("note.json").exists(), "the note was consumed");

        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    /// A viewer cannot stop or approve what it does not drive.
    ///
    /// Both used to be silent lies: `stop` set a cancel flag no loop was reading
    /// and wrote `Cancelled` over a run the real driver was about to overwrite.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_adopted_run_refuses_to_be_stopped_or_approved() {
        let run = planning_run("hands off");
        let engine = engine_showing(&run, true);
        let sink: Arc<dyn EventSink> = Arc::new(crate::acp::bridge::NullSink);

        let stopped = engine.stop(&sink).unwrap_err();
        assert!(stopped.contains("another process"), "{stopped}");

        let models = RoleModels {
            lead: "m".into(),
            worker: "m".into(),
            verifier: "m".into(),
            diverse: false,
            thinking: roles::ThinkingByRole::default(),
        };
        let approved = engine
            .approve(sink, roles::example_plan(), models, 2)
            .await
            .unwrap_err();
        assert!(approved.contains("another process"), "{approved}");

        // And the refusal did not write over the driver's state.
        assert_eq!(Run::load(&run.id).unwrap().state, RunState::Planning);

        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    /// A viewer asks the driver to stop, and says only that it asked.
    ///
    /// The two processes share the run directory and nothing else, so the request
    /// is a file. This is the adopter's half: it writes the request and does not
    /// touch the run's state, because the state is the driver's to write.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_adopted_run_takes_a_stop_request_instead_of_a_stop() {
        let run = planning_run("ask nicely");
        let engine = engine_showing(&run, true);
        let sink: Arc<dyn EventSink> = Arc::new(crate::acp::bridge::NullSink);

        assert!(!stop_requested(&run.dir()), "nothing asked yet");

        let asked = engine.request_stop(&sink).unwrap();
        assert_eq!(asked.state, RunState::Planning, "a request is not a stop");
        assert!(stop_requested(&run.dir()), "the request is on disk");
        assert_eq!(
            Run::load(&run.id).unwrap().state,
            RunState::Planning,
            "the adopter did not write over the driver's state"
        );

        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    /// A process that drives the run stops it directly; asking itself is a no-op.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_run_driven_here_refuses_a_stop_request() {
        let run = planning_run("mine");
        let engine = engine_showing(&run, false);
        let sink: Arc<dyn EventSink> = Arc::new(crate::acp::bridge::NullSink);

        let refused = engine.request_stop(&sink).unwrap_err();
        assert!(refused.contains("driven here"), "{refused}");
        assert!(!stop_requested(&run.dir()), "a refusal writes nothing");

        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    /// The driver's half: the tick sees the request and cancels the run.
    ///
    /// This is the mechanism the phase needs. Without it the panel would offer a
    /// button that reports success and stops nothing, which is the button the
    /// phase exists to remove.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_driver_honours_a_stop_request_on_its_next_tick() {
        let mut run = planning_run("stop me");
        run.state = RunState::Running;
        run.members.push(Member::new("w-t1", Role::Worker, "m"));
        run.members[0].state = MemberState::Working;
        run.save().unwrap();

        let engine = engine_showing(&run, false);
        let sink: Arc<dyn EventSink> = Arc::new(crate::acp::bridge::NullSink);

        // Nothing asked: the tick does nothing.
        assert!(engine.honour_stop_request(&run.dir(), &sink).is_none());

        request_stop_at(&run.dir()).unwrap();
        let stopped = engine
            .honour_stop_request(&run.dir(), &sink)
            .expect("the tick acted");

        assert_eq!(stopped.state, RunState::Cancelled);
        assert_eq!(stopped.members[0].state, MemberState::Parked);
        assert_eq!(
            Run::load(&run.id).unwrap().state,
            RunState::Cancelled,
            "the stop is on disk, not only in memory"
        );
        assert!(
            !stop_requested(&run.dir()),
            "the request is consumed, so its presence always means one is outstanding"
        );

        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    /// A request left behind by an earlier run cannot cancel a later one.
    ///
    /// The directory is per-run, so this should never fire — which is exactly why
    /// it is cheaper to make true than to assume.
    #[test]
    fn a_stale_stop_request_is_cleared_when_a_run_starts() {
        let run = planning_run("fresh");
        request_stop_at(&run.dir()).unwrap();
        assert!(stop_requested(&run.dir()));

        // What `start` does to the directory it is about to drive.
        let _ = std::fs::remove_file(run.dir().join(STOP_REQUEST));
        assert!(!stop_requested(&run.dir()));

        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    /// Whatever the panel is shown as live, it can be driven from somewhere.
    ///
    /// The invariant that matters, and the one that holds no matter which run on
    /// disk wins: a run shown as live is always external (somebody is driving it),
    /// and a run shown as not-live is never external (nobody is).
    #[test]
    fn a_run_shown_as_live_is_always_driven_somewhere() {
        let snapshot = Engine::default().snapshot(None, None);
        if let Some(run) = snapshot.run.as_ref() {
            assert_eq!(
                snapshot.external,
                run.state.is_live(),
                "{} is {} but external is {}",
                run.id,
                run.state.label(),
                snapshot.external
            );
        }
    }

    /// Adoption only ever picks a live run; a finished one is history.
    #[test]
    fn a_finished_run_is_never_adopted_as_the_live_run() {
        let mut run = Run::new("done already", "/tmp", Member::new("lead", Role::Lead, "m"));
        run.state = RunState::Done;
        run.save().unwrap();

        let snapshot = Engine::default().snapshot(None, None);
        // A finished run is never the run in progress. (An *interrupted* run can
        // be shown, because that is how the panel says a run died mid-flight.)
        assert_ne!(
            snapshot.run.as_ref().map(|r| r.id.as_str()),
            Some(run.id.as_str()),
            "a finished run was adopted as the live run"
        );
        assert!(
            snapshot.history.iter().any(|h| h.id == run.id),
            "a finished run belongs in history"
        );

        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    #[test]
    fn a_verdict_kind_parses_and_an_unknown_one_does_not() {
        assert_eq!(parse_kind("verdict"), Some(BoardKind::Verdict));
        assert_eq!(parse_kind("  DECISION "), Some(BoardKind::Decision));
        assert_eq!(parse_kind("nonsense"), None);
        assert_eq!(parse_kind(""), None);
    }

    #[test]
    fn an_inbox_path_is_namespaced_by_run_and_member() {
        let path = inbox_dir("run-1", "w-t1");
        assert!(path.ends_with(Path::new("run-1").join("inbox").join("w-t1")));
    }

    #[test]
    fn a_drained_inbox_accepts_a_well_formed_entry() {
        let run = Run::new(
            "g",
            "/tmp",
            Member::new("lead", Role::Lead, "m"),
        );
        let dir = inbox_dir(&run.id, "w-t1");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("a.json"),
            r#"{"kind":"decision","text":"chose a token bucket","task":"t1"}"#,
        )
        .unwrap();

        let entries = drain_inbox(&run, "w-t1");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].kind, BoardKind::Decision);
        assert_eq!(entries[0].text, "chose a token bucket");
        assert_eq!(entries[0].from, "w-t1");
        // The file is consumed so it does not re-fire.
        assert!(!dir.join("a.json").exists());

        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    /// The boolean the verifier committed to wins over the first word of its
    /// text.
    ///
    /// The first live run marked a task verified on a verdict that opened with
    /// "FAILED. The mechanical criterion passes on the change ... but it is too
    /// weak to catch X". A boolean cannot be written that ambiguously, which is
    /// the whole reason the field exists.
    #[test]
    fn a_verdicts_boolean_ruling_wins_over_its_first_word() {
        let mut entry = BoardEntry::new(
            "v-t1",
            BoardKind::Verdict,
            "passed. The criterion fails on base and passes on the change.",
        );
        entry.passed = Some(false);
        assert_eq!(
            verdict_ruling(&entry),
            Some(false),
            "a verdict that says passed and means failed is read as a failure"
        );

        entry.passed = Some(true);
        entry.text = "failed. The criterion is too weak to catch the empty input.".into();
        assert_eq!(
            verdict_ruling(&entry),
            Some(true),
            "the field is the ruling in both directions, not only when it fails"
        );
    }

    /// An entry that omits the boolean is read by its first word.
    ///
    /// The field replaces the parse for new entries; it does not remove it. An
    /// entry written before the field existed, or by a verifier that wrote only
    /// prose, still has to be readable.
    #[test]
    fn a_verdict_without_a_boolean_falls_back_to_its_first_word() {
        let mut entry = BoardEntry::new(
            "v-t1",
            BoardKind::Verdict,
            "FAILED. The criterion passes on the change.",
        );
        assert_eq!(entry.passed, None);
        assert_eq!(verdict_ruling(&entry), Some(false));

        entry.text = "passed. Both halves hold.".into();
        assert_eq!(verdict_ruling(&entry), Some(true));
    }

    /// A verdict with neither a boolean nor a ruling is a failure, not a pass.
    #[test]
    fn a_verdict_with_no_boolean_and_no_ruling_is_not_a_pass() {
        let entry = BoardEntry::new(
            "v-t1",
            BoardKind::Verdict,
            "The criterion is a real test.",
        );
        assert_eq!(verdict_ruling(&entry), None);
    }

    /// The boolean survives the inbox, which is the only way it reaches the
    /// ruling: the drain rebuilds the entry field by field, so a field it does
    /// not copy is a field the verifier wrote and the engine never saw.
    #[test]
    fn a_drained_verdict_carries_its_boolean_ruling() {
        let run = Run::new("g", "/tmp", Member::new("lead", Role::Lead, "m"));
        let dir = inbox_dir(&run.id, "v-t1");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("v.json"),
            r#"{"kind":"verdict","passed":false,"task":"t1","text":"passed. The criterion holds, but an empty input panics where the base returned 0.","command":"node --test test/step.test.js","evidence":["node --test test/step.test.js -> 1 failed"]}"#,
        )
        .unwrap();

        let entries = drain_inbox(&run, "v-t1");
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].passed,
            Some(false),
            "the boolean reaches the board entry"
        );
        assert_eq!(
            verdict_ruling(&entries[0]),
            Some(false),
            "and it is the ruling, not the first word of the text"
        );

        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    /// The criterion is proved before the verifier is spawned, so the brief can
    /// state the result instead of asking for it.
    #[tokio::test]
    async fn the_criterion_is_proved_before_the_verifier_is_spawned() {
        let base = std::env::temp_dir().join(format!("vd-crit-base-{}", super::super::new_run_id()));
        let change =
            std::env::temp_dir().join(format!("vd-crit-change-{}", super::super::new_run_id()));
        std::fs::create_dir_all(&base).unwrap();
        std::fs::create_dir_all(&change).unwrap();
        std::fs::write(change.join("marker"), "x").unwrap();

        let task = Task::new("t1", "One", "Do it.", "test -f marker");
        let proof = criterion_proof(&task, &base, &change).await;
        assert_eq!(proof.base_exit, Some(1), "the base has no marker");
        assert_eq!(proof.change_exit, Some(0), "the change does");
        assert!(proof.failing_first);

        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&change);
    }

    /// The proof is computed before the verifier's process exists.
    ///
    /// The order is carried by `VerifierPrep`: the proof is a field of the value
    /// the preparation step returns, and `spawn_for_task` cannot reach
    /// `spawn_member` without that value in hand. What this test pins is that the
    /// preparation step produces the proof itself, from the trees it just
    /// created — the change side at the branch tip, the base side at the task's
    /// own base commit — and that it starts no process doing it. A proof computed
    /// after the spawn would leave the verifier's session open and unprompted for
    /// up to two `PROOF_TIMEOUT`s, which is the waste this phase exists to remove.
    #[tokio::test]
    async fn a_verifiers_proof_is_computed_before_its_process_exists() {
        let (repo, base) = repo_with_a_commit("verifier-prep");
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(&repo)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        // The branch under test adds the marker the criterion looks for.
        git(&["checkout", "-q", "-b", "swarm/run-1/t1"]);
        std::fs::write(repo.join("marker"), "x").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "two"]);

        let run_id = format!("run-{}", crate::swarm::new_run_id());
        let mut task = Task::new("t1", "One", "Do it.", "test -f marker");
        task.base_commit = Some(base.clone());
        let path = worktree::worktree_path(&run_id, "v-t1");

        let prep = prepare_verifier(&run_id, &task, &repo, &path, "swarm/run-1/t1", &base)
            .await
            .expect("the trees are prepared");

        // The trees the proof ran against are the ones this step created.
        assert!(path.is_dir(), "the verifier's worktree exists");
        assert!(prep.base_path.is_dir(), "the base checkout exists");
        assert_eq!(
            prep.readable,
            vec![prep.base_path.clone()],
            "the base checkout is the one directory the verifier may read"
        );

        // And the proof is a real observation from them, not a placeholder.
        assert_eq!(prep.proof.base_exit, Some(1), "the base has no marker");
        assert_eq!(prep.proof.change_exit, Some(0), "the branch tip does");
        assert!(prep.proof.failing_first);

        // No process was started to get it.
        let started = {
            let inner = engine().inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.bridges.contains_key("v-t1")
        };
        assert!(
            !started,
            "preparing a verifier must not start its process"
        );

        // `release_run`, not two `remove_worktree`s: the second left the run's
        // directory behind in the user's real `~/.vibe/worktrees`.
        let problems = worktree::release_run(&CommandGit, &repo, &run_id);
        assert!(problems.is_empty(), "the test's own checkouts are removed: {problems:?}");
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// A criterion the engine cannot run is not attempted, and the reason is the
    /// engine's rather than a shell's exit 127 on both sides.
    #[tokio::test]
    async fn a_criterion_the_engine_cannot_run_is_not_attempted() {
        let missing = Path::new("/does/not/exist");

        let prose = Task::new("t1", "One", "Do it.", "the tests pass, and the build is clean");
        let proof = criterion_proof(&prose, missing, missing).await;
        assert_eq!(proof.base_exit, None);
        let note = proof.note.unwrap();
        assert!(note.contains("not one shell command"), "{note}");

        let empty = Task::new("t2", "Two", "Do it.", "   ");
        let proof = criterion_proof(&empty, missing, missing).await;
        assert_eq!(proof.base_exit, None);
        assert!(proof.note.unwrap().contains("no acceptance criterion"));
    }

    #[test]
    fn a_verdict_without_evidence_is_rejected_with_a_receipt() {
        let run = Run::new("g", "/tmp", Member::new("lead", Role::Lead, "m"));
        let dir = inbox_dir(&run.id, "v-t1");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("v.json"),
            r#"{"kind":"verdict","text":"passed","task":"t1"}"#,
        )
        .unwrap();

        let entries = drain_inbox(&run, "v-t1");
        assert!(entries.is_empty(), "a verdict with no evidence is discarded");

        let receipt = receipts_dir(&run.id, "v-t1").join("v.json");
        assert!(receipt.exists());
        let body = std::fs::read_to_string(&receipt).unwrap();
        assert!(body.contains("\"accepted\": false"), "{body}");
        assert!(body.contains("evidence"), "{body}");

        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    #[test]
    fn a_malformed_entry_is_rejected_rather_than_crashing_the_loop() {
        let run = Run::new("g", "/tmp", Member::new("lead", Role::Lead, "m"));
        let dir = inbox_dir(&run.id, "w-t1");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("bad.json"), "{ not json").unwrap();
        std::fs::write(dir.join("empty.json"), r#"{"kind":"note","text":"  "}"#).unwrap();

        let entries = drain_inbox(&run, "w-t1");
        assert!(entries.is_empty());
        assert!(!dir.join("bad.json").exists());
        assert!(!dir.join("empty.json").exists());

        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    #[test]
    fn a_missing_inbox_is_empty_not_an_error() {
        let run = Run::new("g", "/tmp", Member::new("lead", Role::Lead, "m"));
        assert!(drain_inbox(&run, "nobody").is_empty());
    }

    #[test]
    fn the_rehearsal_view_reports_violations_for_a_bad_plan() {
        let engine = Engine::default();
        let mut plan = roles::example_plan();
        plan.tasks.truncate(1);
        let view = engine.rehearse(&plan, 3);
        assert!(!view.valid());
        assert!(view.violations.iter().any(|v| v.key == "too-few-tasks"));
    }

    #[test]
    fn the_rehearsal_view_accepts_the_example_plan() {
        let engine = Engine::default();
        let view = engine.rehearse_example(3);
        assert!(view.valid(), "{:?}", view.violations);
        assert_eq!(view.rehearsal.waves.len(), 2);
        assert_eq!(view.concurrency, 2);
    }

    #[test]
    fn the_rehearsal_never_asks_for_more_workers_than_the_plan_can_use() {
        let engine = Engine::default();
        let view = engine.rehearse_example(6);
        assert_eq!(view.concurrency, 2, "the example plan is two-wide");
    }

    #[test]
    fn the_required_capabilities_are_every_capability() {
        let keys = required_capabilities();
        assert_eq!(keys.len(), CapabilityId::ALL.len());
        assert!(keys.contains(&"compaction"));
        assert!(keys.contains(&"web"));
        assert!(keys.contains(&"channeler"));
    }

    #[test]
    fn a_capability_that_arrives_over_mcp_is_unreachable_until_its_server_is_registered() {
        // The panel used to count the toggle alone, so it certified the code
        // graph and web search while nothing checked that a member could call
        // them. These are the two that arrive over MCP; the other four run on a
        // boundary the app owns and cannot be missing.
        assert_eq!(unreachable_from(&[]), vec!["code-graph", "web"]);
        assert_eq!(
            unreachable_from(&["codebase-memory".to_string()]),
            vec!["web"]
        );
        assert!(unreachable_from(&[
            "codebase-memory".to_string(),
            "donsetch".to_string(),
        ])
        .is_empty());
        // A server the app does not manage does not satisfy a capability.
        assert_eq!(
            unreachable_from(&["some-other-server".to_string()]),
            vec!["code-graph", "web"]
        );
    }

    #[test]
    fn a_precondition_list_reports_ready_only_when_every_entry_is() {
        let all_ready = vec![
            Precondition::ready("a", "A"),
            Precondition::ready("b", "B"),
        ];
        assert!(ready(&all_ready));

        let one_blocked = vec![
            Precondition::ready("a", "A"),
            Precondition::blocked("b", "B", "nope"),
        ];
        assert!(!ready(&one_blocked));
    }

    #[test]
    fn preconditions_are_computed_without_a_workspace() {
        // Must not panic and must name the missing workspace rather than
        // silently reporting ready.
        let list = preconditions(None);
        assert!(!list.is_empty());
        let repository = list.iter().find(|p| p.key == "repository").unwrap();
        assert!(!repository.ready);
        assert!(repository.detail.contains("no workspace"));
    }

    #[test]
    fn a_non_repository_workspace_blocks_the_repository_precondition() {
        let dir = std::env::temp_dir().join(format!("vd-not-a-repo-{}", super::super::new_run_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let list = preconditions(Some(&dir));
        let repository = list.iter().find(|p| p.key == "repository").unwrap();
        assert!(!repository.ready);
        assert!(repository.detail.contains("not a git repository"));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A git that records what it was asked and answers from a script.
    struct FakeGit {
        calls: std::sync::Mutex<Vec<String>>,
    }

    impl FakeGit {
        fn new() -> Self {
            Self {
                calls: std::sync::Mutex::new(Vec::new()),
            }
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl worktree::Git for FakeGit {
        fn run(&self, args: &[&str], _cwd: &Path) -> Result<String, String> {
            self.calls.lock().unwrap().push(args.join(" "));
            Ok(String::new())
        }
    }

    fn earned() -> Verdict {
        Verdict {
            passed: true,
            reason: "pass — the criterion fails on the base and passes on the branch".into(),
            evidence: vec!["cargo test rate_limit".into()],
            command: Some("cargo test rate_limit".into()),
            base_exit: Some(1),
            change_exit: Some(0),
            failing_first: true,
            base_dirtied: false,
            at: now(),
        }
    }

    fn started(transitions: &[Transition]) -> Vec<String> {
        transitions
            .iter()
            .filter_map(|t| match t {
                Transition::Start { task } => Some(task.clone()),
                _ => None,
            })
            .collect()
    }

    /// The whole state machine, minus the processes.
    ///
    /// This is the deepest proof available without a live agent, and it is the
    /// one that would catch the two failures that matter: a scheduler that
    /// deadlocks on a valid DAG, and a merge order that disagrees with the DAG.
    /// Every step here is the same function the drive loop calls.
    #[test]
    fn a_plan_walks_from_blocked_to_integrated_without_a_process() {
        let plan = roles::example_plan();
        assert!(plan::validate(&plan).is_ok(), "the example plan must be valid");

        let mut run = Run::new("g", "/tmp", Member::new("lead", Role::Lead, "m"));
        run.tasks = plan.tasks.clone();
        for task in &mut run.tasks {
            task.state = TaskState::Blocked;
            task.branch = Some(worktree::branch_name(&run.id, &task.id));
        }
        run.integration = Integration::new(format!("swarm/{}", run.id), "base");
        run.concurrency = 2;

        // The rehearsal must agree with the scheduler, because it *is* the
        // scheduler run against copies.
        let rehearsal = schedule::rehearse(&run.tasks, run.concurrency, None);
        assert_eq!(rehearsal.waves.len(), 2, "two waves: t1+t3, then t2");

        // Round 1 — t1 and t3 are independent, so both take a slot.
        let transitions = schedule::advance(&mut run.tasks, run.concurrency);
        assert_eq!(started(&transitions), vec!["t1", "t3"]);

        // Both report evidence and both pass.
        for id in ["t1", "t3"] {
            let task = run.task_mut(id).unwrap();
            task.state = TaskState::Verifying;
            let transition = schedule::apply_verdict(task, earned(), 2);
            assert!(
                matches!(transition, Transition::Complete { .. }),
                "{id} should complete"
            );
        }

        // Round 2 — t2 unblocks now that t1 is verified.
        let transitions = schedule::advance(&mut run.tasks, run.concurrency);
        assert_eq!(started(&transitions), vec!["t2"]);

        let task = run.task_mut("t2").unwrap();
        task.state = TaskState::Verifying;
        schedule::apply_verdict(task, earned(), 2);

        assert!(run.all_verified());
        assert!(!schedule::is_stuck(&run.tasks), "a finished run is not stuck");

        // Integration merges in DAG order, not declaration order.
        let git = FakeGit::new();
        let outcome = integrate::integrate(&git, Path::new("/tmp"), &mut run);
        assert!(
            matches!(outcome, IntegrationOutcome::Merged { .. }),
            "{outcome:?}"
        );
        assert_eq!(run.integration.merged, vec!["t1", "t3", "t2"]);
        assert_eq!(run.integration.state, IntegrationState::Merged);
        // The merge is a real git call, in the order the DAG gave.
        let calls = git.calls();
        assert!(
            calls.iter().any(|call| call.starts_with("merge")),
            "{calls:?}"
        );
        let merged: Vec<&String> = calls.iter().filter(|c| c.starts_with("merge")).collect();
        assert_eq!(merged.len(), 3, "one merge per verified task: {calls:?}");
    }

    /// A verdict that passes without proving the criterion fails on the base is
    /// not a test, and the scheduler must treat it as a failure.
    #[test]
    fn an_unearned_pass_does_not_verify_a_task() {
        let mut task = Task::new("t1", "t", "d", "a");
        task.state = TaskState::Verifying;
        task.attempts = 1;

        let unearned = Verdict {
            passed: true,
            reason: "looks right to me".into(),
            evidence: vec!["read the diff".into()],
            command: None,
            base_exit: None,
            change_exit: None,
            failing_first: false,
            base_dirtied: false,
            at: now(),
        };
        let transition = schedule::apply_verdict(&mut task, unearned, 2);
        assert!(matches!(transition, Transition::Fail { .. }));
        assert_eq!(
            task.state,
            TaskState::Ready,
            "it goes back for another attempt"
        );
    }

    /// A task that fails its last attempt stops the run rather than spinning.
    #[test]
    fn a_task_that_exhausts_its_attempts_makes_the_run_stuck() {
        let mut tasks = vec![
            Task::new("t1", "t", "d", "a"),
            Task::new("t2", "t", "d", "a"),
        ];
        tasks[1].deps = vec!["t1".into()];
        for task in &mut tasks {
            task.state = TaskState::Blocked;
        }

        let transitions = schedule::advance(&mut tasks, 2);
        assert_eq!(started(&transitions), vec!["t1"]);

        tasks[0].state = TaskState::Verifying;
        tasks[0].attempts = 2;
        let failed = Verdict {
            passed: false,
            reason: "the criterion passes on the base too".into(),
            evidence: vec!["cargo test rate_limit on base: ok".into()],
            command: Some("cargo test rate_limit".into()),
            base_exit: Some(0),
            change_exit: Some(0),
            failing_first: false,
            base_dirtied: false,
            at: now(),
        };
        let transition = schedule::apply_verdict(&mut tasks[0], failed, 2);
        assert!(matches!(transition, Transition::Fail { .. }));
        assert_eq!(tasks[0].state, TaskState::Failed);

        // t2 can never start, so the run must say so instead of waiting.
        assert!(schedule::is_stuck(&tasks));
    }

    /// The engine refuses to approve a plan the gate would refuse.
    #[test]
    fn a_plan_with_a_cycle_is_refused_before_it_can_start() {
        let mut plan = roles::example_plan();
        plan.tasks[0].deps = vec!["t2".into()];
        let violations = plan::validate(&plan).err().unwrap_or_default();
        assert!(
            violations.iter().any(|v| v.key() == "cycle"),
            "{violations:?}"
        );
    }

    /// Two unordered tasks claiming one file would produce a merge nobody can
    /// attribute, so the plan is refused rather than run and conflicted.
    #[test]
    fn two_unordered_tasks_claiming_one_file_are_refused() {
        let mut plan = roles::example_plan();
        plan.tasks[2].files = vec!["src/rate.rs".into()];
        let violations = plan::validate(&plan).err().unwrap_or_default();
        assert!(
            violations.iter().any(|v| v.key() == "overlapping-files"),
            "{violations:?}"
        );
    }

    /// The same overlap is legal when one task waits for the other: the second
    /// sees the first's change, so the merge is a fast-forward.
    #[test]
    fn the_same_overlap_is_legal_across_a_dependency_edge() {
        let mut plan = roles::example_plan();
        plan.tasks[1].files = vec!["src/rate.rs".into()];
        assert!(
            plan::validate(&plan).is_ok(),
            "t2 depends on t1, so sharing src/rate.rs is ordered"
        );
    }

    /// A plan with no integration suite is refused: every task can pass and the
    /// combination still be broken, which is the failure a swarm is most likely
    /// to produce.
    #[test]
    fn a_plan_without_an_integration_suite_is_refused() {
        let mut plan = roles::example_plan();
        plan.integration_suite = None;
        let violations = plan::validate(&plan).err().unwrap_or_default();
        assert!(
            violations.iter().any(|v| v.key() == "no-integration-suite"),
            "{violations:?}"
        );
    }

    /// A task with no acceptance criterion cannot be verified, only approved.
    #[test]
    fn a_task_without_an_acceptance_criterion_is_refused() {
        let mut plan = roles::example_plan();
        plan.tasks[0].acceptance = String::new();
        let violations = plan::validate(&plan).err().unwrap_or_default();
        assert!(
            violations.iter().any(|v| v.key() == "missing-acceptance"),
            "{violations:?}"
        );
    }

    /// The digest a worker receives carries upstream decisions and nothing else.
    #[test]
    fn a_worker_digest_carries_upstream_decisions_and_not_a_transcript() {
        let mut board = Board::new();
        board.append(
            BoardEntry::new("w-t1", BoardKind::Decision, "token bucket, not sliding window")
                .about("t1"),
        );
        board.append(
            BoardEntry::new("w-t1", BoardKind::Evidence, "cargo test: 12 passed").about("t1"),
        );
        board.append(BoardEntry::new("w-t9", BoardKind::Decision, "unrelated").about("t9"));

        let digest = board.digest_for("t2", &["t1".to_string()]);
        assert!(digest.contains("token bucket"), "{digest}");
        assert!(
            !digest.contains("12 passed"),
            "evidence does not cross an edge"
        );
        assert!(!digest.contains("unrelated"), "{digest}");
    }

    /* -------------------------------------------------------------- *
     * The live proof
     *
     * Everything above runs against fakes. These run against a real
     * `vibe-acp` in a real git repository, because the one thing a fake
     * cannot tell you is whether the process-spawning path works.
     *
     * `#[ignore]` by default: the default suite must not need an agent on
     * PATH. Run them with
     * `cargo test --lib swarm::engine::tests::live -- --ignored --nocapture`.
     * -------------------------------------------------------------- */

    /// A scratch git repository with one commit, and a worktree on a branch.
    ///
    /// Returns `(repo, worktree, base_commit)`. The repo is a real one: the
    /// proof runs `git worktree add`, `git diff` and a real command in it.
    fn live_repo(name: &str) -> (PathBuf, PathBuf, String) {
        let repo =
            std::env::temp_dir().join(format!("vd-live-{name}-{}", super::super::new_run_id()));
        std::fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(&repo)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "live@local"]);
        git(&["config", "user.name", "live"]);
        git(&["config", "commit.gpgsign", "false"]);

        std::fs::write(repo.join("rate.py"), "def allow(key):\n    return True\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "base: a limiter that allows everything"]);
        let base = git(&["rev-parse", "HEAD"]);

        let worktree = repo.join("wt");
        git(&[
            "worktree",
            "add",
            "-b",
            "swarm/live/t1",
            &worktree.to_string_lossy(),
            &base,
        ]);
        (repo, worktree, base)
    }

    fn cleanup(repo: &Path) {
        let _ = std::process::Command::new("git")
            .args(["worktree", "remove", "--force", "wt"])
            .current_dir(repo)
            .output();
        let _ = std::fs::remove_dir_all(repo);
    }

    /// The brief the live tests give a worker. Small on purpose: these prove the
    /// path, not the model.
    const LIVE_BRIEF: &str = "Edit rate.py so that `allow(key)` returns False when key == \
                              \"blocked\" and True otherwise. Change nothing else. Then commit \
                              your work on the current branch with the message 'limit blocked'. \
                              Do not run any other command.";

    /// A mode the session does not offer is an error, not a silent fallback.
    ///
    /// This is the failure the whole provisioning path exists to prevent: a
    /// member that keeps the session default runs on Vibe's coding prompt, and
    /// the lead implements the goal instead of planning it. The run looks like a
    /// swarm and behaves like one agent, which is invisible unless something
    /// refuses.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs a real vibe-acp on PATH"]
    async fn live_an_unoffered_mode_is_refused_rather_than_silently_defaulted() {
        let (repo, worktree, _base) = live_repo("mode");
        let sink = Arc::new(crate::acp::bridge::RecordingSink::default());

        let error = spawn_member(
            sink,
            "w-t1",
            Role::Worker,
            &worktree,
            "deepseek",
            Some("high"),
            Some("vibedesk-swarm-does-not-exist"),
            Vec::new(),
            None,
        )
        .await
        .err()
        .expect("an unoffered mode must not be silently ignored");

        assert!(
            error.contains("vibedesk-swarm-does-not-exist"),
            "the error names the agent it wanted: {error}"
        );
        assert!(
            error.contains("vibedesk-swarm-does-not-exist.toml"),
            "the error names the file it expected: {error}"
        );
        assert!(
            error.contains("smart-approve"),
            "the error lists what the session did offer: {error}"
        );

        cleanup(&repo);
    }

    /// The whole process-spawning path, against a real agent.
    ///
    /// This is the claim that was "not proven": that `spawn_member` starts a
    /// `vibe-acp`, opens a session in a worktree, applies the model and the
    /// mode, runs a turn, and that the member's events land on its own scoped
    /// names rather than the main chat's.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs a real vibe-acp on PATH"]
    async fn live_a_member_spawns_runs_a_turn_and_stays_off_the_main_event_names() {
        let (repo, worktree, _base) = live_repo("spawn");
        let sink = Arc::new(crate::acp::bridge::RecordingSink::default());

        let spawned = spawn_member(
            sink.clone(),
            "w-t1",
            Role::Worker,
            &worktree,
            "deepseek",
            Some("high"),
            Some(roles::agent_for(Role::Worker)),
            Vec::new(),
            None,
        )
        .await
        .expect("a member must start against a real agent");

        // The session exists, has a pid, and reports what it actually got.
        assert!(!spawned.session_id.is_empty());
        assert!(spawned.pid.is_some(), "a real process has a pid");
        // The worker's agent, not a built-in mode. This only holds once
        // provisioning has written `~/.vibe/agents/vibedesk-swarm-worker.toml`;
        // without it the session does not offer the mode and keeps its default,
        // which is the silent fallback `provision::swarm_ready` exists to stop.
        assert_eq!(
            spawned.agent.as_deref(),
            Some(roles::agent_for(Role::Worker)),
            "the worker's own agent was requested and the session reports it back"
        );
        assert_eq!(
            spawned.model.as_deref(),
            Some("deepseek"),
            "the model was requested and the session reports it back"
        );

        spawned
            .bridge
            .turn(
                "session/prompt",
                json!({
                    "sessionId": spawned.session_id,
                    "prompt": [{ "type": "text", "text": LIVE_BRIEF }],
                }),
            )
            .await
            .expect("the turn must settle");

        // The agent actually edited the file in its own worktree.
        let edited = std::fs::read_to_string(worktree.join("rate.py")).unwrap();
        assert!(
            edited.contains("blocked"),
            "the worker edited its worktree, not somewhere else: {edited}"
        );

        // Isolation: every event landed on the scoped name, and none on the
        // main session's.
        assert!(
            !sink.on("acp://notification/swarm/w-t1").is_empty(),
            "a real turn emits notifications; got {:?}",
            sink.events()
                .iter()
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>()
        );
        assert!(
            sink.on("acp://notification").is_empty(),
            "a member must never emit on the main session's event name"
        );
        assert!(
            !sink.on("agent://status/swarm/w-t1").is_empty(),
            "the member reports its own status"
        );

        spawned.bridge.stop().await.unwrap();
        cleanup(&repo);
    }

    /// The MCP servers the user registered reach a real member.
    ///
    /// The panel claimed this and nothing checked it. It turns out to hold for a
    /// reason the code did not state: `vibe-acp` integrates the servers in its
    /// own `config.toml` whatever the client sends, so a session opened with
    /// `mcpServers: []` still reaches the code graph. This test pins that
    /// behaviour, because the app's own comment claimed the opposite and a
    /// reader who believed it would conclude the capability was unreachable.
    ///
    /// It drives a real `vibe-acp` in `ask` mode, the only mode where the gate is
    /// guaranteed to see the call, and asserts the call happened and completed.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs a real vibe-acp on PATH and a registered MCP server"]
    async fn live_a_member_can_call_the_mcp_servers_the_user_registered() {
        let (repo, worktree, _base) = live_repo("mcp");
        let sink = Arc::new(crate::acp::bridge::RecordingSink::default());

        let spawned = spawn_member(
            sink.clone(),
            "w-t1",
            Role::Worker,
            &worktree,
            "deepseek",
            Some("high"),
            // `ask` rather than the worker's own agent: the point is that the
            // gate answers an MCP request, and a worker's agent may auto-run the
            // call without ever asking.
            Some("ask"),
            Vec::new(),
            None,
        )
        .await
        .expect("a member must start against a real agent");

        spawned
            .bridge
            .turn(
                "session/prompt",
                json!({
                    "sessionId": spawned.session_id,
                    "prompt": [{ "type": "text", "text":
                        "Call the codebase-memory MCP tool `list_projects` exactly once. \
                         If you have no MCP tools at all, reply exactly NO_MCP_TOOLS and stop." }],
                }),
            )
            .await
            .expect("the turn must settle");

        let events = sink.on("acp://notification/swarm/w-t1");
        // The announcement carries the title; the result update carries only the
        // id and the status. Correlate on the id, which is what the gate does.
        let announced: Vec<(String, String)> = events
            .iter()
            .filter_map(|payload| payload.pointer("/params/update"))
            .filter(|update| {
                update.get("sessionUpdate").and_then(Value::as_str) == Some("tool_call")
            })
            .filter_map(|update| {
                Some((
                    update.get("toolCallId")?.as_str()?.to_string(),
                    update.get("title")?.as_str()?.to_string(),
                ))
            })
            .collect();
        let mcp_ids: Vec<&String> = announced
            .iter()
            .filter(|(_, title)| title.starts_with("mcp_"))
            .map(|(id, _)| id)
            .collect();

        assert!(
            !mcp_ids.is_empty(),
            "the member must have an MCP tool to call; it announced {:?}",
            announced
        );

        let completed: Vec<String> = events
            .iter()
            .filter_map(|payload| payload.pointer("/params/update"))
            .filter(|update| {
                update.get("sessionUpdate").and_then(Value::as_str) == Some("tool_call_update")
            })
            .filter(|update| update.get("status").and_then(Value::as_str) == Some("completed"))
            .filter_map(|update| update.get("toolCallId")?.as_str().map(str::to_string))
            .collect();

        assert!(
            mcp_ids.iter().any(|id| completed.contains(id)),
            "the MCP call must complete, not be refused by the gate; announced {:?}, completed {:?}",
            announced,
            completed
        );

        spawned.bridge.stop().await.unwrap();
        cleanup(&repo);
    }

    /// A verifier may call an MCP server, which it could not before.
    ///
    /// Every MCP permission request carries no effects, so it read as
    /// `Effect::Unknown` and the gate refused it for a verifier — removing the
    /// code graph and web search from the one role whose job is checking a
    /// change against the codebase.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs a real vibe-acp on PATH and a registered MCP server"]
    async fn live_a_verifier_can_call_an_mcp_server() {
        let (repo, worktree, _base) = live_repo("mcp-verify");
        let sink = Arc::new(crate::acp::bridge::RecordingSink::default());

        let spawned = spawn_member(
            sink.clone(),
            "v-t1",
            Role::Verifier,
            &worktree,
            "deepseek",
            Some("high"),
            Some("ask"),
            Vec::new(),
            None,
        )
        .await
        .expect("a verifier must start against a real agent");

        spawned
            .bridge
            .turn(
                "session/prompt",
                json!({
                    "sessionId": spawned.session_id,
                    "prompt": [{ "type": "text", "text":
                        "Call the codebase-memory MCP tool `list_projects` exactly once. \
                         If you have no MCP tools at all, reply exactly NO_MCP_TOOLS and stop." }],
                }),
            )
            .await
            .expect("the turn must settle");

        let events = sink.on("acp://notification/swarm/v-t1");
        let mcp_ids: Vec<String> = events
            .iter()
            .filter_map(|payload| payload.pointer("/params/update"))
            .filter(|update| {
                update.get("sessionUpdate").and_then(Value::as_str) == Some("tool_call")
            })
            .filter(|update| {
                update
                    .get("title")
                    .and_then(Value::as_str)
                    .is_some_and(|title| title.starts_with("mcp_"))
            })
            .filter_map(|update| update.get("toolCallId")?.as_str().map(str::to_string))
            .collect();
        let completed: Vec<String> = events
            .iter()
            .filter_map(|payload| payload.pointer("/params/update"))
            .filter(|update| {
                update.get("sessionUpdate").and_then(Value::as_str) == Some("tool_call_update")
            })
            .filter(|update| update.get("status").and_then(Value::as_str) == Some("completed"))
            .filter_map(|update| update.get("toolCallId")?.as_str().map(str::to_string))
            .collect();

        assert!(
            !mcp_ids.is_empty(),
            "a verifier must have an MCP tool to call"
        );
        assert!(
            mcp_ids.iter().any(|id| completed.contains(id)),
            "a verifier's MCP call must complete; the gate refused it before this fix"
        );

        spawned.bridge.stop().await.unwrap();
        cleanup(&repo);
    }

    /// Failing-first, proven rather than claimed.
    ///
    /// A real agent makes a real change in a real worktree; the engine then runs
    /// one command on the base commit and on the change and records both exit
    /// codes. The command is chosen so it *must* fail on the base: it asserts the
    /// behaviour the change introduces.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs a real vibe-acp on PATH"]
    async fn live_failing_first_is_observed_on_a_real_change() {        let (repo, worktree, base) = live_repo("prove");
        let sink = Arc::new(crate::acp::bridge::RecordingSink::default());

        let spawned = spawn_member(
            sink.clone(),
            "w-t1",
            Role::Worker,
            &worktree,
            "deepseek",
            Some("high"),
            Some(roles::agent_for(Role::Worker)),
            Vec::new(),
            None,
        )
        .await
        .expect("a member must start");

        spawned
            .bridge
            .turn(
                "session/prompt",
                json!({
                    "sessionId": spawned.session_id,
                    "prompt": [{ "type": "text", "text": LIVE_BRIEF }],
                }),
            )
            .await
            .expect("the turn must settle");
        spawned.bridge.stop().await.unwrap();

        // The base checkout, detached at the commit the run started from.
        let base_checkout = repo.join("base");
        let out = std::process::Command::new("git")
            .args([
                "worktree",
                "add",
                "--detach",
                &base_checkout.to_string_lossy(),
                &base,
            ])
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );

        // The change must actually differ, or the proof is vacuous.
        let diff = std::process::Command::new("git")
            .args(["diff", "--stat", &base, "swarm/live/t1"])
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(
            !String::from_utf8_lossy(&diff.stdout).trim().is_empty(),
            "the worker committed nothing, so there is no change to prove"
        );

        // One command, two answers. It asserts the new behaviour, so it cannot
        // pass on the base.
        let command = "python -c \"from rate import allow; assert allow('blocked') is False\"";
        let proof = prove::prove(&base_checkout, &worktree, command, prove::PROOF_TIMEOUT);
        assert_eq!(
            proof.base_exit,
            Some(1),
            "the command must fail on the base: {:?}",
            proof.note
        );
        assert_eq!(
            proof.change_exit,
            Some(0),
            "the command must pass on the change: {:?}",
            proof.note
        );
        assert!(proof.failing_first, "both exits observed, so it is a proof");
        assert!(proof.note.is_none());

        cleanup(&repo);
    }

    /// A command that passes on both sides is not a proof, on a real repo.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs a real vibe-acp on PATH"]
    async fn live_a_command_that_passes_on_the_base_is_refused_as_a_proof() {
        let (repo, worktree, base) = live_repo("vacuous");
        let base_checkout = repo.join("base");
        let out = std::process::Command::new("git")
            .args([
                "worktree",
                "add",
                "--detach",
                &base_checkout.to_string_lossy(),
                &base,
            ])
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(out.status.success());

        // `allow` returns True on both sides, so this command proves nothing.
        let command = "python -c \"from rate import allow; assert allow('a') is True\"";
        let proof = prove::prove(&base_checkout, &worktree, command, prove::PROOF_TIMEOUT);
        assert_eq!(proof.base_exit, Some(0));
        assert_eq!(proof.change_exit, Some(0));
        assert!(!proof.failing_first);
        assert!(proof.note.unwrap().contains("does not test the change"));

        cleanup(&repo);
    }

    /* -------------------------------------------------------------- *
     * The reflection pass
     * -------------------------------------------------------------- */

    fn reflection_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("vd-reflect-{}", super::super::new_run_id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_reflection_that_has_not_been_written_yet_is_missing_not_malformed() {
        let dir = reflection_dir();
        assert!(matches!(read_reflection(&dir), ReflectionRead::Missing));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_written_reflection_is_read_and_consumed() {
        let dir = reflection_dir();
        std::fs::write(
            dir.join("gotchas.json"),
            r#"{"summary":"ok","gotchas":[{"file":"src/a.rs","text":"t","source":"verdict t1"}]}"#,
        )
        .unwrap();

        match read_reflection(&dir) {
            ReflectionRead::Draft(draft) => {
                assert_eq!(draft.summary, "ok");
                assert_eq!(draft.gotchas.len(), 1);
            }
            _ => panic!("a written reflection must be read"),
        }
        // Consumed: a second tick cannot re-apply it.
        assert!(!dir.join("gotchas.json").exists());
        assert!(matches!(read_reflection(&dir), ReflectionRead::Missing));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_malformed_reflection_is_reported_once_and_kept_for_inspection() {
        let dir = reflection_dir();
        std::fs::write(dir.join("gotchas.json"), "I had a lovely time.").unwrap();

        match read_reflection(&dir) {
            ReflectionRead::Malformed(error) => assert!(error.contains("not a reflection"), "{error}"),
            _ => panic!("prose is not a reflection"),
        }
        // The body survives, so the user can tell a bad prompt from a bad model.
        let kept = std::fs::read_to_string(dir.join("gotchas.rejected.txt")).unwrap();
        assert_eq!(kept, "I had a lovely time.");
        // And it does not re-fire.
        assert!(matches!(read_reflection(&dir), ReflectionRead::Missing));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_the_reflector_has_an_inbox_that_is_not_board_entries() {
        assert!(drains_board_entries(Role::Lead));
        assert!(drains_board_entries(Role::Worker));
        assert!(drains_board_entries(Role::Verifier));
        assert!(!drains_board_entries(Role::Reflector));
    }

    #[test]
    fn the_reflector_is_a_role_with_a_name_of_its_own() {
        assert_eq!(Role::Reflector.label(), "reflector");
        assert_ne!(Role::Reflector.label(), Role::Verifier.label());
    }

    /// The pass runs on the cheapest tier the run already chose, not the lead's.
    #[test]
    fn the_reflector_inherits_the_worker_model() {
        let mut run = Run::new("do the thing", "/tmp/ws", Member::new("lead", Role::Lead, "big"));
        run.members
            .push(Member::new("w-t1", Role::Worker, "small"));
        let model = run
            .members
            .iter()
            .find(|m| m.role == Role::Worker)
            .map(|m| m.model.clone())
            .unwrap_or_else(|| run.lead.model.clone());
        assert_eq!(model, "small");
    }

    /// A reflection whose gotchas cite nothing on the board keeps none of them.
    #[test]
    fn a_reflection_over_an_empty_board_writes_no_gotchas() {
        let board = Board::new();
        let draft = reflect::parse(
            r#"{"summary":"s","gotchas":[{"file":"src/a.rs","text":"t","source":"verdict t1"}]}"#,
        )
        .unwrap();
        let (reflection, dropped) = reflect::accept(draft, &board);
        assert!(reflection.gotchas.is_empty());
        assert_eq!(dropped.len(), 1);
        assert!(reflection.degraded.is_none(), "an empty result is not degraded");
    }

    /// A run that never reached reflection still records why.
    #[test]
    fn a_degraded_reflection_names_its_reason() {
        let board = Board::new();
        let reflection = reflect::degraded("no session logs", &board);
        assert_eq!(reflection.degraded.as_deref(), Some("no session logs"));
        assert!(reflection.gotchas.is_empty());
    }

    #[test]
    fn gotchas_are_filtered_to_the_files_a_task_touches() {
        let gotchas = vec![
            Gotcha {
                file: "src/rate.rs".into(),
                text: "a".into(),
                source: "verdict t1".into(),
            },
            Gotcha {
                file: "src/other.rs".into(),
                text: "b".into(),
                source: "verdict t2".into(),
            },
        ];
        let hit = gotchas_for(&gotchas, &["src/rate.rs".to_string()]);
        assert_eq!(hit.len(), 1);
        assert_eq!(hit[0].text, "a");
    }
    /* -------------------------------------------------------------- *
     * Escalation: a member asking for a decision stops the run
     * -------------------------------------------------------------- */

    fn escalation(from: &str, text: &str) -> BoardEntry {
        BoardEntry::new(from, BoardKind::Escalation, text).about("t1")
    }

    #[test]
    fn a_member_escalation_stops_the_run() {
        let posted = vec![escalation(
            "w-t1",
            "the plan gives the arena fix to t2, but ARENA_HALF lives in src/player.js",
        )];
        let stop = escalation_stopping(&posted).expect("a member escalation must stop the run");
        assert_eq!(stop.from, "w-t1");
        assert!(stop.text.contains("src/player.js"));
    }

    #[test]
    fn the_engines_own_escalations_do_not_stop_the_run() {
        // Plan violations are posted by the engine and handled by sending the
        // plan back to the lead. Treating them as a member escalation would stop
        // every run whose first plan was rejected.
        let posted = vec![escalation("engine", "task t1 has no acceptance criteria")];
        assert!(escalation_stopping(&posted).is_none());
    }

    #[test]
    fn ordinary_board_traffic_does_not_stop_the_run() {
        let posted = vec![
            BoardEntry::new("w-t1", BoardKind::Claim, "claimed t1").about("t1"),
            BoardEntry::new("w-t1", BoardKind::Evidence, "done").about("t1"),
            BoardEntry::new("v-t1", BoardKind::Verdict, "pass").about("t1"),
            BoardEntry::new("engine", BoardKind::Note, "handed to a verifier").about("t1"),
        ];
        assert!(escalation_stopping(&posted).is_none());
    }

    /* -------------------------------------------------------------- *
     * Attempt staleness: a retry must not inherit the last result
     * -------------------------------------------------------------- */

    fn attempt_task(started_at: Option<&str>) -> Task {
        let mut task = Task::new("t1", "a task", "detail", "node --test");
        task.started_at = started_at.map(|s| s.to_string());
        task
    }

    fn at(entry_at: &str) -> BoardEntry {
        let mut entry = BoardEntry::new("v-t1", BoardKind::Verdict, "failed").about("t1");
        entry.at = entry_at.to_string();
        entry
    }

    #[test]
    fn a_verdict_from_the_previous_attempt_is_not_current() {
        // The eleventh live run: evidence at 20:44:28 put the task in
        // `Verifying`, and the next tick re-read the 20:37:48 verdict from the
        // attempt before it, applied it again and failed the task.
        let task = attempt_task(Some("2026-11-18T20:37:49.003Z"));
        assert!(!entry_is_current(&at("2026-11-18T20:37:48.000Z"), &task));
    }

    #[test]
    fn a_verdict_from_this_attempt_is_current() {
        let task = attempt_task(Some("2026-11-18T20:37:49.003Z"));
        assert!(entry_is_current(&at("2026-11-18T20:45:02.000Z"), &task));
    }

    /// A verdict retires the verifier: its bridge leaves the map — and the
    /// process with it — and the member is parked. Run 3's v-t6 posted the
    /// same verdict three times because the process stayed alive with its
    /// session and re-emitted its answer.
    #[tokio::test]
    async fn an_accepted_verdict_retires_the_verifiers_bridge_and_parks_the_member() {
        use crate::acp::bridge::NullSink;

        let mut run = planning_run("retire");
        run.state = RunState::Running;
        let mut task = Task::new("t1", "do the thing", "the detail", "the criterion");
        task.state = TaskState::Verifying;
        task.owner = Some("w-t1".into());
        run.tasks = vec![task];
        let mut worker = Member::new("w-t1", Role::Worker, "m");
        worker.task = Some("t1".into());
        worker.state = MemberState::Idle;
        let mut verifier = Member::new("v-t1", Role::Verifier, "m");
        verifier.task = Some("t1".into());
        verifier.state = MemberState::Working;
        run.members = vec![worker, verifier];

        {
            let mut inner = engine().inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.bridges.insert(
                "v-t1".into(),
                Arc::new(AcpBridge::with_sink(Arc::new(NullSink))),
            );
        }

        retire_verifier(&mut run, "t1").await;

        let bridge_gone = {
            let inner = engine().inner.lock().unwrap_or_else(|e| e.into_inner());
            !inner.bridges.contains_key("v-t1")
        };
        assert!(bridge_gone, "the verifier's bridge must be dropped on verdict");
        assert_eq!(
            run.member("v-t1").unwrap().state,
            MemberState::Parked,
            "the verifier is parked, not left working"
        );

        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    /// A no-op amendment while replanning is the lead confirming the plan: the
    /// step is consumed without escalating, and the plan keeps its tasks. Run
    /// run-20261121-081416-000 escalated on exactly this, because the no-op
    /// arrived as an undifferentiated `Err`.
    #[tokio::test]
    async fn a_no_op_amendment_in_replanning_does_not_escalate_the_run() {
        let mut run = planning_run("noop");
        run.state = RunState::Replanning;
        run.tasks = vec![Task::new("t1", "title", "detail", "criterion")];
        let inbox = inbox_dir(&run.id, "lead");
        std::fs::create_dir_all(&inbox).unwrap();
        std::fs::write(
            inbox.join("amendment.json"),
            r#"{"acceptance": {"t1": "criterion"}}"#,
        )
        .unwrap();

        let sink: Arc<dyn EventSink> = Arc::new(crate::acp::bridge::NullSink);
        let step = handle_lead_amendment(sink, &run);

        assert_eq!(
            step,
            AmendmentStep::Consumed,
            "a no-op amendment must not escalate the run"
        );
        assert_eq!(run.state, RunState::Replanning);
        assert_eq!(run.tasks.len(), 1, "the plan keeps its tasks");
        assert!(
            !inbox.join("amendment.json").exists(),
            "the amendment was consumed either way"
        );

        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    #[test]
    fn an_entry_posted_at_the_instant_the_attempt_started_is_not_current() {
        // The boundary is inclusive on the stale side: the retry's own claim is
        // posted in the same tick the attempt starts, and a result stamped with
        // that instant cannot be a result of work done after it.
        let task = attempt_task(Some("2026-11-18T20:37:49.003Z"));
        assert!(!entry_is_current(&at("2026-11-18T20:37:49.003Z"), &task));
    }

    #[test]
    fn a_task_with_no_start_time_accepts_any_entry() {
        // A run adopted from disk before `started_at` existed has no attempt
        // boundary to compare against, so the entry is taken at face value
        // rather than silently dropped.
        let task = attempt_task(None);
        assert!(entry_is_current(&at("2026-11-18T20:37:48.000Z"), &task));
    }

    #[test]
    fn the_first_member_escalation_wins() {
        let posted = vec![
            escalation("w-t2", "second"),
            escalation("w-t1", "first"),
        ];
        assert_eq!(escalation_stopping(&posted).unwrap().text, "second");
    }

    #[test]
    fn escalated_is_terminal_but_not_a_failure() {
        assert!(RunState::Escalated.is_terminal());
        assert!(!RunState::Escalated.is_live());
        assert_eq!(RunState::Escalated.label(), "escalated");
        // Every state has its own label: two states reading the same in the panel
        // is how a run that stopped for a decision looks like one that crashed.
        let all = [
            RunState::Planning,
            RunState::AwaitingApproval,
            RunState::Running,
            RunState::Integrating,
            RunState::Reflecting,
            RunState::Done,
            RunState::Failed,
            RunState::Escalated,
            RunState::Cancelled,
            RunState::Interrupted,
        ];
        let mut labels: Vec<&str> = all.iter().map(|s| s.label()).collect();
        labels.sort_unstable();
        let before = labels.len();
        labels.dedup();
        assert_eq!(labels.len(), before, "two run states share a label");
    }
    /* -------------------------------------------------------------- *
     * Question routing: each question to its owner, on that owner's budget
     * -------------------------------------------------------------- */

    fn a_run() -> Run {
        Run::new("goal", "D:/ws", Member::new("lead", Role::Lead, "m"))
    }

    #[test]
    fn a_plan_question_goes_to_the_lead_while_it_has_amendment_budget() {
        let mut run = a_run();
        assert_eq!(
            route_question(&run, Question::PlanNeedsChanging),
            Route {
                owner: Owner::Lead,
                state: RunState::Replanning,
                budget: Budget::Amendments
            }
        );
        run.amendments = 1;
        assert_eq!(
            route_question(&run, Question::PlanNeedsChanging).owner,
            Owner::Lead
        );
    }

    #[test]
    fn a_plan_question_goes_to_the_user_once_the_amendment_budget_is_spent() {
        // A lead that needs a third amendment is not coordinating, it is
        // guessing, and the user should see the question rather than watch the
        // run loop.
        let mut run = a_run();
        run.amendments = amend::MAX_AMENDMENTS;
        assert_eq!(
            route_question(&run, Question::PlanNeedsChanging),
            Route {
                owner: Owner::User,
                state: RunState::Escalated,
                budget: Budget::None
            }
        );
    }

    /// The criterion question is the plan gate's, and it does not touch the
    /// amendment budget.
    ///
    /// This is the routing the thirteenth live run needed and did not have. Its
    /// t2 criterion passed on the base commit, which is the plan's writing, and
    /// the engine charged it to `amendments` — the same pot a worker breaching
    /// its grant draws on. Two unrelated questions, one budget, and the run
    /// stopped with both unanswered.
    #[test]
    fn a_criterion_question_is_the_plan_gates_and_leaves_the_amendment_budget_alone() {
        let mut run = a_run();
        run.amendments = amend::MAX_AMENDMENTS;
        assert_eq!(
            route_question(&run, Question::CriterionUnprovable),
            Route {
                owner: Owner::PlanGate,
                state: RunState::Replanning,
                budget: Budget::PlanAttempts
            },
            "a spent amendment budget must not send a criterion question to the user"
        );
    }

    #[test]
    fn a_criterion_question_goes_to_the_user_once_the_plan_gate_is_out_of_revisions() {
        let mut run = a_run();
        run.plan_attempts = PLAN_ATTEMPTS;
        assert_eq!(
            route_question(&run, Question::CriterionUnprovable).owner,
            Owner::User
        );
    }

    /// The two budgets are independent in both directions.
    #[test]
    fn the_two_budgets_do_not_drain_each_other() {
        let mut run = a_run();
        run.plan_attempts = PLAN_ATTEMPTS;
        assert_eq!(
            route_question(&run, Question::PlanNeedsChanging).owner,
            Owner::Lead,
            "a spent plan gate must not send a grant question to the user"
        );

        let mut run = a_run();
        run.amendments = amend::MAX_AMENDMENTS;
        assert_eq!(
            route_question(&run, Question::CriterionUnprovable).owner,
            Owner::PlanGate,
            "a spent amendment budget must not send a criterion question to the user"
        );
    }

    /// The user's route is the same question with the budget spent, not a
    /// separate class. Both questions reach it, and neither spends a counter on
    /// the way out.
    #[test]
    fn a_question_with_no_budget_left_goes_to_the_user_without_spending_anything() {
        let mut run = a_run();
        run.plan_attempts = PLAN_ATTEMPTS;
        run.amendments = amend::MAX_AMENDMENTS;
        for question in [Question::CriterionUnprovable, Question::PlanNeedsChanging] {
            assert_eq!(
                route_question(&run, question),
                Route {
                    owner: Owner::User,
                    state: RunState::Escalated,
                    budget: Budget::None
                }
            );
        }
    }

    #[test]
    fn replanning_is_live_and_escalated_is_not() {
        assert!(RunState::Replanning.is_live());
        assert!(!RunState::Replanning.is_terminal());
        assert!(RunState::Escalated.is_terminal());
        assert!(!RunState::Escalated.is_live());
    }

    /* -------------------------------------------------------------- *
     * The roster tells the truth
     * -------------------------------------------------------------- */

    /// A member created for a plan has no process, and the roster has to say so.
    ///
    /// The live run showed twenty-one members `starting` while three `vibe-acp`
    /// processes existed. The roster was not lying about a state; it was
    /// reporting a state that did not exist.
    #[test]
    fn a_member_with_no_process_is_queued_not_starting() {
        let member = Member::new("w-t1", Role::Worker, "m");
        assert_eq!(member.state, MemberState::Queued);
        assert_eq!(member.state.label(), "queued");
        assert!(!member.state.is_live(), "a queued member has nothing to lose");
    }

    /// A turn in flight is `working`; a turn in flight with a request open is
    /// `waiting`; and the answer puts it back to `working`.
    #[test]
    fn a_blocked_member_reads_waiting_and_returns_to_working_on_the_answer() {
        let mut run = run_with_silent_worker(TaskState::Running, 0);
        let turn = Some(now());

        let mut live = vec![(
            "w-t1".to_string(),
            Live {
                blocked: false,
                turn_at: turn.clone(),
            },
        )];
        assert!(fold_live(&mut run, &live));
        assert_eq!(run.member("w-t1").unwrap().state, MemberState::Working);
        assert_eq!(run.member("w-t1").unwrap().turn_started_at, turn);

        // The gate holds a request open.
        live[0].1.blocked = true;
        assert!(fold_live(&mut run, &live));
        assert_eq!(run.member("w-t1").unwrap().state, MemberState::Waiting);
        assert!(
            run.member("w-t1").unwrap().state.is_live(),
            "a member blocked on a request is mid-turn, not gone"
        );

        // The gate answers.
        live[0].1.blocked = false;
        assert!(fold_live(&mut run, &live));
        assert_eq!(run.member("w-t1").unwrap().state, MemberState::Working);

        // Idempotent: a tick that changes nothing moves nothing.
        assert!(!fold_live(&mut run, &live));
    }

    /// The turn clock is the watch's, and it stops when the turn does.
    #[test]
    fn the_turn_clock_is_cleared_when_the_turn_ends() {
        let mut run = run_with_silent_worker(TaskState::Running, 0);
        let live = vec![(
            "w-t1".to_string(),
            Live {
                blocked: false,
                turn_at: Some(now()),
            },
        )];
        fold_live(&mut run, &live);
        assert!(run.member("w-t1").unwrap().turn_started_at.is_some());

        let ended = vec![(
            "w-t1".to_string(),
            Live {
                blocked: false,
                turn_at: None,
            },
        )];
        assert!(fold_live(&mut run, &ended));
        assert_eq!(run.member("w-t1").unwrap().turn_started_at, None);
        assert_eq!(
            run.member("w-t1").unwrap().state,
            MemberState::Working,
            "a member with a task keeps the state the engine gave it"
        );
    }

    /// A member the engine has already decided about is not moved by a turn
    /// clock. The wedge detector fails a member precisely because its turn is not
    /// answering, so a fold that put it back to `working` would erase the one word
    /// the run died on.
    #[test]
    fn a_failed_member_is_not_put_back_to_working_by_its_turn_clock() {
        let mut run = run_with_silent_worker(TaskState::Running, 0);
        run.member_mut("w-t1").unwrap().state = MemberState::Failed;
        let live = vec![(
            "w-t1".to_string(),
            Live {
                blocked: false,
                turn_at: Some(now()),
            },
        )];
        fold_live(&mut run, &live);
        assert_eq!(run.member("w-t1").unwrap().state, MemberState::Failed);
    }

    /// A parked member is a decision too. `after_amendment` parks a lead that has
    /// answered and has nothing more to say, and a fold that put it back to
    /// `working` would make the wedge detector expect it to keep talking — the
    /// failure that cost the thirteenth live run.
    #[test]
    fn a_parked_member_is_not_put_back_to_working_by_a_turn_still_in_flight() {
        let mut run = run_with_silent_worker(TaskState::Running, 0);
        run.member_mut("w-t1").unwrap().state = MemberState::Parked;
        let live = vec![(
            "w-t1".to_string(),
            Live {
                blocked: false,
                turn_at: Some(now()),
            },
        )];
        fold_live(&mut run, &live);
        assert_eq!(run.member("w-t1").unwrap().state, MemberState::Parked);
    }

    /// A member left `waiting` by a gate that has answered is the one word the
    /// roster must never keep, even if its turn has already ended.
    #[test]
    fn an_answered_request_clears_waiting_even_with_no_turn_in_flight() {
        let mut run = run_with_silent_worker(TaskState::Running, 0);
        run.member_mut("w-t1").unwrap().state = MemberState::Waiting;
        let live = vec![(
            "w-t1".to_string(),
            Live {
                blocked: false,
                turn_at: None,
            },
        )];
        assert!(fold_live(&mut run, &live));
        assert_eq!(run.member("w-t1").unwrap().state, MemberState::Working);
    }

    /// A turn that ends leaves a span behind, because the clock it started from
    /// is cleared.
    ///
    /// This is the record the timeline draws from. `turn_started_at` is `None`
    /// between turns, so a finished run has no way to say that its verifier took
    /// twenty minutes where its worker took six — which is the one thing about a
    /// slow run worth knowing. The span is appended by the fold, because the fold
    /// is the only place that sees the transition and the drive loop is the only
    /// place that saves.
    #[test]
    fn a_turn_that_ends_leaves_a_span_with_the_moment_it_started() {
        let mut run = run_with_silent_worker(TaskState::Running, 0);
        let started = now();
        let live = vec![(
            "w-t1".to_string(),
            Live {
                blocked: false,
                turn_at: Some(started.clone()),
            },
        )];
        fold_live(&mut run, &live);
        assert!(
            run.member("w-t1").unwrap().spans.is_empty(),
            "a turn in flight is not a span yet"
        );

        let ended = vec![(
            "w-t1".to_string(),
            Live {
                blocked: false,
                turn_at: None,
            },
        )];
        fold_live(&mut run, &ended);

        let spans = &run.member("w-t1").unwrap().spans;
        assert_eq!(spans.len(), 1, "the finished turn is recorded");
        assert_eq!(spans[0].start, started, "the span starts when the turn did");
        assert!(
            spans[0].end >= spans[0].start,
            "the span ends at or after its start, never before it"
        );

        // Idempotent: a tick that changes nothing does not append a second span.
        fold_live(&mut run, &ended);
        assert_eq!(run.member("w-t1").unwrap().spans.len(), 1);
    }

    /// A turn that began and ended between two ticks still leaves a span.
    ///
    /// `turn_started` and `turn_ended` both run inside one prompt, so a turn
    /// shorter than the 400ms tick arrives at the fold as a new start rather than
    /// as an end. The old turn is over either way, and a fold that only watched
    /// for `None` would lose it.
    #[test]
    fn a_turn_that_ended_between_two_ticks_still_leaves_a_span() {
        let mut run = run_with_silent_worker(TaskState::Running, 0);
        // Distinct stamps rather than two `now()` calls: the clock has
        // millisecond resolution and both would land in the same one.
        let first = "2026-01-01T00:00:00.000Z".to_string();
        fold_live(
            &mut run,
            &[(
                "w-t1".to_string(),
                Live {
                    blocked: false,
                    turn_at: Some(first.clone()),
                },
            )],
        );

        // The next tick sees a different start, because the turn in between began
        // and ended without the loop looking.
        let second = "2026-01-01T00:00:01.000Z".to_string();
        fold_live(
            &mut run,
            &[(
                "w-t1".to_string(),
                Live {
                    blocked: false,
                    turn_at: Some(second.clone()),
                },
            )],
        );

        let spans = &run.member("w-t1").unwrap().spans;
        assert_eq!(spans.len(), 1, "the turn that ended is recorded");
        assert_eq!(spans[0].start, first);
        assert_eq!(run.member("w-t1").unwrap().turn_started_at, Some(second));
    }

    /// The span ring is bounded, and it keeps the newest.
    ///
    /// A member that takes twenty turns does not need twenty spans on a row
    /// fifteen pixels tall. The bound is what stops a long run's `run.json` from
    /// growing a span per turn forever, and keeping the newest is what makes the
    /// row describe the member's current state rather than its first hour.
    #[test]
    fn a_member_keeps_only_the_newest_spans() {
        let mut member = Member::new("w-t1", Role::Worker, "m");
        for index in 0..(super::super::SPANS_KEPT + 5) {
            member.push_span(format!("start-{index}"), format!("end-{index}"));
        }
        assert_eq!(member.spans.len(), super::super::SPANS_KEPT);
        assert_eq!(
            member.spans[0].start,
            format!("start-{}", 5),
            "the oldest went, not the newest"
        );
        assert_eq!(
            member.spans[member.spans.len() - 1].start,
            format!("start-{}", super::super::SPANS_KEPT + 4)
        );
    }

    /// A worker whose turn the engine ends reads `idle`, and the wedge detector
    /// leaves it alone.
    ///
    /// `Idle` is the turn being over, which is true of a member with a task as
    /// much as one without: a worker whose evidence has landed has finished its
    /// part, and the minutes a verifier takes are not the worker working. The
    /// wedge detector gates on `working`, so an idle member is not expected to
    /// talk — which is the point, because it has nothing left to say.
    #[test]
    fn a_worker_whose_turn_ends_reads_idle_and_is_not_failed_by_the_wedge() {
        let mut run = run_with_silent_worker(TaskState::Running, MEMBER_IDLE_LIMIT_SECS + 60);

        // The same member, still `working`, is a wedge: the silence is real and
        // the task is still running. This is the contrast that makes the next
        // assertion mean something.
        assert!(
            wedge(&run, &never_busy, &no_pulse).is_some(),
            "a silent worker mid-task is a wedge"
        );

        // The engine ends the turn because the evidence has landed.
        let state = state_after_turn(run.member("w-t1").unwrap())
            .expect("a working member is idled by the end of its turn");
        run.member_mut("w-t1").unwrap().state = state;
        assert_eq!(run.member("w-t1").unwrap().state, MemberState::Idle);

        assert!(
            wedge(&run, &never_busy, &no_pulse).is_none(),
            "an idle member is not expected to talk, so its silence is not a wedge"
        );
        std::fs::remove_dir_all(super::super::run_dir(&run.id)).ok();
    }

    /// A parked lead stays parked when its turn ends.
    ///
    /// `after_amendment` parks a lead that has answered and has nothing more to
    /// say. A cancel that put it back to `idle` would make the roster disagree
    /// with the engine about the same member — and `idle` is the weaker word, so
    /// the disagreement would be invisible until the run failed.
    #[test]
    fn a_parked_lead_stays_parked_when_its_turn_ends() {
        let mut lead = Member::new("lead", Role::Lead, "m");
        lead.state = MemberState::Parked;
        assert_eq!(state_after_turn(&lead), None);

        // And a lead that is still working is idled, because the turn is over.
        lead.state = MemberState::Working;
        assert_eq!(state_after_turn(&lead), Some(MemberState::Idle));
    }

    /// The engine's own decisions are not overwritten by the end of a turn.
    #[test]
    fn ending_a_turn_does_not_overwrite_a_member_the_engine_has_decided_about() {
        for decided in [
            MemberState::Parked,
            MemberState::Done,
            MemberState::Failed,
            MemberState::Lost,
        ] {
            let mut member = Member::new("w-t1", Role::Worker, "m");
            member.task = Some("t1".into());
            member.state = decided;
            assert_eq!(
                state_after_turn(&member),
                None,
                "{decided:?} is the engine's word, not the turn's"
            );
        }
    }

    /// A respawn is a new session, not a new member: the ledger continues, so the
    /// run's total never falls across a tick.
    ///
    /// The cancelled run shows what the old behaviour cost — `w-t1` fell from
    /// 1,189,646 input tokens to 461,743, and `budget.used_tokens` sums the
    /// survivors, so the run under-reported what it cost.
    #[test]
    fn a_respawned_member_keeps_its_earlier_spend_and_the_total_never_falls() {
        let mut run = run_with_silent_worker(TaskState::Running, 0);

        // The first session's ledger, as the drive loop last folded it.
        let mut ledger = Usage {
            input: 1_189_646,
            output: 9_000,
            cached: 1_000_000,
            turns: 1,
            last_input: 1_189_646,
            last_output: 9_000,
            last_cached: 1_000_000,
            ..Default::default()
        };
        fold_usage(&mut run, &[("w-t1".to_string(), ledger.clone())]);
        run.recompute_spend();
        let before = run.budget.used_tokens;
        assert_eq!(before, 1_189_646 + 9_000);

        // The process is replaced. The new session's counters start at zero, and
        // the carried `last_*` readings are what make its first reading a delta
        // rather than a correction.
        ledger.observe(461_743, 3_000, 400_000);
        assert_eq!(ledger.input, 1_189_646 + 461_743);

        fold_usage(&mut run, &[("w-t1".to_string(), ledger)]);
        run.recompute_spend();
        assert!(
            run.budget.used_tokens >= before,
            "a member's spend must never go backwards: {before} then {}",
            run.budget.used_tokens
        );
        assert_eq!(run.member("w-t1").unwrap().tokens_in, 1_189_646 + 461_743);
    }

    /* -------------------------------------------------------------- *
     * Phase 15 — the run remembers
     * -------------------------------------------------------------- */

    /// A finished run of `workspace`, with one failed task and its verdict.
    ///
    /// The shape is the observed run's: a one-wide first rank and a first task
    /// that owns 22 files, which is the fact its lead needed and did not have.
    fn past_run(id: &str, workspace: &str, goal: &str) -> Run {
        let mut run = Run::new(goal, workspace, Member::new("lead", Role::Lead, "m"));
        run.id = id.to_string();
        run.state = RunState::Failed;
        run.failure = Some("no task can make progress".into());

        let mut t1 = Task::new("t1", "the limiter", "do it", "cargo test rate_limit");
        t1.state = TaskState::Failed;
        t1.attempts = 3;
        t1.files = (0..22).map(|index| format!("src/f{index}.rs")).collect();
        t1.verdict = Some(Verdict {
            passed: false,
            reason: "the middleware does not handle a burst".into(),
            evidence: Vec::new(),
            command: None,
            base_exit: Some(1),
            change_exit: Some(0),
            failing_first: true,
            base_dirtied: false,
            at: now(),
        });
        let mut t2 = Task::new("t2", "the config", "do it", "cargo test config");
        t2.state = TaskState::Verified;
        t2.deps = vec!["t1".into()];
        run.tasks = vec![t1, t2];
        run
    }

    /// The first proof: a second run in the same workspace receives the first
    /// run's outcome in its lead brief.
    ///
    /// The brief is the only thing the lead reads before it plans, so "the run
    /// remembers" is exactly this assertion: the goal, the shape and the verdict
    /// that failed a task are in the text the next lead is handed.
    #[test]
    fn a_second_run_in_the_same_workspace_is_told_what_the_first_one_learned() {
        let first = past_run("r1", "/ws", "add rate limiting to the http client");
        let history = history::History::from_runs(vec![first], "/ws", Some("r2"));

        let brief = roles::lead_brief(
            "add caching to the http client",
            "/ws",
            "r2",
            "/tmp/inbox",
            "",
            &history.lead_section(),
        );

        assert!(
            brief.contains("add rate limiting to the http client"),
            "the last run's goal is in the brief: {brief}"
        );
        assert!(
            brief.contains("first rank 1 wide"),
            "the shape that would have warned this lead: {brief}"
        );
        assert!(
            brief.contains("the middleware does not handle a burst"),
            "the verdict that failed the task: {brief}"
        );

        // And a run in a different workspace is not this workspace's history.
        let elsewhere = history::History::from_runs(
            vec![past_run("r9", "/other", "a different repository")],
            "/ws",
            Some("r2"),
        );
        let other_brief = roles::lead_brief(
            "add caching",
            "/ws",
            "r2",
            "/tmp/inbox",
            "",
            &elsewhere.lead_section(),
        );
        assert!(
            !other_brief.contains("a different repository"),
            "another workspace's run is not this one's memory: {other_brief}"
        );
        assert!(
            !other_brief.contains("What the last runs of this workspace learned"),
            "with no history there is no section at all: {other_brief}"
        );
    }

    /// The second proof: a worker whose task touches a file with a recorded
    /// gotcha receives that gotcha, and a worker whose task touches nothing
    /// recorded receives none.
    ///
    /// The chain is the run's own: the history's gotchas, filtered by
    /// `gotchas_for` against the task's files, rendered by `gotcha_section` into
    /// the brief. `gotchas_for` was the function that existed and was called only
    /// from a test; this is the call the run makes.
    #[test]
    fn a_worker_is_told_the_gotcha_recorded_against_its_own_files_and_no_other() {
        let mut first = past_run("r1", "/ws", "add rate limiting");
        first.reflection = Some(Reflection {
            at: now(),
            summary: "one gotcha".into(),
            gotchas: vec![Gotcha {
                file: "src/rate.rs".into(),
                text: "the retry loop swallows 429s, so assert on the retry count".into(),
                source: "verdict t1".into(),
            }],
            degraded: None,
        });
        let history = history::History::from_runs(vec![first], "/ws", Some("r2"));

        let touching =
            Task::new("t1", "the limiter", "do it", "cargo test").with_files(&["src/rate.rs"]);
        let gotchas = gotchas_for(&history.gotchas(), &touching.files);
        let brief = roles::worker_brief(
            &touching,
            "",
            "r2",
            "/tmp/inbox",
            "abc",
            "(none)",
            "",
            &roles::gotcha_section(&gotchas),
        );
        assert!(
            brief.contains("the retry loop swallows 429s"),
            "the worker editing the file is told what it cost last time: {brief}"
        );
        assert!(brief.contains("src/rate.rs"), "{brief}");

        let elsewhere =
            Task::new("t2", "the config", "do it", "cargo test").with_files(&["src/config.rs"]);
        let none = gotchas_for(&history.gotchas(), &elsewhere.files);
        assert!(none.is_empty(), "nothing was recorded about src/config.rs");
        let quiet = roles::worker_brief(
            &elsewhere,
            "",
            "r2",
            "/tmp/inbox",
            "abc",
            "(none)",
            "",
            &roles::gotcha_section(&none),
        );
        assert!(
            !quiet.contains("What past runs learned"),
            "a worker with nothing recorded gets no section: {quiet}"
        );
        assert!(!quiet.contains("429"), "{quiet}");
    }

    /// The third proof: the plan review shows a per-role median once two runs
    /// exist.
    ///
    /// One run is an anecdote. The observed run's verifier model took 20.7
    /// minutes against the worker's 6, and a user choosing a verifier model
    /// should see that number before the run rather than after it — but only once
    /// there is a second run to call it a median.
    #[test]
    fn the_plan_review_shows_a_per_role_median_once_two_runs_exist() {
        let with_usage = |id: &str, verifier_secs: f64| {
            let mut run = past_run(id, "/ws", "add rate limiting");
            run.role_usage = vec![
                crate::swarm::RoleUsage {
                    role: Role::Worker,
                    turns: 1,
                    secs: 360.0,
                    tokens_in: 60_000,
                    tokens_out: 0,
                    cached: 0,
                },
                crate::swarm::RoleUsage {
                    role: Role::Verifier,
                    turns: 1,
                    secs: verifier_secs,
                    tokens_in: 25_000,
                    tokens_out: 0,
                    cached: 0,
                },
            ];
            run
        };
        let engine = Engine::default();
        let plan = five_deep_plan();

        // One run: the review has no median to show.
        let one = engine.rehearse_with(
            &plan,
            4,
            history::History::from_runs(vec![with_usage("r1", 1242.0)], "/ws", Some("r2")),
        );
        assert!(
            one.role_medians.is_empty(),
            "one run is not a median: {:?}",
            one.role_medians
        );

        // Two runs: the verifier's 20.7 minutes and its 6 are now a median.
        let two = engine.rehearse_with(
            &plan,
            4,
            history::History::from_runs(
                vec![with_usage("r1", 1242.0), with_usage("r2", 360.0)],
                "/ws",
                Some("r3"),
            ),
        );
        let verifier = two
            .role_medians
            .iter()
            .find(|median| median.role == Role::Verifier)
            .expect("the verifier's turn is the number the user is choosing");
        assert_eq!(verifier.runs, 2);
        assert_eq!(verifier.turn_secs, 801, "the middle of 360 and 1242");
        assert_eq!(verifier.fresh_tokens, 25_000);

        let worker = two
            .role_medians
            .iter()
            .find(|median| median.role == Role::Worker)
            .expect("the worker's turn is the comparison");
        assert_eq!(worker.turn_secs, 360);

        // The panel reads these by name, so the names are the contract.
        let body = serde_json::to_string(&two).unwrap();
        assert!(body.contains("\"roleMedians\""), "{body}");
        assert!(body.contains("\"turnSecs\":801"), "{body}");
    }

    /// The fourth proof, at the seam the run uses: the derived budget moves when
    /// the median moves.
    ///
    /// `schedule::rehearse` proves the arithmetic; this proves the review and the
    /// approval both read the measured figure rather than the constant. The
    /// observed run's task-pair was about 27 minutes, not the 10 the constant
    /// assumed, and at 27 the same five-deep plan gets a clock that covers it.
    #[test]
    fn the_budget_the_review_shows_and_the_run_is_given_follow_the_measured_median() {
        let engine = Engine::default();
        let plan = five_deep_plan();

        let assumed = engine.rehearse_with(&plan, 4, history::History::empty());
        assert!(!assumed.rehearsal.measured, "no history is a guess");
        assert_eq!(
            assumed.rehearsal.median_task_secs,
            schedule::DEFAULT_MEDIAN_TASK_SECS
        );
        assert_eq!(assumed.rehearsal.budget_wall_secs, 6000);

        // Two past runs whose tasks took 27 minutes each.
        let measured = 1620;
        let mut first = past_run("r1", "/ws", "add rate limiting");
        let mut second = past_run("r2", "/ws", "add caching");
        for run in [&mut first, &mut second] {
            for task in &mut run.tasks {
                task.started_at = Some("2026-01-01T00:00:00.000Z".into());
                task.ended_at = Some("2026-01-01T00:27:00.000Z".into());
            }
        }
        let history = history::History::from_runs(vec![first, second], "/ws", Some("r3"));
        assert_eq!(history.median_task_secs(), Some(measured));

        let review = engine.rehearse_with(&plan, 4, history.clone());
        assert!(review.rehearsal.measured, "a measurement says so");
        assert_eq!(review.rehearsal.median_task_secs, measured);
        assert_eq!(review.rehearsal.budget_wall_secs, 16200);
        assert!(
            review.rehearsal.budget_wall_secs > assumed.rehearsal.budget_wall_secs,
            "a slower workspace must get a longer clock"
        );

        // And the approval sets the same number the review promised.
        let run = Run::new("g", "/ws", Member::new("lead", Role::Lead, "m"));
        let running = start_running(&run, &plan, "abc123", 4, Some(&history));
        assert_eq!(
            running.budget.max_wall_secs,
            review.rehearsal.budget_wall_secs
        );
    }

    /// The per-role record survives the members, because it is stamped on the run.
    ///
    /// The plan review reads past runs from disk, where the roster's live spend is
    /// no longer being folded. A record derived from `members` at read time would
    /// be a second answer; this one is written once, when the run stops.
    #[test]
    fn a_finished_run_records_what_each_role_cost() {
        let mut run = Run::new("g", "/ws", Member::new("lead", Role::Lead, "m"));
        run.lead.turns = 1;
        run.lead.active_secs = 90.0;
        run.lead.tokens_in = 40_000;

        let mut worker = Member::new("w-t1", Role::Worker, "m");
        worker.turns = 2;
        worker.active_secs = 720.0;
        worker.tokens_in = 120_000;
        worker.tokens_out = 4_000;
        run.members.push(worker);

        let mut verifier = Member::new("v-t1", Role::Verifier, "m");
        verifier.turns = 1;
        verifier.active_secs = 1242.0;
        verifier.tokens_in = 1_989_387;
        run.members.push(verifier);

        // A member that never took a turn is not a measurement of zero.
        run.members.push(Member::new("v-t2", Role::Verifier, "m"));

        run.state = RunState::Done;
        run.finish();

        let worker = run
            .role_usage
            .iter()
            .find(|usage| usage.role == Role::Worker)
            .expect("the worker's turns are recorded");
        assert_eq!(worker.turns, 2);
        assert_eq!(worker.turn_secs(), Some(360), "720 seconds over two turns");
        assert_eq!(worker.turn_tokens(), Some(62_000));

        let verifier = run
            .role_usage
            .iter()
            .find(|usage| usage.role == Role::Verifier)
            .expect("the verifier's turn is the number the user needs");
        assert_eq!(verifier.turns, 1, "the member with no turn is not counted");
        assert_eq!(verifier.turn_secs(), Some(1242), "20.7 minutes against 6");

        // The reflector never ran, so it is absent rather than zero.
        assert!(run
            .role_usage
            .iter()
            .all(|usage| usage.role != Role::Reflector));

        // Stamped once: a second finish does not double it.
        run.finish();
        assert_eq!(run.role_usage.len(), 3);
    }
}
