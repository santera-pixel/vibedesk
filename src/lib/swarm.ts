/**
 * The swarm, as the panel sees it.
 *
 * These types mirror `src-tauri/src/swarm` field for field. Hand-written for the
 * same reason `capabilities.ts` is: the Rust side serialises with
 * `rename_all = "camelCase"` and the enums with `kebab-case`, and a generator
 * would invent fields the commands do not send. When a command changes, this
 * file changes with it.
 *
 * Nothing here calls the bridge. The namespace lives in `bridge.ts` with every
 * other command family, so there is still exactly one module that owns
 * `invoke`.
 */

/* ---------------------------------------------------------------- *
 * States
 * ---------------------------------------------------------------- */

/** Where a whole run is. Mirrors `RunState`. */
export type RunState =
  | 'planning'
  | 'awaiting-approval'
  | 'running'
  | 'integrating'
  | 'reflecting'
  | 'done'
  | 'failed'
  | 'replanning'
  | 'escalated'
  | 'cancelled'
  | 'interrupted';

/** Where one task is. Mirrors `TaskState`. */
export type TaskState =
  | 'blocked'
  | 'ready'
  | 'running'
  | 'verifying'
  | 'verified'
  | 'failed'
  | 'conflict';

/** What a member is for. Mirrors `Role`. */
export type Role = 'lead' | 'worker' | 'verifier' | 'reflector';

/** Where one member is. Mirrors `MemberState`. */
export type MemberState =
  | 'queued'
  | 'starting'
  | 'idle'
  | 'working'
  | 'parked'
  | 'waiting'
  | 'done'
  | 'failed'
  | 'lost';

/** What an entry on the board is. Mirrors `BoardKind`. */
export type BoardKind =
  | 'claim'
  | 'decision'
  | 'evidence'
  | 'verdict'
  | 'conflict'
  | 'escalation'
  | 'note';

/** What one line of a member's feed is. Mirrors `ActivityKind`. */
export type ActivityKind = 'tool-call' | 'tool-update' | 'message' | 'permission';

/** Where the integration branch is. Mirrors `IntegrationState`. */
export type IntegrationState = 'pending' | 'merging' | 'merged' | 'conflict' | 'failed';

/* ---------------------------------------------------------------- *
 * The run document
 * ---------------------------------------------------------------- */

export interface Verdict {
  passed: boolean;
  reason: string;
  evidence: string[];
  /**
   * The one command the verifier named as its failing-first test.
   *
   * The engine runs it itself, on both sides, so this is the command that was
   * actually executed rather than the one that was described.
   */
  command: string | null;
  /** What that command exited with on the base checkout. Non-zero is the point. */
  baseExit: number | null;
  /** What it exited with on the change. Zero is the point. */
  changeExit: number | null;
  /**
   * Whether the engine *observed* the command fail on the base and pass on the
   * change.
   *
   * This is the field that separates a test from an approval. A verdict that
   * passes without it is not earned, and the engine treats it as a failure.
   */
  failingFirst: boolean;
  /** Whether the verifier left the base worktree dirty. */
  baseDirtied: boolean;
  at: string;
}

export interface Task {
  id: string;
  title: string;
  detail: string;
  /** The files this task expects to touch. Two unordered tasks may not share one. */
  files: string[];
  /** Task ids that must be verified before this one may start. */
  deps: string[];
  /** The observable criterion a verifier tests. */
  acceptance: string;
  state: TaskState;
  owner: string | null;
  verifier: string | null;
  attempts: number;
  verdict: Verdict | null;
  branch: string | null;
  /**
   * The commit this task's work started from: the run's base with every
   * dependency already merged in. The base side of its failing-first proof.
   */
  baseCommit: string | null;
  worktree: string | null;
  startedAt: string | null;
  endedAt: string | null;
  /**
   * Why this task could not start, when its dependencies would not merge.
   *
   * A dependency conflict is not the task's fault: two branches that both
   * verified still disagree about a file, and only the lead can say which one
   * owns it. The task sits in `conflict` until the amendment lands.
   */
  conflict: string | null;
  /** How many times the lead has been asked to resolve this task's conflict. */
  conflictRepairs: number;
  /**
   * Files the worker committed that the plan did not grant it.
   *
   * Read from the branch by the engine, not reported by the worker. The verifier
   * fails the change for it, because a grant nothing checks is a suggestion.
   */
  outOfGrant: string[];
  /**
   * Files this task owns that its dependencies disagreed about.
   *
   * The engine kept the incoming dependency's version, so the tree starts from
   * that and making it right is this task's work.
   */
  reconcile: string[];
  /**
   * The skills this task's worker is told to load before it writes.
   *
   * Named by the lead, or filled by the engine from the ranking when the lead
   * named none. A name the machine does not have is refused by the plan gate.
   */
  skills: string[];
}

/**
 * One turn a member ran, from the moment it was asked for to the moment it
 * ended.
 *
 * The timeline draws these as spans on a wall clock. Nothing else in the
 * document says a verifier spent twenty minutes where its worker spent six.
 */
export interface TurnSpan {
  /** When the turn was asked for. */
  start: string;
  /** When the watch stopped reporting it. */
  end: string;
}

export interface Member {
  id: string;
  role: Role;
  model: string;
  /** The reasoning level the session reports it is on, when one was applied. */
  thinking: string | null;
  mode: string | null;
  state: MemberState;
  pid: number | null;
  sessionId: string | null;
  worktree: string | null;
  /** The task this member is on, when it is on one. */
  task: string | null;
  turns: number;
  tokensIn: number;
  tokensOut: number;
  /** Input tokens the provider served from its prompt cache. */
  cachedTokens: number;
  /** Seconds this member spent inside turns. */
  activeSecs: number;
  /** The context window in use at the last reading, and its size. */
  contextUsed: number;
  contextSize: number;
  parkedAt: string | null;
  /**
   * When the current turn was asked for, and `null` between turns.
   *
   * The panel needs a live elapsed time for a member that is working, and the
   * only honest source is the moment the turn began. `lastAt` is a different
   * clock: it moves when the member writes to the board, which is not when it
   * started.
   */
  turnStartedAt: string | null;
  /**
   * The turns this member has finished, oldest first.
   *
   * `turnStartedAt` is cleared when a turn ends, so a finished run has no record
   * of when a member's turns ran — and the timeline cannot draw a verifier row
   * from a field that is `null` between turns. This is that record.
   *
   * Bounded at twelve, newest kept: the timeline draws one row per member at
   * fifteen pixels, and twelve spans is the most that can each stay a mark you
   * can see and click.
   */
  spans: TurnSpan[];
  lastAt: string | null;
  error: string | null;
  /** The skills this member was told to load, in the order the brief named them. */
  skillsAssigned: string[];
  /**
   * The skills this member's own tool calls loaded.
   *
   * Observed by the engine from the member's `tool_call` notifications, not
   * reported by the member. This is the difference between a run that used its
   * skills and a run that was handed them.
   */
  skillsLoaded: string[];
  /**
   * What this member is doing right now, and what it just did.
   *
   * `current` is filled for every member; `recent` only for the one the panel
   * has selected, because the snapshot is polled every two seconds and a
   * twenty-one member run would otherwise carry four thousand items per poll.
   *
   * Absent when there is nothing to say — a member with no watch, which is a
   * member that never started, and every member of a run read back from disk.
   */
  activity?: Activity;
}

/**
 * One thing a member did, as its own stream reported it.
 *
 * Observed, not reported: every field comes from a notification the member
 * emitted, which is the same evidence the skill ledger is built from.
 */
export interface ActivityItem {
  at: string;
  kind: ActivityKind;
  /** The tool call's name, the member's opening words, or what was asked for. */
  title: string;
  /** `in-progress`, `completed`, `failed`, `said`, `pending` or `answered`. */
  status: string;
  /** The tool call this line is about, when it is about one. */
  id: string | null;
  /**
   * Tokens this member had spent when the line was recorded.
   *
   * A reading per line, not a total: the difference between two lines over the
   * time between them is a rate, and a rate is what says whether a member is
   * doing anything.
   */
  tokens: number;
}

/** What a member had spent at one moment of its current turn. */
export interface Reading {
  /** When the reading was taken, in the clock the panel reads. */
  at: string;
  /** Tokens this member had spent by then, both directions. */
  tokens: number;
}

/** What one member is doing, and — for the selected member — what it just did. */
export interface Activity {
  /** The tool call running right now, if any. */
  current: ActivityItem | null;
  /** The feed, newest first. Empty for every member but the selected one. */
  recent: ActivityItem[];
  /**
   * The token readings of the current turn, oldest first.
   *
   * `recent` is the selected member's alone, so a roster card has no series to
   * draw a rate from — and a card is where the question "is this one doing
   * anything" is asked, twenty-one times at once. Forty points, which is exactly
   * the sparkline's width in pixels.
   *
   * Empty between turns, deliberately: a parked member has no turn, so it has no
   * readings, so its card draws a flat line.
   */
  series: Reading[];
}

export interface Budget {
  maxTokens: number;
  usedTokens: number;
  /** Input tokens served from cache across the whole run. */
  cachedTokens: number;
  maxWallSecs: number;
  startedAt: string | null;
}

export interface Conflict {
  task: string;
  files: string[];
  detail: string;
}

export interface Integration {
  branch: string;
  base: string;
  state: IntegrationState;
  /** Task ids merged so far, in merge order. */
  merged: string[];
  conflict: Conflict | null;
  /** The command that proves the integrated result. */
  suite: string | null;
  suitePassed: boolean | null;
}

export interface Gotcha {
  file: string;
  text: string;
  /** What backs it: a verdict, a conflict, an escalation. */
  source: string;
}

export interface Reflection {
  at: string;
  summary: string;
  gotchas: Gotcha[];
  /** Set when the pass ran without session logs and read the board only. */
  degraded: string | null;
}

export interface Run {
  id: string;
  goal: string;
  workspace: string;
  state: RunState;
  createdAt: string;
  updatedAt: string;
  lead: Member;
  members: Member[];
  tasks: Task[];
  budget: Budget;
  integration: Integration;
  /** How many tasks may hold a worker slot at once. */
  concurrency: number;
  reflection: Reflection | null;
  failure: string | null;
  rehearsal: boolean;
  /** When the run stopped. Null while it is still going. */
  endedAt: string | null;
  /**
   * The skill catalogue this run planned against.
   *
   * Stored on the run rather than re-read by the panel: a catalogue read later
   * would drift as skills are installed, and the ledger would describe a run
   * that did not happen.
   */
  skills: Skill[];
  /**
   * Whether this run was handed the machine's method at all.
   *
   * Recorded on the run rather than re-read from the switch, because the switch
   * can be flipped while a run is in flight. It is also what tells the two empty
   * cases apart: no skills because the switch was off, and no skills because the
   * machine has none installed.
   */
  skillsEnabled: boolean;
  /** The skills that looked relevant to the goal, in rank order. */
  skillShortlist: SkillMatch[];
  /** Skills assigned to a task that no member ever loaded. */
  skillGaps: string[];
  /** Skills the reflection pass decided this machine is missing. */
  skillProposals: SkillProposal[];
  /**
   * Why this plan is accepted but should not be.
   *
   * On the run rather than recomputed here, because the panel must show what the
   * gate saw. A plan re-shaped later would describe a run that did not happen.
   */
  shapeWarnings: ShapeWarning[];
  /**
   * What each role cost, stamped once when the run stopped.
   *
   * Recorded on the run rather than derived from `members` here, because the plan
   * review reads past runs from disk where the roster's live spend is no longer
   * being folded. Empty on a run that is still going, and on a run written before
   * the field existed.
   */
  roleUsage: RoleUsage[];
}

/** One role's share of a run: how long its turns took and what they cost. */
export interface RoleUsage {
  role: Role;
  /** Turns this role took, summed over its members. */
  turns: number;
  /** Seconds spent inside those turns, summed. */
  secs: number;
  tokensIn: number;
  tokensOut: number;
}

/**
 * One installed skill, as the swarm sees it.
 *
 * `path` is the load-bearing field. A member's session is opened with no MCP
 * servers, so the mastery bundles — which arrive only through the `mcp_mastery`
 * tool — are not reachable from inside a run. The file is, so every skill is
 * loadable by reading its `SKILL.md`.
 */
export interface Skill {
  name: string;
  description: string;
  /** The `SKILL.md` this skill is defined by. */
  path: string;
  /** `project`, `user`, `plugin` or `mastery`. */
  scope: string;
  /** The plugin or mastery bundle it came from, when it came from one. */
  bundle: string | null;
  enabled: boolean;
}

/** One skill's relevance to a goal, with the terms that matched. */
export interface SkillMatch {
  name: string;
  score: number;
  why: string[];
}

/** A skill the reflection pass decided this machine is missing. */
export interface SkillProposal {
  name: string;
  description: string;
  body: string;
  /** Why the run needed it: the task and the thing that went wrong. */
  because: string;
  /** Where the run wrote it. Accepting copies it into the skill store. */
  path: string;
}

export interface RunSummary {
  id: string;
  goal: string;
  state: RunState;
  createdAt: string;
  tasks: number;
  verified: number;
  tokens: number;
  rehearsal: boolean;
}

/* ---------------------------------------------------------------- *
 * The board
 * ---------------------------------------------------------------- */

export interface BoardEntry {
  /** Assigned by the engine, monotonic, never reused. */
  seq: number;
  at: string;
  /** The member that wrote it. */
  from: string;
  /** Who it is for. Empty means everyone. */
  to: string[];
  kind: BoardKind;
  text: string;
  /** The task it concerns, when it concerns one. */
  task: string | null;
  /** Commands and their output lines, for evidence and verdicts. */
  evidence: string[];
}

/* ---------------------------------------------------------------- *
 * Preconditions
 * ---------------------------------------------------------------- */

export interface Precondition {
  key: string;
  label: string;
  ready: boolean;
  /** Why it is not ready. Empty when it is. */
  detail: string;
}

/* ---------------------------------------------------------------- *
 * The plan and the rehearsal
 * ---------------------------------------------------------------- */

export interface Plan {
  goal: string;
  tasks: Task[];
  /** The command that proves the integrated result, not one task. */
  integrationSuite: string | null;
  notes: string;
  /**
   * Skills the lead wrote for this run, because the machine lacks the method.
   *
   * Written into the run's own directory and handed to this run's members. Not
   * installed: keeping one is a click, through the same proposal path the
   * reflection pass uses.
   */
  skillsToWrite: SkillDraft[];
}

/** A skill a run authored, before it is a file. */
export interface SkillDraft {
  name: string;
  description: string;
  /** The `SKILL.md` body, without frontmatter. */
  body: string;
  /** Which task needed it, and why this machine did not have it. */
  because: string;
}

export interface PlanShape {
  /** Tasks in execution order. */
  order: string[];
  /** The longest chain of dependent tasks. No concurrency beats it. */
  criticalPath: string[];
  maxParallel: number;
  /**
   * How many tasks have no dependencies.
   *
   * The first rank is the only part of a plan that can start at once, so this is
   * the width the run actually begins at. `maxParallel` is the width it might
   * reach later, and the two are not the same claim: the observed ten-task plan
   * had a widest rank of six and a first rank of one, so its first six minutes
   * were a single worker and the 2.0 speedup hid it.
   */
  firstRankWidth: number;
  files: number;
}

/**
 * A reason a plan is accepted but should not be. Mirrors `ShapeWarning`.
 *
 * Not a violation: a violation refuses the plan and sends it back to the lead,
 * and a lead cannot make a genuinely sequential plan parallel without lying
 * about its dependencies. The plan review shows these beside the approve button,
 * which is the approve-anyway.
 */
export type ShapeWarning =
  | {
      kind: 'not-worth-it';
      speedup: number;
      serialSteps: number;
      parallelSteps: number;
    }
  | { kind: 'one-wide-first-rank'; task: string }
  | { kind: 'task-too-large'; task: string; files: number };

export interface Wave {
  index: number;
  tasks: string[];
}

export interface Rehearsal {
  waves: Wave[];
  criticalPath: string[];
  maxParallel: number;
  /** Wall-clock in task-units when run serially. */
  serialSteps: number;
  /** Wall-clock in task-units at the given concurrency. */
  parallelSteps: number;
  /** `serialSteps / parallelSteps`. Below 1.5 the swarm is not worth it. */
  speedup: number;
  /**
   * The plan's own wall-clock projection, in seconds.
   *
   * Shown beside the budget so a plan that cannot finish says so before it
   * starts: a projection longer than the budget is a run that will be failed
   * mid-task with every worktree discarded.
   */
  projectedWallSecs: number;
  /** The wall clock a run of this plan is given. */
  budgetWallSecs: number;
  /**
   * The seconds one step was costed at, and whether that number was measured.
   *
   * `measured` is false when no run of this workspace has finished a task yet,
   * and the figure is the assumed ten minutes. The review says which it is: a
   * projection built on a guess and a projection built on this machine's own
   * history are different claims, and the user is deciding whether to spend a run
   * on the second one.
   */
  medianTaskSecs: number;
  measured: boolean;
  /** A rough token figure. An estimate, labelled as one. */
  estimatedTokens: number;
  /** The same plan run as a single agent, for comparison. */
  singleAgentTokens: number;
}

/**
 * What one role's turns have actually cost in this workspace.
 *
 * Per turn rather than per run, because that is the unit a user is choosing
 * between: a verifier model that takes 20 minutes a turn is a different decision
 * from one that takes 6, whatever the run's total. Mirrors `history::RoleMedian`.
 */
export interface RoleMedian {
  role: Role;
  /** Median seconds one turn of this role took. */
  turnSecs: number;
  /** Median tokens one turn of this role cost, in and out. */
  turnTokens: number;
  /** How many runs contributed. Never below two: one observation is not a median. */
  runs: number;
}

export interface Violation {
  key: string;
  message: string;
}

export interface RehearsalView {
  violations: Violation[];
  shape: PlanShape;
  rehearsal: Rehearsal;
  concurrency: number;
  /**
   * What each role's turns have actually cost in this workspace.
   *
   * Empty until two runs have finished here. One observation is not a median, and
   * a review that showed a single run's number under that word would be
   * presenting an anecdote as a measurement.
   */
  roleMedians: RoleMedian[];
}

export interface RoleModels {
  lead: string;
  worker: string;
  verifier: string;
  /** Whether the verifier is genuinely a different model. */
  diverse: boolean;
  /** The reasoning level each role asks for. Empty means the session default. */
  thinking: ThinkingByRole;
}

/** A reasoning level per role. */
export interface ThinkingByRole {
  lead: string;
  worker: string;
  verifier: string;
}

/**
 * One model the agent will accept, and the levels it accepts with it.
 *
 * The levels are per model, not global: `glm-5-3` takes low/high/max and
 * `mistral-medium-3.5` takes only off/high. A panel that showed one list for
 * every model would offer a level the agent refuses for the model beside it.
 */
export interface ModelOption {
  /** The alias `session/set_config_option` takes. */
  value: string;
  name: string;
  /** The underlying model id, when the agent reports one. */
  detail: string | null;
  /** The levels this model accepts. Empty means the agent did not say. */
  thinking: string[];
}

/** Where the catalogue came from, so the panel can say which authority it read. */
export type CatalogSource = 'agent' | 'config' | 'merged';

export interface Catalog {
  models: ModelOption[];
  source: CatalogSource;
}

/** What `swarm_models` answers: the catalogue, the assignment, and the defaults. */
export interface CrewView {
  catalog: Catalog;
  picked: RoleModels | null;
  defaults: ThinkingByRole;
}

export interface SwarmSnapshot {
  run: Run | null;
  board: BoardEntry[];
  preconditions: Precondition[];
  /** Whether all six capabilities are on. The panel entry is dim without it. */
  capabilitiesAllOn: boolean;
  capabilitiesOn: number;
  capabilitiesTotal: number;
  history: RunSummary[];
  live: boolean;
  /**
   * Whether the live run is driven by another process.
   *
   * A run outlives the window that started it, so the panel can be showing a run
   * this process cannot stop or approve. The UI says so rather than offering a
   * button that cannot work.
   */
  external: boolean;
}

/* ---------------------------------------------------------------- *
 * Reading the state
 * ---------------------------------------------------------------- */

/** The tone a run's state paints in. */
export function runTone(state: RunState): 'live' | 'good' | 'bad' | 'idle' {
  switch (state) {
    case 'planning':
    case 'awaiting-approval':
    case 'running':
    case 'integrating':
    case 'reflecting':
    case 'replanning':
      return 'live';
    case 'done':
      return 'good';
    case 'failed':
    case 'cancelled':
      return 'bad';
    case 'escalated':
      return 'idle';
    case 'interrupted':
      return 'idle';
  }
}

/** The tone a task's state paints in. */
export function taskTone(state: TaskState): 'live' | 'good' | 'bad' | 'idle' {
  switch (state) {
    case 'running':
    case 'verifying':
      return 'live';
    case 'verified':
      return 'good';
    case 'failed':
    case 'conflict':
      return 'bad';
    case 'blocked':
    case 'ready':
      return 'idle';
  }
}

/**
 * What a task's state means, in one clause.
 *
 * Total over `TaskState` on purpose: a state the panel cannot describe is a state
 * the user sees as a bare word. `blocked` and `ready` are the pair that most needs
 * the distinction — one is waiting on a dependency, the other is waiting on a
 * slot, and they look identical in a list.
 */
export function describeTaskState(state: TaskState): string {
  switch (state) {
    case 'blocked':
      return 'waiting on a dependency';
    case 'ready':
      return 'waiting for a worker slot';
    case 'running':
      return 'a worker is implementing it';
    case 'verifying':
      return 'a verifier is trying to break it';
    case 'verified':
      return 'verified against the base commit';
    case 'failed':
      return 'failed its verifier';
    case 'conflict':
      return 'a branch would not merge — the files are named on the board';
  }
}

/**
 * Whether a task's work is on its branch, verdict or no verdict.
 *
 * Mirrors `TaskState::is_committed` in the engine, and exists for the same
 * reason: a dependent may build on a task the moment its commit exists, so
 * `verifying` counts and `running` does not.
 */
function isCommitted(state: TaskState): boolean {
  return state === 'verifying' || state === 'verified';
}

/**
 * What a task's state means, naming the dependency when there is one.
 *
 * `describeTaskState` is handed a state and nothing else, so all it can say about
 * a blocked task is that it is waiting on *a* dependency. The user reading a
 * blocked task wants the name: which task, and what that task is doing. That is
 * the difference between a plan that is stuck and a plan that is working, and it
 * is the one thing the state word cannot carry.
 *
 * The dependency named is the first one that has not committed, not the first one
 * listed. A task behind `t1` and `t2` where `t1` is verified is waiting on `t2`,
 * and naming `t1` would send the user to look at work that is already done.
 *
 * A dependency the plan no longer carries is named as missing rather than
 * dropped. A blocked task waiting on nothing is a plan that disagrees with
 * itself, and that is worth seeing rather than smoothing over.
 */
export function describeTask(task: Task, tasks: Task[]): string {
  if (task.state !== 'blocked') return describeTaskState(task.state);
  const byId = new Map(tasks.map((t) => [t.id, t]));
  const pending = task.deps.find((id) => {
    const dep = byId.get(id);
    return dep === undefined || !isCommitted(dep.state);
  });
  // No dependency left to wait on: the scheduler has not moved it yet, and
  // inventing a name here would be worse than the general sentence.
  if (pending === undefined) return describeTaskState('blocked');
  const dep = byId.get(pending);
  if (dep === undefined) return `waiting on ${pending}, which is not in the plan`;
  return `waiting on ${dep.id} — ${describeTaskState(dep.state)}`;
}

/** The tone a member's state paints in. */
export function memberTone(state: MemberState): 'live' | 'good' | 'bad' | 'idle' {
  switch (state) {
    case 'starting':
    case 'working':
    case 'waiting':
      return 'live';
    case 'done':
      return 'good';
    case 'failed':
    case 'lost':
      return 'bad';
    case 'queued':
    case 'idle':
    case 'parked':
      return 'idle';
  }
}

/** The tone a board entry paints in. */
export function boardKindTone(kind: BoardKind): 'live' | 'good' | 'bad' | 'idle' {
  switch (kind) {
    case 'verdict':
      return 'good';
    case 'conflict':
    case 'escalation':
      return 'bad';
    case 'decision':
    case 'evidence':
      return 'live';
    case 'claim':
    case 'note':
      return 'idle';
  }
}

/**
 * The tone a feed line is drawn in.
 *
 * Read from the status, not the kind: a tool call that is running and one that
 * failed are the same kind of line and mean opposite things, and the status is
 * the only field that tells them apart.
 */
export function activityTone(item: ActivityItem): 'live' | 'good' | 'bad' | 'idle' {
  switch (item.status) {
    case 'completed':
      return 'good';
    case 'failed':
      return 'bad';
    case 'in-progress':
    case 'pending':
      return 'live';
    default:
      return 'idle';
  }
}

/** Whether a run is doing work right now. */
export function isLive(state: RunState): boolean {
  return (
    state === 'planning' ||
    state === 'awaiting-approval' ||
    state === 'running' ||
    state === 'integrating' ||
    state === 'reflecting' ||
    state === 'replanning'
  );
}

/** Whether a run has stopped for good. */
export function isTerminal(state: RunState): boolean {
  return (
    state === 'done' ||
    state === 'failed' ||
    state === 'escalated' ||
    state === 'cancelled'
  );
}

/**
 * A run's state as a sentence, for the one-line strip in the conversation.
 *
 * The panel can afford `awaiting-approval` as a chip because the panel has room
 * to explain it. A strip that sits above the composer has one clause, so the
 * state has to arrive already phrased. Total over `RunState` on purpose: a new
 * state must fail to compile here rather than render as an empty gap.
 */
export function describeRunState(state: RunState): string {
  switch (state) {
    case 'planning':
      return 'the lead is planning';
    case 'awaiting-approval':
      return 'waiting for your approval';
    case 'running':
      return 'working';
    case 'integrating':
      return 'merging';
    case 'reflecting':
      return 'reflecting';
    case 'done':
      return 'done';
    case 'failed':
      return 'failed';
    case 'replanning':
      return 'the lead is re-planning';
    case 'escalated':
      return 'stopped for a decision';
    case 'cancelled':
      return 'stopped';
    case 'interrupted':
      return 'interrupted';
  }
}

/** A member's role, as the roster labels it. */
export function roleLabel(role: Role): string {
  switch (role) {
    case 'lead':
      return 'Lead';
    case 'worker':
      return 'Worker';
    case 'verifier':
      return 'Verifier';
    case 'reflector':
      return 'Reflector';
  }
}

/** A member's role, as one letter, for a dense roster row. */
export function roleGlyph(role: Role): string {
  switch (role) {
    case 'lead':
      return 'L';
    case 'worker':
      return 'W';
    case 'verifier':
      return 'V';
    case 'reflector':
      return 'R';
  }
}

/**
 * What a verdict is worth, in one word.
 *
 * `earned` is the only state that means the change was tested. `claimed` means
 * the verifier said it passed but the engine could not observe the failing-first
 * half, and `dirty` means the baseline it measured was one it had changed.
 */
export function verdictTone(verdict: Verdict): 'good' | 'warn' | 'bad' {
  if (verdict.baseDirtied) return 'bad';
  if (verdict.passed && verdict.failingFirst) return 'good';
  if (verdict.passed) return 'warn';
  return 'bad';
}

/** The two exit codes, as one line: `base 1 → change 0`. */
export function exitPair(verdict: Verdict): string {
  const base = verdict.baseExit === null ? '—' : String(verdict.baseExit);
  const change = verdict.changeExit === null ? '—' : String(verdict.changeExit);
  return `base ${base} → change ${change}`;
}

/** The critical path, as one line. */
export function criticalPathLabel(path: string[]): string {
  if (path.length === 0) return 'none';
  if (path.length === 1) return `${path[0]} alone`;
  return path.join(' → ');
}

/**
 * The speedup below which the swarm is not worth running.
 *
 * The same threshold `plan::MIN_SPEEDUP` applies on the Rust side, so the gate
 * and the panel cannot drift apart about what "worth it" means.
 */
export const MIN_SPEEDUP = 1.5;

/**
 * The floor under a run's wall clock, in seconds.
 *
 * Mirrors `schedule::MIN_WALL_SECS`: one hour, which is what the budget used to
 * be unconditionally. It is also the line a plan's own projection is measured
 * against, because a plan that needs more than the hour every run used to get is
 * a plan worth saying out loud before it spends anything.
 */
export const MIN_WALL_SECS = 60 * 60;

/**
 * Whether a rehearsal is worth running.
 *
 * The threshold is the one the plan document commits to: below 1.5× the swarm
 * spends more coordination than it saves, and the honest answer is a chat.
 */
export function worthRunning(rehearsal: Rehearsal): boolean {
  return rehearsal.speedup >= MIN_SPEEDUP;
}

/**
 * Whether the plan needs longer than the hour a run used to be given.
 *
 * The budget is derived from the projection, so "the projection exceeds the
 * budget" can never be true and a warning on that comparison is dead UI. What
 * the user needs to know is how long the plan will take, not whether it fits a
 * number computed from it: a 70-minute plan is a decision to make before
 * spending, whatever the budget says.
 */
export function outrunsTheHour(rehearsal: Rehearsal): boolean {
  return rehearsal.projectedWallSecs > MIN_WALL_SECS;
}

/**
 * How the projection was costed, and whether the number was measured.
 *
 * A projection built on a guess and one built on this machine's own history are
 * different claims, and the user is deciding whether to spend a run on the second
 * one. The assumed figure is `schedule::DEFAULT_MEDIAN_TASK_SECS`; the measured
 * one is the median of what tasks actually took in this workspace.
 */
export function perStepLabel(rehearsal: Rehearsal): string {
  const each = formatDuration(rehearsal.medianTaskSecs);
  return rehearsal.measured ? `${each} per step, measured here` : `${each} per step, assumed`;
}

/**
 * The per-role line the plan review shows, or null when nothing is measured.
 *
 * The observed run's verifier model took 20.7 minutes against the worker's 6, and
 * nothing anywhere said so. A user choosing a verifier model should see that
 * number before the run rather than after it, which is why this is on the review
 * and not only on the finished run.
 *
 * The count is the smallest number of runs behind any one role, because that is
 * the weakest claim the line makes: a role measured over two runs beside one
 * measured over five is a line whose least-supported figure is the two.
 *
 * Null rather than an empty string when there is nothing to say: a heading with
 * nothing under it reads as "the roles cost nothing", which is a different claim
 * from "no run here has finished twice".
 */
export function roleMedianLine(medians: RoleMedian[]): string | null {
  if (medians.length === 0) return null;
  const runs = Math.min(...medians.map((median) => median.runs));
  const parts = medians.map((median) => `${median.role} ${formatDuration(median.turnSecs)}`);
  return `per turn, median over ${runs} run${runs === 1 ? '' : 's'}: ${parts.join(' · ')}`;
}

/**
 * The one line the user reads for a shape warning. Mirrors `ShapeWarning::message`.
 *
 * Written as a fact about the plan, because the decision it informs is the
 * user's: the plan is accepted with this, not refused for it.
 */
export function shapeWarningMessage(warning: ShapeWarning): string {
  switch (warning.kind) {
    case 'not-worth-it':
      return (
        `this plan's best case is ${warning.speedup.toFixed(1)}× — ` +
        `${warning.serialSteps} steps serially, ${warning.parallelSteps} at its widest. ` +
        `Below ${MIN_SPEEDUP}× the coordination costs more than it saves, and one agent ` +
        `would finish sooner`
      );
    case 'one-wide-first-rank':
      return (
        `only ${warning.task} can start — the first rank is one task wide, so the run ` +
        `begins single-threaded however many workers it is given`
      );
    case 'task-too-large':
      return (
        `${warning.task} owns ${warning.files} files — a task that large is a rank of one ` +
        `wearing a task's name, and everything behind it waits for one worker`
      );
  }
}

/** A token figure, short enough for a chip. */
export function formatTokens(value: number): string {
  if (!Number.isFinite(value) || value <= 0) return '0';
  if (value < 1000) return String(Math.round(value));
  if (value < 1_000_000) return `${(value / 1000).toFixed(value < 10_000 ? 1 : 0)}k`;
  return `${(value / 1_000_000).toFixed(1)}M`;
}

/** A duration in seconds, short enough for a chip. */
export function formatDuration(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds <= 0) return '0s';
  if (seconds < 60) return `${Math.round(seconds)}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  return `${hours}h ${minutes % 60}m`;
}

/**
 * How long one task took, in seconds.
 *
 * A task that has not started has no duration, and a task that is still running
 * is measured to now — the graph is a live picture, not a report. A finished
 * task is measured to its own end, so the number stops moving when the work does.
 */
export function taskDuration(task: Task, now = Date.now()): number {
  if (!task.startedAt) return 0;
  const start = new Date(task.startedAt).getTime();
  if (Number.isNaN(start)) return 0;
  const end = task.endedAt ? new Date(task.endedAt).getTime() : now;
  if (Number.isNaN(end)) return 0;
  return Math.max(0, Math.round((end - start) / 1000));
}

/** How far through a run is, as a fraction. */
export function progress(run: Run): number {
  if (run.tasks.length === 0) return 0;
  const done = run.tasks.filter((t) => t.state === 'verified').length;
  return done / run.tasks.length;
}

/**
 * The tasks in dependency order, for the graph.
 *
 * Falls back to declaration order when the graph has a cycle: a panel that
 * renders nothing because the plan is invalid is a panel that cannot show the
 * user why.
 */
export function orderedTasks(tasks: Task[]): Task[] {
  const byId = new Map(tasks.map((t) => [t.id, t]));
  const placed = new Set<string>();
  const out: Task[] = [];

  const visit = (task: Task, seen: Set<string>) => {
    if (placed.has(task.id) || seen.has(task.id)) return;
    seen.add(task.id);
    for (const dep of task.deps) {
      const upstream = byId.get(dep);
      if (upstream) visit(upstream, seen);
    }
    placed.add(task.id);
    out.push(task);
  };

  for (const task of tasks) visit(task, new Set());
  return out;
}

/**
 * The edges of the dependency graph, as pairs.
 *
 * Only edges between tasks that exist: a plan with a dangling dependency is
 * refused before it runs, but the panel may be showing a plan the user is still
 * editing.
 */
export function edges(tasks: Task[]): Array<{ from: string; to: string }> {
  const ids = new Set(tasks.map((t) => t.id));
  const out: Array<{ from: string; to: string }> = [];
  for (const task of tasks) {
    for (const dep of task.deps) {
      if (ids.has(dep)) out.push({ from: dep, to: task.id });
    }
  }
  return out;
}
