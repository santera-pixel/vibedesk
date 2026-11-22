/**
 * The swarm, projected for the panel.
 *
 * `swarm.ts` owns the run document: the types the engine sends, and the
 * one-line readings of a single field. This file owns the *views* — the three
 * observability primitives the panel draws, derived from that document and from
 * nothing else:
 *
 *   roster   who is on the run, what each one is doing, and what it has spent
 *   threads  what they said to each other, grouped into exchanges
 *   graph    the dependency DAG, its critical path, and the lanes it ran in
 *   spend    the bill, split by member and by role
 *
 * Every function here is pure and total. That is the point: the panel is a
 * renderer, and a renderer that computes is a renderer that cannot be tested.
 * `tests/swarm-view.test.ts` drives these directly, with no DOM and no bridge.
 *
 * Nothing here invents a number. A figure that cannot be measured is `null` and
 * the panel says so, because a zero reads as a measurement and is not one.
 */
import {
  edges,
  isLive,
  memberTone,
  orderedTasks,
  roleLabel,
  taskDuration,
  taskTone,
  type ActivityItem,
  type BoardEntry,
  type BoardKind,
  type Member,
  type Reading,
  type Role,
  type Run,
  type Task,
} from './swarm.ts';

/* ------------------------------------------------------------------ *
 * Reading a timestamp
 * ------------------------------------------------------------------ */

/** Milliseconds for an ISO stamp, or `null` when it is missing or unreadable. */
function ms(iso: string | null | undefined): number | null {
  if (!iso) return null;
  const value = new Date(iso).getTime();
  return Number.isNaN(value) ? null : value;
}

/* ------------------------------------------------------------------ *
 * The roster
 * ------------------------------------------------------------------ */

export interface AgentSpend {
  /** Input plus output tokens. */
  tokens: number;
  /** Input tokens the provider served from its prompt cache. */
  cached: number;
  /** Seconds spent inside turns. */
  secs: number;
  turns: number;
  /** `cached / tokens`, clamped. Zero when nothing has been spent. */
  cacheRate: number;
}

export interface AgentContext {
  used: number;
  size: number;
  /** `used / size`, clamped. Zero when the size is unknown. */
  fraction: number;
}

/**
 * What one agent was told to load, and what it actually loaded.
 *
 * `missing` is the whole point of the pair. A member that was handed a skill and
 * never opened it is a member that worked from the brief alone, and the run's
 * claim that it used the method is then false. The engine observes the loads
 * from the member's own tool calls, so this is evidence rather than a report.
 */
export interface AgentSkills {
  /** Skills the brief named, in the order it named them. */
  assigned: string[];
  /** Skills this member's own tool calls opened. */
  loaded: string[];
  /** Assigned but never opened. */
  missing: string[];
}

export interface AgentView {
  member: Member;
  role: Role;
  tone: 'live' | 'good' | 'bad' | 'idle';
  /**
   * What this agent is doing, in one clause.
   *
   * This is the field the roster exists for. A state word says `working`; this
   * says `trying to break t2`, which is the thing the user actually wants to
   * know and cannot get from any single field of the run document.
   */
  doing: string;
  spend: AgentSpend;
  context: AgentContext;
  /** The last thing this agent put on the board, or null if it has said nothing. */
  lastLine: BoardEntry | null;
  /** How many board entries this agent wrote. */
  lines: number;
  /** The task it is on, when it is on one. */
  task: Task | null;
  /** Whether it is suspended: alive, holding its session, spending nothing. */
  parked: boolean;
  /** The method it was handed and the method it opened. */
  skills: AgentSkills;
}

export interface RosterGroup {
  /** The task the group is on, or null for members not yet assigned one. */
  task: Task | null;
  agents: AgentView[];
}

export interface RosterView {
  lead: AgentView;
  /** Workers and verifiers, grouped by task, in dependency order. */
  groups: RosterGroup[];
  /** The reflector, which is a stage of the run rather than a task. */
  reflector: AgentView | null;
  /** Every agent, lead first, then groups in order, then the reflector. */
  all: AgentView[];
  /** How many have no process yet: the scheduler has not given them a slot. */
  queued: number;
  /** How many are inside a turn right now. */
  working: number;
  /** How many are alive and between turns: a healthy run's normal shape. */
  idle: number;
  /** How many are suspended and spending nothing. */
  parked: number;
  /** How many have failed or lost their process. */
  broken: number;
}

/**
 * What one agent is doing, in one clause.
 *
 * Total over `MemberState` and over `Role`, so a state the panel cannot describe
 * fails to compile rather than rendering as a bare word. The lead is described
 * by the run's stage rather than by its own state, because a lead inside a turn is that turn
 * and a lead between turns is the stage it is driving. The alternative — the
 * lead's own word — told the user nothing for the length of a run.
 */
export function describeAgent(member: Member, task: Task | null, run: Run): string {
  switch (member.state) {
    case 'queued':
      // No process yet. The scheduler has not given it a slot, so there is
      // nothing to describe but the wait — and saying "implementing t1" here is
      // the lie the state word exists to stop.
      return 'waiting for a slot';
    case 'starting':
      return 'opening a session';
    case 'parked':
      // A member that has never opened a session is not parked, it is waiting to
      // be spawned. The two look identical in the state word and mean opposite
      // things: one is holding a session it is not spending on, the other has
      // nothing to hold.
      return member.sessionId === null && member.turns === 0
        ? 'not started yet'
        : 'parked, spending nothing';
    case 'lost':
      return 'lost its process';
    case 'failed':
      return 'failed';
    case 'done':
      return member.role === 'reflector' ? 'wrote the reflection' : 'finished';
    case 'idle':
      // A worker between turns has nothing else to say here; a lead is described
      // by the stage it is driving, which is the same sentence it gets mid-turn.
      if (member.role !== 'lead') return 'idle';
      break;
    case 'waiting':
      // Blocked on a permission or an elicitation the gate has not answered.
      // Not "waiting on t1": the task is not what is holding it up, and the user
      // reading this is trying to find out why nothing is moving.
      return 'blocked on a request';
    case 'working':
      break;
  }

  switch (member.role) {
    case 'lead':
      switch (run.state) {
        case 'planning':
          return 'planning the run';
        case 'replanning':
          return 're-planning after a refusal';
        case 'awaiting-approval':
          return 'waiting for your approval';
        case 'integrating':
          return 'merging the branches';
        case 'reflecting':
          return 'reading the run back';
        case 'running':
          return 'coordinating the workers';
        case 'done':
          return 'finished';
        case 'failed':
          return 'failed';
        case 'escalated':
          return 'stopped for a decision';
        case 'cancelled':
          return 'stopped';
        case 'interrupted':
          return 'interrupted';
      }
    case 'worker':
      return task ? `implementing ${task.id}` : 'implementing';
    case 'verifier':
      return task ? `trying to break ${task.id}` : 'trying to break a change';
    case 'reflector':
      return 'reading the run back';
  }
}

function spendOf(member: Member): AgentSpend {
  const tokens = Math.max(0, member.tokensIn) + Math.max(0, member.tokensOut);
  const cached = Math.max(0, member.cachedTokens);
  return {
    tokens,
    cached,
    secs: Math.max(0, member.activeSecs),
    turns: Math.max(0, member.turns),
    cacheRate: tokens > 0 ? Math.min(1, cached / tokens) : 0,
  };
}

function contextOf(member: Member): AgentContext {
  const used = Math.max(0, member.contextUsed);
  const size = Math.max(0, member.contextSize);
  return { used, size, fraction: size > 0 ? Math.min(1, used / size) : 0 };
}

/**
 * The skills a member was handed, the ones it opened, and the gap.
 *
 * Order is the brief's order for `assigned` and the member's own order for
 * `loaded`, because both are sequences the user can compare against the brief
 * and against the transcript. `missing` is derived, never stored: a stored gap
 * would go stale the moment the member loaded the skill late.
 */
export function skillsOf(member: Member): AgentSkills {
  const assigned = member.skillsAssigned ?? [];
  const loaded = member.skillsLoaded ?? [];
  const opened = new Set(loaded);
  return {
    assigned,
    loaded,
    missing: assigned.filter((name) => !opened.has(name)),
  };
}

/**
 * How much of the plan ran on the machine's method.
 *
 * The ledger's job is to make a run comparable to the next one, and the one
 * comparison a skill plane creates is method against instinct: two runs of the
 * same goal, one handed the catalogue and one not. `assigned` counts tasks whose
 * brief named a skill; `opened` counts the tasks where somebody on the task
 * actually read one. The gap between them is the number worth watching, because
 * a run that assigns skills and never opens them has paid the tokens of a
 * longer brief for nothing.
 */
export interface MethodCoverage {
  /** Tasks in the plan. */
  tasks: number;
  /** Tasks whose brief named at least one skill. */
  assigned: number;
  /** Tasks where a member on the task opened at least one of its skills. */
  opened: number;
  /** Skill assignments no member on the task opened. */
  unread: number;
}

export function methodCoverage(run: Run): MethodCoverage {
  const opened = new Map<string, Set<string>>();
  for (const member of run.members) {
    if (!member.task) continue;
    const set = opened.get(member.task) ?? new Set<string>();
    for (const name of member.skillsLoaded ?? []) set.add(name);
    opened.set(member.task, set);
  }

  let assigned = 0;
  let withOpen = 0;
  let unread = 0;
  for (const task of run.tasks) {
    const skills = task.skills ?? [];
    if (skills.length === 0) continue;
    // A task nobody has been spawned on has not been worked, so it cannot have
    // skipped a skill. Counting it would report every assignment as unread for
    // the whole of the approval wait, which is the one moment the user is
    // reading the plan.
    if (!opened.has(task.id)) continue;
    assigned += 1;
    const read = opened.get(task.id) ?? new Set<string>();
    const hit = skills.filter((name) => read.has(name)).length;
    if (hit > 0) withOpen += 1;
    unread += skills.length - hit;
  }

  return { tasks: run.tasks.length, assigned, opened: withOpen, unread };
}

/**
 * One agent, projected.
 *
 * `lastLine` is the newest board entry the agent wrote, not the newest entry
 * about it: the roster answers "what is this agent doing", and the most direct
 * evidence of that is the last thing it said.
 */
export function agentView(
  member: Member,
  run: Run,
  board: BoardEntry[],
): AgentView {
  const task = member.task ? (run.tasks.find((t) => t.id === member.task) ?? null) : null;
  const mine = board.filter((entry) => entry.from === member.id);
  return {
    member,
    role: member.role,
    tone: memberTone(member.state),
    doing: describeAgent(member, task, run),
    spend: spendOf(member),
    context: contextOf(member),
    lastLine: mine.length > 0 ? (mine[mine.length - 1] ?? null) : null,
    lines: mine.length,
    task,
    parked: member.state === 'parked',
    skills: skillsOf(member),
  };
}

/**
 * Who is on the run.
 *
 * The lead is not a row in the list, it is the head of it: it owns the run's
 * acceptance criterion and it is the only member that outlives a task. Workers
 * and verifiers hang off it grouped by the task they are on, so the shape of the
 * answer to "who is doing what" is the shape of the tree. The reflector is a
 * stage rather than a task, so it sits at the end where the run's last step
 * belongs.
 *
 * Groups follow dependency order, not arrival order: a roster that reshuffles
 * every time a worker is spawned is a roster the user has to re-read.
 */
/**
 * Live member processes: the lead and every member with a pid. Shown beside the
 * worker ceiling, because `concurrency` bounds workers, not processes — a task
 * being verified holds a live verifier and does not count against the ceiling.
 */
export function liveProcesses(run: Run): number {
  const alive = (member: Member) => member.pid != null;
  return run.members.filter(alive).length + (alive(run.lead) ? 1 : 0);
}

export function rosterView(run: Run, board: BoardEntry[]): RosterView {
  const lead = agentView(run.lead, run, board);
  const reflectorMember = run.members.find((member) => member.role === 'reflector') ?? null;
  const reflector = reflectorMember ? agentView(reflectorMember, run, board) : null;

  const order = new Map(orderedTasks(run.tasks).map((task, index) => [task.id, index]));
  const groups = new Map<string, AgentView[]>();

  for (const member of run.members) {
    if (member.role === 'reflector') continue;
    const key = member.task ?? '';
    const list = groups.get(key) ?? [];
    list.push(agentView(member, run, board));
    groups.set(key, list);
  }

  const projected: RosterGroup[] = [...groups.entries()]
    .map(([taskId, agents]) => ({
      task: taskId ? (run.tasks.find((t) => t.id === taskId) ?? null) : null,
      agents,
    }))
    .sort((a, b) => {
      // Unassigned members last: they are the ones with nothing to show yet.
      if (!a.task) return 1;
      if (!b.task) return -1;
      return (order.get(a.task.id) ?? 0) - (order.get(b.task.id) ?? 0);
    });

  const all = [lead, ...projected.flatMap((group) => group.agents)];
  if (reflector) all.push(reflector);

  return {
    lead,
    groups: projected,
    reflector,
    all,
    // The four counts are mutually exclusive by construction, and that is worth
    // stating because it is what makes them readable as a breakdown. Each is read
    // from `member.state` — `broken` is the tone of the two states that are bad —
    // and no state is two of them. A queued member in particular is not parked:
    // parking is something a member with a session does, and a member with no
    // process has nothing to hold. Before this count existed, a queued member was
    // in none of the three, so a roster of twelve waiting on slots read as a
    // roster of four.
    //
    // They do not cover every state, and are not meant to: `starting`, `waiting`
    // and `done` are none of these five, and the roster's own total is printed
    // beside them for exactly that reason. `idle` is counted because it is the
    // ordinary state of a member between turns, and a healthy roster of four that
    // read "0 working" looked like four that had stopped.
    queued: all.filter((agent) => agent.member.state === 'queued').length,
    working: all.filter((agent) => agent.member.state === 'working').length,
    idle: all.filter((agent) => agent.member.state === 'idle').length,
    parked: all.filter((agent) => agent.parked).length,
    broken: all.filter((agent) => agent.tone === 'bad').length,
  };
}

/* ------------------------------------------------------------------ *
 * The board, as threads
 * ------------------------------------------------------------------ */

/**
 * One disagreement and everything that followed it.
 *
 * The board is a transcript, and a transcript read top to bottom loses the one
 * thing worth reading: a verifier rejected a change, the lead decided who pays
 * for it, and the worker retried. Those are three entries of three different
 * kinds, and nothing in the document links them. An exchange is that link,
 * derived from order alone: an opener is an escalation or a conflict, and
 * everything after it belongs to it until the next opener.
 */
export interface Exchange {
  /** The entry that opened it: an escalation or a conflict. */
  opener: BoardEntry;
  /** What followed, oldest first. */
  replies: BoardEntry[];
  /** Whether a decision or a verdict landed in the replies. */
  resolved: boolean;
  /** Who the opener was addressed to, joined. Empty when it was addressed to all. */
  owner: string;
}

/**
 * One thing to draw, in order.
 *
 * A thread is not a flat list of entries: an escalation and the decision that
 * answered it are one object, and drawing them as two siblings loses the only
 * relationship on the board that matters. A block is either a whole exchange or
 * a lone entry, and the projection decides which so the panel does not have to.
 */
export type Block =
  | { kind: 'exchange'; exchange: Exchange }
  | { kind: 'entry'; entry: BoardEntry };

export interface Thread {
  /** The task this thread is about, or null for run-wide entries. */
  task: string | null;
  /** Every entry the filter kept, oldest first. */
  entries: BoardEntry[];
  /** The exchanges inside it, oldest first. */
  exchanges: Exchange[];
  /** The render order: exchanges and lone entries, interleaved as they happened. */
  blocks: Block[];
  /** The kinds present, so the panel can say what a thread is made of. */
  kinds: BoardKind[];
  /** The members who wrote in it, in first-appearance order. */
  authors: string[];
  /** The newest entry, for sorting. */
  lastAt: string;
}

/** The kinds that open an exchange. */
const OPENERS: ReadonlySet<BoardKind> = new Set<BoardKind>(['escalation', 'conflict']);

/** The kinds that close one. */
const CLOSERS: ReadonlySet<BoardKind> = new Set<BoardKind>(['decision', 'verdict']);

/**
 * The board, grouped by task and split into exchanges.
 *
 * `filter` narrows what is shown, and it keeps an exchange whole: filtering to
 * problems shows each problem *and* what answered it, because a problem with its
 * answer cut off is a worse answer than no filter at all. A lone entry that the
 * filter drops is dropped; an entry inside a kept exchange is kept with it.
 *
 * Threads are ordered by dependency, then run-wide entries last: a thread about
 * `t3` is only meaningful after `t1` and `t2`, and the run-wide entries are the
 * ones that frame all of them.
 */
export function threads(
  board: BoardEntry[],
  run: Run,
  filter: BoardFilter = 'all',
): Thread[] {
  const order = new Map(orderedTasks(run.tasks).map((task, index) => [task.id, index]));
  // The lead scopes a global decision to `"run"` rather than to nothing, so a
  // scope that names no task in the plan is run-wide. Without this the panel
  // would show a thread headed `run` and sort it as if it were the first task.
  const scope = (id: string | null): string =>
    id && order.has(id) ? id : '';
  const byTask = new Map<string, BoardEntry[]>();
  for (const entry of board) {
    const key = scope(entry.task);
    const list = byTask.get(key) ?? [];
    list.push(entry);
    byTask.set(key, list);
  }

  const out: Thread[] = [];
  for (const [taskId, entries] of byTask) {
    // Exchanges are built from the whole thread, so a filter cannot change what
    // an exchange is, only whether it is shown.
    const exchanges: Exchange[] = [];
    const ownerOf = new Map<number, Exchange>();
    let open: Exchange | null = null;
    for (const entry of entries) {
      if (OPENERS.has(entry.kind)) {
        open = { opener: entry, replies: [], resolved: false, owner: entry.to.join(', ') };
        exchanges.push(open);
        ownerOf.set(entry.seq, open);
        continue;
      }
      if (open) {
        open.replies.push(entry);
        ownerOf.set(entry.seq, open);
      }
    }
    for (const exchange of exchanges) {
      exchange.resolved = exchange.replies.some((reply) => CLOSERS.has(reply.kind));
    }

    const kept = new Set<number>();
    for (const entry of entries) {
      if (!matchesFilter(entry, filter)) continue;
      const exchange = ownerOf.get(entry.seq);
      if (exchange) {
        kept.add(exchange.opener.seq);
        for (const reply of exchange.replies) kept.add(reply.seq);
      } else {
        kept.add(entry.seq);
      }
    }
    if (kept.size === 0) continue;

    const shown = entries.filter((entry) => kept.has(entry.seq));
    const blocks: Block[] = [];
    const emitted = new Set<number>();
    for (const entry of shown) {
      const exchange = ownerOf.get(entry.seq);
      if (exchange) {
        if (emitted.has(exchange.opener.seq)) continue;
        emitted.add(exchange.opener.seq);
        blocks.push({ kind: 'exchange', exchange });
      } else {
        blocks.push({ kind: 'entry', entry });
      }
    }

    const kinds: BoardKind[] = [];
    const authors: string[] = [];
    for (const entry of shown) {
      if (!kinds.includes(entry.kind)) kinds.push(entry.kind);
      if (!authors.includes(entry.from)) authors.push(entry.from);
    }

    out.push({
      task: taskId || null,
      entries: shown,
      exchanges: exchanges.filter((exchange) => kept.has(exchange.opener.seq)),
      blocks,
      kinds,
      authors,
      lastAt: shown[shown.length - 1]?.at ?? '',
    });
  }

  return out.sort((a, b) => {
    if (!a.task) return 1;
    if (!b.task) return -1;
    return (order.get(a.task) ?? 0) - (order.get(b.task) ?? 0);
  });
}

/** What the board is filtered to. */
export type BoardFilter = 'all' | 'problems' | 'decisions' | 'verdicts' | 'evidence';

/** Whether an entry survives a filter. Total over `BoardFilter`. */
export function matchesFilter(entry: BoardEntry, filter: BoardFilter): boolean {
  switch (filter) {
    case 'all':
      return true;
    case 'problems':
      return entry.kind === 'escalation' || entry.kind === 'conflict';
    case 'decisions':
      return entry.kind === 'decision';
    case 'verdicts':
      return entry.kind === 'verdict';
    case 'evidence':
      return entry.kind === 'evidence';
  }
}

/**
 * How many entries the snapshot dropped from the front of the board.
 *
 * The engine sends a tail, not the whole transcript, so a long run's board
 * arrives already cut. The sequence numbers are monotonic and never reused, so
 * the first one that arrived says exactly how many are missing. Without this the
 * panel would show a partial transcript as if it were the whole one, which is
 * the one thing a debate log must not do.
 */
export function droppedBefore(board: BoardEntry[]): number {
  const first = board[0];
  return first ? Math.max(0, first.seq - 1) : 0;
}

/** How many entries a filter would show, for the chip's count. */
export function countFilter(board: BoardEntry[], filter: BoardFilter): number {
  if (filter === 'all') return board.length;
  return board.filter((entry) => matchesFilter(entry, filter)).length;
}

/* ------------------------------------------------------------------ *
 * The graph
 * ------------------------------------------------------------------ */

export const NODE_W = 132;
export const NODE_H = 46;
export const GAP_X = 44;
export const GAP_Y = 12;
export const PAD = 10;

export interface GraphNode {
  task: Task;
  /** The longest chain of dependencies behind it. Its column. */
  depth: number;
  /** Its row inside the column. */
  row: number;
  x: number;
  y: number;
  tone: 'live' | 'good' | 'bad' | 'idle';
  /** Seconds, measured to now for a live task and to its own end for a finished one. */
  duration: number;
  /** Whether it lies on the critical path. */
  critical: boolean;
}

export interface GraphLink {
  from: string;
  to: string;
  /** Whether the dependent is live, so the edge animates. */
  live: boolean;
  /** Whether both ends are on the critical path. */
  critical: boolean;
}

export interface Wave {
  index: number;
  tasks: string[];
}

export interface LaneSpan {
  task: string;
  /** Milliseconds since the epoch. */
  start: number;
  end: number;
  tone: 'live' | 'good' | 'bad' | 'idle';
}

export interface Lane {
  index: number;
  spans: LaneSpan[];
}

export interface GraphView {
  nodes: GraphNode[];
  links: GraphLink[];
  /** Tasks grouped by depth: what could run at once, in order. */
  waves: Wave[];
  /** The longest chain of dependent tasks, weighted by duration. */
  criticalPath: string[];
  /** The concurrency lanes the run actually used, packed greedily. */
  lanes: Lane[];
  /** The run's wall-clock window in milliseconds, or null when nothing started. */
  window: { start: number; end: number } | null;
  width: number;
  height: number;
}

/**
 * The longest chain of dependent tasks.
 *
 * Weighted by each task's duration, or by one second when it has none yet. A
 * chain of three tasks that each took a minute is a longer path than a chain of
 * five that each took a second, and only the first one is the floor on
 * wall-clock. Counting tasks instead would name the wrong path on any run where
 * the work is unevenly sized, which is every real run.
 */
export function criticalPath(tasks: Task[], now = Date.now()): string[] {
  const byId = new Map(tasks.map((task) => [task.id, task]));
  const best = new Map<string, { weight: number; path: string[] }>();

  const visit = (id: string, seen: Set<string>): { weight: number; path: string[] } => {
    const known = best.get(id);
    if (known) return known;
    const task = byId.get(id);
    if (!task || seen.has(id)) return { weight: 0, path: [] };
    seen.add(id);

    const own = Math.max(1, taskDuration(task, now));
    let heaviest = { weight: 0, path: [] as string[] };
    for (const dep of task.deps) {
      const upstream = visit(dep, seen);
      if (upstream.weight > heaviest.weight) heaviest = upstream;
    }
    const result = { weight: own + heaviest.weight, path: [...heaviest.path, id] };
    best.set(id, result);
    return result;
  };

  let winner = { weight: 0, path: [] as string[] };
  for (const task of tasks) {
    const candidate = visit(task.id, new Set());
    if (candidate.weight > winner.weight) winner = candidate;
  }
  return winner.path;
}

/**
 * The lanes the run actually used.
 *
 * Greedy interval packing over `[startedAt, endedAt]`: a task goes in the first
 * lane whose last span has already ended. That is the same rule a scheduler
 * uses, so the picture is the schedule rather than a drawing of it, and the
 * number of lanes is the concurrency the run really achieved — which is often
 * lower than the concurrency it was configured with, and that gap is the thing
 * worth seeing.
 */
export function lanes(tasks: Task[], now = Date.now()): Lane[] {
  const spans = tasks
    .map((task) => {
      const start = ms(task.startedAt);
      if (start === null) return null;
      const end = ms(task.endedAt) ?? now;
      return { task, start, end: Math.max(start, end) };
    })
    .filter((span): span is { task: Task; start: number; end: number } => span !== null)
    .sort((a, b) => a.start - b.start || a.end - b.end);

  const packed: Array<{ end: number; spans: LaneSpan[] }> = [];
  for (const span of spans) {
    let lane = packed.find((candidate) => candidate.end <= span.start);
    if (!lane) {
      lane = { end: 0, spans: [] };
      packed.push(lane);
    }
    lane.spans.push({
      task: span.task.id,
      start: span.start,
      end: span.end,
      tone: taskTone(span.task.state),
    });
    lane.end = Math.max(lane.end, span.end);
  }

  return packed.map((lane, index) => ({ index, spans: lane.spans }));
}

/**
 * The dependency DAG, laid out and measured.
 *
 * Laid out by depth, not by declaration order: a task's column is the longest
 * chain of dependencies behind it, so an edge always points right and the
 * critical path is the widest row of the picture. That is the one property worth
 * drawing — it is the floor on wall-clock, and no amount of concurrency beats it.
 */
export function graphView(run: Run, now = Date.now()): GraphView {
  const tasks = run.tasks;
  const byId = new Map(tasks.map((task) => [task.id, task]));
  const depth = new Map<string, number>();

  const visit = (id: string, seen: Set<string>): number => {
    const known = depth.get(id);
    if (known !== undefined) return known;
    if (seen.has(id)) return 0;
    seen.add(id);
    const task = byId.get(id);
    if (!task) return 0;
    let value = 0;
    for (const dep of task.deps) value = Math.max(value, visit(dep, seen) + 1);
    depth.set(id, value);
    return value;
  };
  for (const task of tasks) visit(task.id, new Set());

  const columns = new Map<number, Task[]>();
  for (const task of orderedTasks(tasks)) {
    const column = depth.get(task.id) ?? 0;
    const list = columns.get(column) ?? [];
    list.push(task);
    columns.set(column, list);
  }

  const path = criticalPath(tasks, now);
  const onPath = new Set(path);

  const nodes: GraphNode[] = [];
  let maxRows = 0;
  for (const [column, list] of columns) {
    maxRows = Math.max(maxRows, list.length);
    list.forEach((task, row) => {
      nodes.push({
        task,
        depth: column,
        row,
        x: PAD + column * (NODE_W + GAP_X),
        y: PAD + row * (NODE_H + GAP_Y),
        tone: taskTone(task.state),
        duration: taskDuration(task, now),
        critical: onPath.has(task.id),
      });
    });
  }

  const links: GraphLink[] = edges(tasks).map((link) => ({
    from: link.from,
    to: link.to,
    live: (() => {
      const dependent = byId.get(link.to);
      return dependent
        ? dependent.state === 'running' || dependent.state === 'verifying'
        : false;
    })(),
    critical: onPath.has(link.from) && onPath.has(link.to),
  }));

  const waves: Wave[] = [...columns.entries()]
    .sort((a, b) => a[0] - b[0])
    .map(([index, list]) => ({ index, tasks: list.map((task) => task.id) }));

  const starts = tasks.map((task) => ms(task.startedAt)).filter((v): v is number => v !== null);
  const ends = tasks.map((task) => ms(task.endedAt)).filter((v): v is number => v !== null);
  const window =
    starts.length === 0
      ? null
      : {
          start: Math.min(...starts),
          end: Math.max(ends.length > 0 ? Math.max(...ends) : now, now),
        };

  return {
    nodes,
    links,
    waves,
    criticalPath: path,
    lanes: lanes(tasks, now),
    window,
    width: PAD * 2 + Math.max(1, columns.size) * (NODE_W + GAP_X) - GAP_X,
    height: PAD * 2 + Math.max(1, maxRows) * (NODE_H + GAP_Y) - GAP_Y,
  };
}

/* ------------------------------------------------------------------ *
 * The timeline
 * ------------------------------------------------------------------ */

/** One turn a member ran, in the run's clock. */
export interface MemberSpan {
  /** Milliseconds since the epoch. */
  start: number;
  end: number;
  /** Whether the turn is still running, so the span grows to the current moment. */
  live: boolean;
}

/** One member's row on the timeline. */
export interface MemberRow {
  /** The member this row is. */
  member: string;
  role: Role;
  /** The task it was on, when it was on one. */
  task: string | null;
  /** Its turns, oldest first. Empty for a member that has not worked. */
  spans: MemberSpan[];
  /** Whether its task lies on the critical path. */
  critical: boolean;
  /** Whether it is inside a turn right now. */
  live: boolean;
  /** Seconds it has spent inside the turns drawn here. */
  secs: number;
}

export interface MemberTimeline {
  rows: MemberRow[];
  /**
   * The wall clock the rows are drawn against, in milliseconds.
   *
   * Padded past the current moment while the run is live, so the marker has room
   * to be a mark rather than a border. `elapsed` is the run's own wall clock,
   * which is the figure the header shows.
   */
  window: { start: number; end: number } | null;
  /** The run's own wall clock in seconds, unpadded. */
  elapsed: number;
  /**
   * Where the current moment sits in that window, 0..1, or null when the run is
   * over.
   *
   * A fraction rather than a timestamp because the panel draws it as a position,
   * and a position is what a test can check without a DOM.
   */
  now: number | null;
}

/**
 * How much of the timeline's width is reserved past the current moment.
 *
 * The axis ends at now, so a marker drawn at now would sit exactly on the
 * track's right edge and read as a border rather than as a mark. Four percent is
 * enough room for a one-pixel line to be visibly inside the track and small
 * enough that it does not distort the spans: on a twenty-seven minute run it is
 * about a minute of axis, against spans measured in tens of minutes.
 */
const NOW_PAD = 0.04;

/** The order a role's row sits in, inside one task's pair. */
function roleRank(role: Role): number {
  switch (role) {
    case 'lead':
      return 0;
    case 'worker':
      return 1;
    case 'verifier':
      return 2;
    case 'reflector':
      return 3;
  }
}

/**
 * The run's wall clock, at member resolution.
 *
 * The task lanes answer "how wide did the run actually go". This answers the
 * question a slow run raises: which member is eating it. A task's worker and its
 * verifier are one bar on the task lanes and two rows here, because a verifier
 * that took twenty minutes against a worker's six is the single most useful
 * thing to know about a run that is running long — and the task lanes cannot
 * show it, because the task's span covers both.
 *
 * Rows come from members, not from tasks. A task that never reached `Verifying`
 * has no verifier, and a row invented for it would be a bar for a member that
 * does not exist.
 *
 * The window is the run's own: the earliest recorded start to the latest
 * recorded end, extended to now while the run is live. `graphView`'s window
 * always ends at now, which is right for a live run and wrong for a finished one
 * — a run that ended an hour ago would have its bars squeezed into a sliver of
 * the track. This one stops when the run does.
 */
export function memberTimeline(run: Run, now = Date.now()): MemberTimeline {
  const order = new Map(orderedTasks(run.tasks).map((task, index) => [task.id, index]));
  const onPath = new Set(criticalPath(run.tasks, now));

  const row = (member: Member): MemberRow => {
    const spans: MemberSpan[] = [];
    for (const span of member.spans) {
      const start = ms(span.start);
      if (start === null) continue;
      const end = ms(span.end) ?? now;
      spans.push({ start, end: Math.max(start, end), live: false });
    }
    // A turn in flight is a span that has not ended yet. `turnStartedAt` is the
    // only record of it — the span itself is appended when the turn ends — so
    // without this the row of a member that is working right now would be empty,
    // which is the one row a reader is looking at.
    const open = ms(member.turnStartedAt);
    if (open !== null) {
      spans.push({ start: open, end: Math.max(open, now), live: true });
    }
    spans.sort((a, b) => a.start - b.start);
    const secs = spans.reduce((total, span) => total + (span.end - span.start) / 1000, 0);
    return {
      member: member.id,
      role: member.role,
      task: member.task,
      spans,
      critical: member.task !== null && onPath.has(member.task),
      live: open !== null,
      secs: Math.round(secs),
    };
  };

  // The lead heads the list and the reflector ends it, the same shape the roster
  // draws: the lead outlives every task and the reflector runs after all of them.
  const rows: MemberRow[] = [row(run.lead)];
  const middle = run.members.filter((member) => member.role !== 'reflector');
  middle.sort((a, b) => {
    const left = a.task ? (order.get(a.task) ?? Number.MAX_SAFE_INTEGER) : Number.MAX_SAFE_INTEGER;
    const right = b.task ? (order.get(b.task) ?? Number.MAX_SAFE_INTEGER) : Number.MAX_SAFE_INTEGER;
    if (left !== right) return left - right;
    // Worker before verifier: the worker's turn is what the verifier's waits on,
    // so the pair reads in the order it ran.
    return roleRank(a.role) - roleRank(b.role);
  });
  for (const member of middle) rows.push(row(member));
  const reflector = run.members.find((member) => member.role === 'reflector');
  if (reflector) rows.push(row(reflector));

  const starts: number[] = [];
  const ends: number[] = [];
  for (const task of run.tasks) {
    const start = ms(task.startedAt);
    if (start !== null) starts.push(start);
    const end = ms(task.endedAt);
    if (end !== null) ends.push(end);
  }
  for (const drawn of rows) {
    for (const span of drawn.spans) {
      starts.push(span.start);
      ends.push(span.end);
    }
  }
  if (starts.length === 0) return { rows, window: null, elapsed: 0, now: null };

  const live = isLive(run.state);
  const start = Math.min(...starts);
  const recorded = ends.length > 0 ? Math.max(...ends) : start;
  const end = live ? Math.max(recorded, now) : recorded;
  // A run whose every span is a single instant still needs a track to draw on.
  const span = Math.max(1, end - start);
  const padded = live ? end + span * NOW_PAD : end;
  const fraction = live ? Math.min(1, Math.max(0, (now - start) / (padded - start))) : null;

  return {
    rows,
    window: { start, end: padded },
    elapsed: Math.round(span / 1000),
    now: fraction,
  };
}

/**
 * Tokens per minute between consecutive readings.
 *
 * A rate, not a total. The total says what a member has cost; the rate says
 * whether it is doing anything, which is the question a card asks twenty-one
 * times at once. The first reading has no predecessor, so it has no rate — a
 * series of one reading is a flat line, which is the honest drawing of "one
 * reading, no rate yet".
 *
 * A segment shorter than half a second is dropped rather than divided by: a rate
 * computed over a tenth of a second is a measurement of the poll interval, not
 * of the member.
 */
export function rateSeries(readings: Reading[]): number[] {
  const rates: number[] = [];
  for (let index = 1; index < readings.length; index += 1) {
    const previous = readings[index - 1];
    const current = readings[index];
    if (!previous || !current) continue;
    const from = ms(previous.at);
    const to = ms(current.at);
    if (from === null || to === null) continue;
    const secs = (to - from) / 1000;
    if (secs < 0.5) continue;
    const spent = current.tokens - previous.tokens;
    rates.push(Math.max(0, Math.round((spent / secs) * 60)));
  }
  return rates;
}

/**
 * The sparkline's geometry, as one `d` string.
 *
 * One path rather than forty elements: the panel draws twenty-one of these and
 * polls every two seconds, so the drawing has to be one node per card. The
 * geometry is the data — forty points across forty pixels, one per column — so
 * the line is the readings rather than a resampling of them.
 *
 * Scaled to the series' own peak, because the question is "is this member doing
 * anything" and a member spending ten tokens a minute is doing something. The
 * absolute figure is drawn beside the line, so the shape is not the only reading.
 *
 * Fewer than two rates is a flat line at the baseline: one rate has nothing to
 * be compared against, and drawing it at the top would claim it is the peak of
 * something. A parked member has no rates at all, so its card is flat — which is
 * the honest answer to "is it doing anything".
 */
export function sparkPath(rates: number[], width = 40, height = 12): string {
  const top = 1;
  const bottom = height - 1;
  const flat = `M0 ${bottom} L${width} ${bottom}`;
  if (rates.length < 2) return flat;

  const peak = Math.max(...rates);
  if (!(peak > 0)) return flat;

  const scale = (bottom - top) / peak;
  const step = width / (rates.length - 1);
  return rates
    .map((rate, index) => {
      const x = Math.round(index * step);
      const y = Math.round(bottom - Math.max(0, rate) * scale);
      return `${index === 0 ? 'M' : 'L'}${x} ${y}`;
    })
    .join(' ');
}

/* ------------------------------------------------------------------ *
 * Spend
 * ------------------------------------------------------------------ */

export interface MemberSpend {
  id: string;
  role: Role;
  tokens: number;
  cached: number;
  secs: number;
  turns: number;
  /** Share of the largest member, so a cheap member is still visible. */
  share: number;
}

export interface RoleSpend {
  role: Role;
  label: string;
  tokens: number;
  cached: number;
  secs: number;
  turns: number;
  members: number;
  /** Share of the run's total tokens. */
  share: number;
}

export interface SpendView {
  tokens: number;
  cached: number;
  /** `cached / tokens`, clamped. */
  cacheRate: number;
  /** Seconds spent inside turns, summed across members. Runs in parallel, so this exceeds wall clock. */
  secs: number;
  turns: number;
  /** Wall clock of the run, seconds. */
  wallSecs: number;
  /** Fraction of the token budget used, clamped. */
  budget: number;
  maxTokens: number;
  /**
   * Tokens the run was billed for: everything that crossed the wire minus the
   * cache reads.
   *
   * The ceiling counts this, not `tokens`. A member re-sends its whole context
   * every turn, so a run that has done almost nothing can show millions of
   * tokens of which nearly all are cache reads — and a bar drawn from the
   * traffic figure would read 110% while the run was five cents in.
   */
  spentTokens: number;
  /** The wall-clock limit the run was given, seconds. Zero when it had none. */
  maxWallSecs: number;
  /** Fraction of the wall-clock limit used, clamped. */
  wallBudget: number;
  /** Per member, largest first. */
  members: MemberSpend[];
  /** Per role, largest first. */
  roles: RoleSpend[];
  /**
   * Tokens per verified task, or null when nothing is verified.
   *
   * The one efficiency number a swarm has: a run that spent 400k tokens to
   * verify two tasks is a different animal from one that spent 40k to verify
   * twenty, and the total alone cannot tell them apart.
   */
  perVerified: number | null;
  /** How many tasks are verified. */
  verified: number;
}

/**
 * What the run cost, and who spent it.
 *
 * A swarm's whole promise is that it is faster than one agent, and its whole
 * risk is that it is fifteen times the tokens. Neither claim is checkable
 * without this: the total says what the run cost, the cache share says whether
 * that cost bought anything, and the per-role split says which role is eating
 * the budget — usually the lead, because it re-reads the plan on every tick.
 */
export function spendView(run: Run, now = Date.now()): SpendView {
  const everyone = [run.lead, ...run.members];
  const rows = everyone
    .map((member) => {
      const spend = spendOf(member);
      return { id: member.id, role: member.role, ...spend };
    })
    .filter((row) => row.tokens > 0 || row.secs > 0)
    .sort((a, b) => b.tokens - a.tokens);

  const tokens = rows.reduce((sum, row) => sum + row.tokens, 0);
  const cached = rows.reduce((sum, row) => sum + row.cached, 0);
  const secs = rows.reduce((sum, row) => sum + row.secs, 0);
  const turns = rows.reduce((sum, row) => sum + row.turns, 0);
  const peak = rows.reduce((max, row) => Math.max(max, row.tokens), 0);

  const byRole = new Map<Role, RoleSpend>();
  for (const row of rows) {
    const existing = byRole.get(row.role) ?? {
      role: row.role,
      label: roleLabel(row.role),
      tokens: 0,
      cached: 0,
      secs: 0,
      turns: 0,
      members: 0,
      share: 0,
    };
    existing.tokens += row.tokens;
    existing.cached += row.cached;
    existing.secs += row.secs;
    existing.turns += row.turns;
    existing.members += 1;
    byRole.set(row.role, existing);
  }
  const roles = [...byRole.values()]
    .map((role) => ({ ...role, share: tokens > 0 ? role.tokens / tokens : 0 }))
    .sort((a, b) => b.tokens - a.tokens);

  const start = ms(run.budget.startedAt ?? run.createdAt);
  const end = ms(run.endedAt) ?? now;
  const wallSecs = start === null ? 0 : Math.max(0, Math.round((end - start) / 1000));

  const verified = run.tasks.filter((task) => task.state === 'verified').length;
  const maxTokens = Math.max(0, run.budget.maxTokens);
  const maxWallSecs = Math.max(0, run.budget.maxWallSecs);
  // The ceiling counts what the run was billed for, so the bar does too. Cache
  // reads are traffic, not spend, and a bar drawn from traffic reads 110% on a
  // run that has barely started.
  const spentTokens = Math.max(0, run.budget.usedTokens - run.budget.cachedTokens);

  return {
    tokens,
    cached,
    cacheRate: tokens > 0 ? Math.min(1, cached / tokens) : 0,
    secs,
    turns,
    wallSecs,
    budget: maxTokens > 0 ? Math.min(1, spentTokens / maxTokens) : 0,
    maxTokens,
    spentTokens,
    maxWallSecs,
    wallBudget: maxWallSecs > 0 ? Math.min(1, wallSecs / maxWallSecs) : 0,
    members: rows.map((row) => ({
      ...row,
      share: peak > 0 ? row.tokens / peak : 0,
    })),
    roles,
    perVerified: verified > 0 ? Math.round(tokens / verified) : null,
    verified,
  };
}

/* ------------------------------------------------------------------ *
 * One member, in detail
 * ------------------------------------------------------------------ */

/** The tool call a member is running, and how long it has been running. */
export interface CurrentTool {
  item: ActivityItem;
  /** Seconds since it was announced. */
  secs: number;
}

/**
 * One member, as the detail pane draws it.
 *
 * The roster answers "who is on the run" in a card the width of a column. This
 * answers "what is this one doing" in the width of the panel, and the difference
 * is the whole phase: a state word says `working` for the whole of a thirteen
 * minute turn, and a live run spent thirteen of those minutes with the panel
 * reporting a verifier that was working and nothing else.
 */
export interface MemberDetailView {
  agent: AgentView;
  /** The tool call running right now, with its elapsed time. */
  current: CurrentTool | null;
  /** How long the current turn has been running, or null between turns. */
  turnSecs: number | null;
  /**
   * Tokens per minute over the feed's window, or null when it cannot be measured.
   *
   * A rate, not a total. The total says what a member has cost; the rate says
   * whether it is doing anything, which is the question a member that has been
   * quiet for four minutes raises.
   */
  rate: number | null;
  /** The feed, newest first, exactly as the engine sent it. */
  feed: ActivityItem[];
  /** This member's own board entries, newest first. */
  entries: BoardEntry[];
}

/** Seconds between two ISO stamps, or null when either is unreadable. */
function between(from: string, to: number): number | null {
  const start = ms(from);
  if (start === null) return null;
  return Math.max(0, (to - start) / 1000);
}

/**
 * Tokens per minute across the feed.
 *
 * The oldest line's reading is the total at that moment and the newest is the
 * total now, so the difference over the time between them is what the member
 * spent in that window. Two lines a second apart give a noisy figure, which is
 * why the span has a floor: a rate computed over a tenth of a second is a
 * measurement of the poll interval, not of the member.
 */
function rateOf(feed: ActivityItem[]): number | null {
  if (feed.length < 2) return null;
  const newest = feed[0];
  const oldest = feed[feed.length - 1];
  if (!newest || !oldest) return null;
  const from = ms(oldest.at);
  const to = ms(newest.at);
  if (from === null || to === null) return null;
  const secs = (to - from) / 1000;
  if (secs < 1) return null;
  const spent = newest.tokens - oldest.tokens;
  if (spent <= 0) return null;
  // Rounded to the token: a rate with thirteen decimal places is not a
  // measurement, and the panel formats it to three significant figures anyway.
  return Math.round((spent / secs) * 60);
}

/**
 * One member, projected for the detail pane.
 *
 * `now` is a parameter rather than a call to `Date.now()` so the elapsed times
 * are testable: a function that reads the clock itself can only be tested by
 * waiting.
 */
export function memberDetail(
  member: Member,
  run: Run,
  board: BoardEntry[],
  now: number,
): MemberDetailView {
  const agent = agentView(member, run, board);
  const feed = member.activity?.recent ?? [];
  const current = member.activity?.current ?? null;
  const turnSecs = member.turnStartedAt ? between(member.turnStartedAt, now) : null;
  return {
    agent,
    current: current ? { item: current, secs: between(current.at, now) ?? 0 } : null,
    turnSecs,
    rate: rateOf(feed),
    feed,
    entries: board.filter((entry) => entry.from === member.id).reverse(),
  };
}

/**
 * What the selected member is doing, for the board's inline strip.
 *
 * The board is a transcript of what *happened*; this is the one block on it that
 * says what is *happening*. It is a projection rather than a render because the
 * interesting cases are the empty ones — a parked member has no feed, a member
 * that never started has no activity at all, and a run read back from disk has
 * neither — and those are exactly the cases a renderer cannot be asked about.
 *
 * `now` is a parameter for the same reason it is in `memberDetail`: a function
 * that reads the clock itself can only be tested by waiting.
 */
export interface LiveActivityView {
  /** The tool call in flight, or null. */
  current: ActivityItem | null;
  /** How long it has been running, in seconds. Zero when nothing is. */
  elapsed: number;
  /** The newest few things it did, newest first. */
  recent: ActivityItem[];
  /** What to say when there is nothing to draw, or null when there is. */
  note: string | null;
}

/**
 * How many feed lines the board's strip shows.
 *
 * Three. The strip sits above a transcript that is already the long version of
 * the same run, so its job is to say "still going, here is what" in the space a
 * glance covers — not to be a second feed.
 */
const LIVE_LINES = 3;

export function liveActivity(member: Member, now: number): LiveActivityView {
  const current = member.activity?.current ?? null;
  const recent = (member.activity?.recent ?? []).slice(0, LIVE_LINES);
  const started = current ? ms(current.at) : null;
  const elapsed = started === null ? 0 : Math.max(0, (now - started) / 1000);
  // Three different silences, and they mean three different things. A member
  // with no activity has no watch, which is a member that never started or a run
  // read back from disk. A working member with no tool call is thinking. Any
  // other member with no tool call is simply not doing anything.
  const note = current
    ? null
    : member.activity
      ? member.state === 'working'
        ? 'thinking — no tool call in flight'
        : 'nothing running'
      : 'no live feed for this member';
  return { current, elapsed, recent, note };
}

/* ------------------------------------------------------------------ *
 * Selection
 * ------------------------------------------------------------------ */

/**
 * What the panel is focused on.
 *
 * The three surfaces are one picture seen three ways, so a click on any of them
 * has to mean the same thing everywhere: a task node in the graph, a group in
 * the roster, and a thread on the board are the same subject. An agent is the
 * other subject, and selecting one narrows the board to what that agent said.
 */
export type Selection =
  | { kind: 'task'; id: string }
  | { kind: 'agent'; id: string }
  | null;

/** Whether a board entry belongs to the current selection. */
export function inSelection(entry: BoardEntry, selection: Selection): boolean {
  if (!selection) return true;
  if (selection.kind === 'task') return entry.task === selection.id;
  return entry.from === selection.id || entry.to.includes(selection.id);
}

/** Whether a task is the selected one. */
export function taskSelected(selection: Selection, id: string): boolean {
  return selection?.kind === 'task' && selection.id === id;
}

/** Whether an agent is the selected one. */
export function agentSelected(selection: Selection, id: string): boolean {
  return selection?.kind === 'agent' && selection.id === id;
}

/**
 * The next member in the roster, for a `j` or a `k`.
 *
 * `order` is the roster's own order — the one the user is looking at — and not
 * `run.members`, which is arrival order and reshuffles as workers spawn. A
 * keyboard that walked a different order than the eye would be worse than no
 * keyboard at all, so the caller passes `rosterView(...).all` and this function
 * never looks at the run.
 *
 * `delta` is `+1` for the next row and `-1` for the previous. It wraps, because a
 * roster is a short list and a key that stops at the end is a key the user has to
 * check the end of. A selection that is not a member — a task, or nothing at all
 * — starts at the first row going forward and the last going back, so the first
 * `j` selects something rather than moving from nowhere to nowhere.
 */
export function stepSelection(selection: Selection, order: string[], delta: number): Selection {
  if (order.length === 0) return selection;
  const current = selection?.kind === 'agent' ? order.indexOf(selection.id) : -1;
  if (current < 0) {
    const start = delta >= 0 ? 0 : order.length - 1;
    const id = order[start];
    return id === undefined ? selection : { kind: 'agent', id };
  }
  const next = order[(current + delta + order.length) % order.length];
  return next === undefined ? selection : { kind: 'agent', id: next };
}

/** A one-line description of the selection, for the panel's filter bar. */
export function describeSelection(selection: Selection, run: Run): string {
  if (!selection) return 'everything';
  if (selection.kind === 'task') {
    const task = run.tasks.find((t) => t.id === selection.id);
    return task ? `${task.id} · ${task.title}` : selection.id;
  }
  if (selection.id === run.lead.id) return 'the lead';
  const member = run.members.find((m) => m.id === selection.id);
  return member ? `${member.id} (${roleLabel(member.role)})` : selection.id;
}
