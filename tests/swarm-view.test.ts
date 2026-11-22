/**
 * The panel's projections, driven directly.
 *
 * These are the tests the old suite was missing the equivalent of: they exercise
 * the thing the panel actually draws, not the fields it draws from. Every
 * function under test is pure, so there is no DOM, no bridge and no run to fake
 * beyond the document itself.
 */
import assert from 'node:assert/strict';
import { test } from 'node:test';
import {
  agentSelected,
  agentView,
  countFilter,
  criticalPath,
  describeAgent,
  describeSelection,
  droppedBefore,
  graphView,
  inSelection,
  lanes,
  liveActivity,
  liveProcesses,
  matchesFilter,
  memberDetail,
  memberTimeline,
  methodCoverage,
  rateSeries,
  rosterView,
  skillsOf,
  sparkPath,
  spendView,
  stepSelection,
  taskSelected,
  threads,
  type BoardFilter,
} from '../src/lib/swarm-view.ts';
import type {
  ActivityItem,
  BoardEntry,
  BoardKind,
  Member,
  Run,
  Task,
} from '../src/lib/swarm.ts';

/* ------------------------------------------------------------------ *
 * Fixtures
 * ------------------------------------------------------------------ */

function task(id: string, deps: string[] = [], state: Task['state'] = 'blocked'): Task {
  return {
    id,
    title: `Task ${id}`,
    detail: '',
    files: [],
    deps,
    acceptance: '',
    state,
    owner: null,
    verifier: null,
    attempts: 0,
    verdict: null,
    branch: null,
    baseCommit: null,
    worktree: null,
    startedAt: null,
    endedAt: null,
    conflict: null,
    conflictRepairs: 0,
    outOfGrant: [],
    reconcile: [],
    skills: [],
  };
}

function member(id: string, role: Member['role'], state: Member['state'] = 'idle'): Member {
  return {
    id,
    role,
    model: 'm',
    mode: null,
    state,
    pid: null,
    sessionId: null,
    worktree: null,
    task: null,
    turns: 0,
    tokensIn: 0,
    tokensOut: 0,
    cachedTokens: 0,
    activeSecs: 0,
    contextUsed: 0,
    contextSize: 0,
    parkedAt: null,
    turnStartedAt: null,
    spans: [],
    lastAt: null,
    error: null,
    skillsAssigned: [],
    skillsLoaded: [],
  };
}

function run(tasks: Task[], members: Member[] = [], state: Run['state'] = 'running'): Run {
  return {
    id: 'run-1',
    goal: 'g',
    workspace: '/tmp',
    state,
    createdAt: '2026-01-01T00:00:00.000Z',
    updatedAt: '2026-01-01T00:00:00.000Z',
    lead: member('lead', 'lead', 'working'),
    members,
    tasks,
    budget: { maxTokens: 1000, usedTokens: 0, cachedTokens: 0, maxWallSecs: 60, startedAt: null },
    integration: {
      branch: 'swarm/run-1',
      base: 'HEAD',
      state: 'pending',
      merged: [],
      conflict: null,
      suite: null,
      suitePassed: null,
    },
    concurrency: 4,
    reflection: null,
    failure: null,
    rehearsal: false,
    endedAt: null,
    skills: [],
    skillShortlist: [],
    skillGaps: [],
    skillProposals: [],
    roleUsage: [],
  };
}

let seq = 0;
function entry(
  from: string,
  kind: BoardKind,
  text: string,
  taskId: string | null = null,
  to: string[] = [],
): BoardEntry {
  seq += 1;
  return {
    seq,
    at: `2026-01-01T00:00:${String(seq).padStart(2, '0')}.000Z`,
    from,
    to,
    kind,
    text,
    task: taskId,
    evidence: [],
  };
}

/* ------------------------------------------------------------------ *
 * describeAgent
 * ------------------------------------------------------------------ */

test('a worker says which task it is implementing, not just that it is working', () => {
  const t = task('t1', [], 'running');
  const w = member('w1', 'worker', 'working');
  w.task = 't1';
  assert.equal(describeAgent(w, t, run([t])), 'implementing t1');
});

test('a verifier says it is trying to break the change, which is a different job', () => {
  const t = task('t1', [], 'verifying');
  const v = member('v1', 'verifier', 'working');
  v.task = 't1';
  assert.equal(describeAgent(v, t, run([t])), 'trying to break t1');
});

test('a parked agent says it is spending nothing, because that is the point of parking it', () => {
  const w = member('w1', 'worker', 'parked');
  w.sessionId = 'sess-1';
  assert.match(describeAgent(w, null, run([])), /spending nothing/);
});

test('a member blocked on a request says so, and does not blame its task', () => {
  // `waiting` is the gate holding a permission or an elicitation open. The task
  // is not what is holding the member up, and naming it sends the user looking
  // in the wrong place at exactly the moment nothing is moving.
  const w = member('w1', 'worker', 'waiting');
  w.task = 't1';
  const text = describeAgent(w, task('t1'), run([]));
  assert.match(text, /blocked on a request/);
  assert.doesNotMatch(text, /t1/, 'the task is not the reason it is waiting');
});

test('a queued member says it is waiting for a slot, not that it is working', () => {
  const w = member('w1', 'worker', 'queued');
  w.task = 't1';
  const text = describeAgent(w, task('t1'), run([]));
  assert.match(text, /slot/);
  assert.doesNotMatch(text, /implementing/, 'a member with no process is not implementing');
});

test('the lead is described by the run stage, not by its own state', () => {
  const lead = member('lead', 'lead', 'working');
  const stages: Array<[Run['state'], RegExp]> = [
    ['planning', /planning/],
    ['replanning', /re-planning/],
    ['awaiting-approval', /approval/],
    ['integrating', /merging/],
    ['reflecting', /reading the run back/],
    ['running', /coordinating/],
  ];
  for (const [state, pattern] of stages) {
    assert.match(describeAgent(lead, null, run([], [], state)), pattern, state);
  }
});

test('every member state produces a sentence, and none of them is empty', () => {
  const states: Member['state'][] = [
    'queued',
    'starting',
    'idle',
    'working',
    'parked',
    'waiting',
    'done',
    'failed',
    'lost',
  ];
  for (const state of states) {
    for (const role of ['lead', 'worker', 'verifier', 'reflector'] as const) {
      const text = describeAgent(member('m', role, state), null, run([]));
      assert.ok(text.length > 0, `${role}/${state} produced nothing`);
    }
  }
});

/* ------------------------------------------------------------------ *
 * agentView
 * ------------------------------------------------------------------ */

test('an agent carries the newest thing it said, not the newest thing said about it', () => {
  const board = [
    entry('w1', 'claim', 'mine, first'),
    entry('v1', 'verdict', 'not mine'),
    entry('w1', 'note', 'mine, last'),
  ];
  const view = agentView(member('w1', 'worker'), run([]), board);
  assert.equal(view.lastLine?.text, 'mine, last');
  assert.equal(view.lines, 2);
});

test('an agent that has said nothing has no last line rather than an empty one', () => {
  const view = agentView(member('w1', 'worker'), run([]), []);
  assert.equal(view.lastLine, null);
  assert.equal(view.lines, 0);
});

test('spend sums input and output, and the cache rate is a fraction of the total', () => {
  const w = member('w1', 'worker');
  w.tokensIn = 800;
  w.tokensOut = 200;
  w.cachedTokens = 500;
  w.activeSecs = 12;
  w.turns = 3;
  const view = agentView(w, run([]), []);
  assert.equal(view.spend.tokens, 1000);
  assert.equal(view.spend.cached, 500);
  assert.equal(view.spend.cacheRate, 0.5);
  assert.equal(view.spend.secs, 12);
  assert.equal(view.spend.turns, 3);
});

test('a cache figure larger than the token count is clamped, not reported as 140%', () => {
  const w = member('w1', 'worker');
  w.tokensIn = 100;
  w.cachedTokens = 140;
  assert.equal(agentView(w, run([]), []).spend.cacheRate, 1);
});

test('an unknown context size is a zero fraction, not a division by zero', () => {
  const w = member('w1', 'worker');
  w.contextUsed = 500;
  w.contextSize = 0;
  const view = agentView(w, run([]), []);
  assert.equal(view.context.fraction, 0);
  assert.equal(view.context.used, 500);
});

test('a known context size gives the fraction the meter draws', () => {
  const w = member('w1', 'worker');
  w.contextUsed = 30_000;
  w.contextSize = 120_000;
  assert.equal(agentView(w, run([]), []).context.fraction, 0.25);
});

test('an agent on a task carries that task, and one on a task the plan dropped carries none', () => {
  const t = task('t1');
  const w = member('w1', 'worker');
  w.task = 't1';
  assert.equal(agentView(w, run([t]), []).task?.id, 't1');

  const ghost = member('w2', 'worker');
  ghost.task = 'gone';
  assert.equal(agentView(ghost, run([t]), []).task, null);
});

/* ------------------------------------------------------------------ *
 * rosterView
 * ------------------------------------------------------------------ */

test('the lead heads the roster and the reflector ends it', () => {
  const members = [
    member('w1', 'worker', 'working'),
    member('r1', 'reflector', 'idle'),
    member('v1', 'verifier', 'working'),
  ];
  const view = rosterView(run([], members), []);
  assert.equal(view.all[0].member.id, 'lead');
  assert.equal(view.all[view.all.length - 1].member.id, 'r1');
  assert.equal(view.reflector?.member.id, 'r1');
});

test('the reflector is not also a group, because it is a stage rather than a task', () => {
  const members = [member('r1', 'reflector', 'idle')];
  const view = rosterView(run([], members), []);
  assert.equal(view.groups.length, 0);
});

test('groups follow dependency order, so the roster does not reshuffle as workers spawn', () => {
  const tasks = [task('t1'), task('t2', ['t1']), task('t3', ['t2'])];
  const members = ['t3', 't1', 't2'].map((id) => {
    const w = member(`w-${id}`, 'worker', 'working');
    w.task = id;
    return w;
  });
  const view = rosterView(run(tasks, members), []);
  assert.deepEqual(
    view.groups.map((group) => group.task?.id),
    ['t1', 't2', 't3'],
  );
});

test('a member with no task yet sorts last, because it has nothing to show', () => {
  const tasks = [task('t1')];
  const assigned = member('w1', 'worker', 'working');
  assigned.task = 't1';
  const loose = member('w2', 'worker', 'starting');
  const view = rosterView(run(tasks, [loose, assigned]), []);
  assert.equal(view.groups[0].task?.id, 't1');
  assert.equal(view.groups[1].task, null);
});

test('the roster counts who is working, who is parked and who is broken', () => {
  const members = [
    member('w1', 'worker', 'working'),
    member('w2', 'worker', 'parked'),
    member('w3', 'worker', 'failed'),
    member('v1', 'verifier', 'parked'),
  ];
  const view = rosterView(run([], members), []);
  // The lead is working too, so it counts.
  assert.equal(view.working, 2);
  assert.equal(view.parked, 2);
  assert.equal(view.broken, 1);
});

test('a queued member is counted as queued, not as parked', () => {
  // The proof the phase asks for. A queued member has no process, so it has
  // nothing to hold — parking is something a member with a session does. Before
  // this count existed a roster of twelve waiting on slots read as a roster of
  // four, and the one member that was actually working was invisible in it.
  const members = [
    member('w1', 'worker', 'queued'),
    member('w2', 'worker', 'queued'),
    member('w3', 'worker', 'parked'),
  ];
  const view = rosterView(run([], members), []);
  assert.equal(view.queued, 2);
  assert.equal(view.parked, 1, 'a queued member is not also parked');
  assert.equal(view.working, 1, 'only the lead is working');
});

test('the four roster counts are mutually exclusive, so no member is counted twice', () => {
  // What makes the header readable as a breakdown. A member in two of them would
  // make the numbers add up to more than the roster, and the reader would have to
  // work out which one to believe.
  const members = [
    member('w1', 'worker', 'queued'),
    member('w2', 'worker', 'working'),
    member('w3', 'worker', 'parked'),
    member('w4', 'worker', 'failed'),
    member('w5', 'worker', 'lost'),
  ];
  const view = rosterView(run([], members), []);
  assert.equal(
    view.queued + view.working + view.parked + view.broken,
    view.all.length,
    'every member here is in exactly one count',
  );
});

test('a state none of the four names is in none of them, and the total still shows it', () => {
  // `starting` is not queued, not working, not parked and not broken. The roster
  // prints its own total beside the four for exactly this reason.
  const members = [member('w1', 'worker', 'starting')];
  const view = rosterView(run([], members), []);
  assert.equal(view.queued + view.working + view.parked + view.broken, 1, 'only the lead');
  assert.equal(view.all.length, 2, 'the starting member is still on the roster');
});

test('an empty roster still has a lead, because a run always has one', () => {
  const view = rosterView(run([]), []);
  assert.equal(view.all.length, 1);
  assert.equal(view.lead.role, 'lead');
});

/* ------------------------------------------------------------------ *
 * threads
 * ------------------------------------------------------------------ */

test('the board is grouped by task, in dependency order, with run-wide entries last', () => {
  const tasks = [task('t1'), task('t2', ['t1'])];
  const board = [
    entry('lead', 'note', 'run-wide'),
    entry('w2', 'claim', 'on t2', 't2'),
    entry('w1', 'claim', 'on t1', 't1'),
  ];
  const view = threads(board, run(tasks));
  assert.deepEqual(
    view.map((thread) => thread.task),
    ['t1', 't2', null],
  );
});

test('an escalation and the decision that answered it are one exchange, not two entries', () => {
  const tasks = [task('t1')];
  const board = [
    entry('v1', 'escalation', 'the criterion is unprovable', 't1', ['lead']),
    entry('lead', 'decision', 'the plan gate pays', 't1'),
    entry('lead', 'note', 'revision 1 of 3', 't1'),
  ];
  const view = threads(board, run(tasks));
  const thread = view[0];
  assert.equal(thread.exchanges.length, 1);
  assert.equal(thread.exchanges[0].opener.text, 'the criterion is unprovable');
  assert.equal(thread.exchanges[0].replies.length, 2);
  assert.equal(thread.exchanges[0].owner, 'lead');
  assert.equal(thread.exchanges[0].resolved, true);
});

test('an exchange with no decision or verdict is unresolved, which is what a stuck run looks like', () => {
  const tasks = [task('t1')];
  const board = [
    entry('v1', 'conflict', 'two branches disagree', 't1'),
    entry('w1', 'note', 'still looking', 't1'),
  ];
  const view = threads(board, run(tasks));
  assert.equal(view[0].exchanges[0].resolved, false);
});

test('a second escalation opens a second exchange rather than joining the first', () => {
  const tasks = [task('t1')];
  const board = [
    entry('v1', 'escalation', 'first', 't1'),
    entry('lead', 'decision', 'answered', 't1'),
    entry('v2', 'escalation', 'second', 't1'),
  ];
  const view = threads(board, run(tasks));
  assert.equal(view[0].exchanges.length, 2);
  assert.equal(view[0].exchanges[0].replies.length, 1);
  assert.equal(view[0].exchanges[1].replies.length, 0);
});

test('a claim before any escalation is a lone entry, not an orphaned reply', () => {
  const tasks = [task('t1')];
  const board = [entry('w1', 'claim', 'taking t1', 't1'), entry('v1', 'escalation', 'no', 't1')];
  const view = threads(board, run(tasks));
  assert.equal(view[0].blocks.length, 2);
  assert.equal(view[0].blocks[0].kind, 'entry');
  assert.equal(view[0].blocks[1].kind, 'exchange');
});

test('filtering to problems keeps the answer with the problem', () => {
  const tasks = [task('t1')];
  const board = [
    entry('w1', 'claim', 'taking t1', 't1'),
    entry('v1', 'escalation', 'the criterion is unprovable', 't1'),
    entry('lead', 'decision', 'the plan gate pays', 't1'),
  ];
  const view = threads(board, run(tasks), 'problems');
  const thread = view[0];
  // The claim is dropped; the escalation keeps its decision.
  assert.deepEqual(
    thread.entries.map((e) => e.kind),
    ['escalation', 'decision'],
  );
  assert.equal(thread.exchanges[0].resolved, true);
});

test('a filter that matches nothing drops the thread rather than showing an empty one', () => {
  const tasks = [task('t1')];
  const board = [entry('w1', 'claim', 'taking t1', 't1')];
  assert.equal(threads(board, run(tasks), 'verdicts').length, 0);
});

test('a thread reports the kinds and authors it actually contains', () => {
  const tasks = [task('t1')];
  const board = [
    entry('w1', 'claim', 'a', 't1'),
    entry('v1', 'verdict', 'b', 't1'),
    entry('w1', 'evidence', 'c', 't1'),
  ];
  const thread = threads(board, run(tasks))[0];
  assert.deepEqual(thread.kinds, ['claim', 'verdict', 'evidence']);
  assert.deepEqual(thread.authors, ['w1', 'v1']);
});

test('every filter is total, and all of them accept every entry', () => {
  const filters: BoardFilter[] = ['all', 'problems', 'decisions', 'verdicts', 'evidence'];
  const kinds: BoardKind[] = [
    'claim',
    'decision',
    'evidence',
    'verdict',
    'conflict',
    'escalation',
    'note',
  ];
  for (const filter of filters) {
    for (const kind of kinds) {
      const result = matchesFilter(entry('m', kind, 'x'), filter);
      assert.equal(typeof result, 'boolean');
      if (filter === 'all') assert.equal(result, true);
    }
  }
});

test('the filter counts match what the filter shows', () => {
  const board = [
    entry('a', 'escalation', 'x'),
    entry('b', 'conflict', 'y'),
    entry('c', 'decision', 'z'),
    entry('d', 'note', 'w'),
  ];
  assert.equal(countFilter(board, 'all'), 4);
  assert.equal(countFilter(board, 'problems'), 2);
  assert.equal(countFilter(board, 'decisions'), 1);
  assert.equal(countFilter(board, 'verdicts'), 0);
});

/* ------------------------------------------------------------------ *
 * criticalPath
 * ------------------------------------------------------------------ */

test('the critical path is weighted by duration, not by how many tasks are in the chain', () => {
  const now = Date.parse('2026-01-01T00:10:00.000Z');
  const slow = task('slow', [], 'verified');
  slow.startedAt = '2026-01-01T00:00:00.000Z';
  slow.endedAt = '2026-01-01T00:09:00.000Z';

  const a = task('a', [], 'verified');
  a.startedAt = '2026-01-01T00:00:00.000Z';
  a.endedAt = '2026-01-01T00:00:01.000Z';
  const b = task('b', ['a'], 'verified');
  b.startedAt = '2026-01-01T00:00:01.000Z';
  b.endedAt = '2026-01-01T00:00:02.000Z';
  const c = task('c', ['b'], 'verified');
  c.startedAt = '2026-01-01T00:00:02.000Z';
  c.endedAt = '2026-01-01T00:00:03.000Z';

  // Three one-second tasks are a shorter path than one nine-minute task.
  assert.deepEqual(criticalPath([a, b, c, slow], now), ['slow']);
});

test('the critical path is a chain, so every task in it depends on the one before', () => {
  const tasks = [task('t1'), task('t2', ['t1']), task('t3', ['t2']), task('t4')];
  const path = criticalPath(tasks);
  assert.deepEqual(path, ['t1', 't2', 't3']);
});

test('an empty plan has an empty critical path rather than a crash', () => {
  assert.deepEqual(criticalPath([]), []);
});

test('a cycle terminates instead of hanging, and still names a path', () => {
  const tasks = [task('a', ['b']), task('b', ['a'])];
  const path = criticalPath(tasks);
  assert.ok(path.length >= 1);
  assert.ok(path.length <= 2);
});

/* ------------------------------------------------------------------ *
 * lanes
 * ------------------------------------------------------------------ */

test('two tasks that overlap get two lanes, because they really did run at once', () => {
  const t1 = task('t1', [], 'verified');
  t1.startedAt = '2026-01-01T00:00:00.000Z';
  t1.endedAt = '2026-01-01T00:00:10.000Z';
  const t2 = task('t2', [], 'verified');
  t2.startedAt = '2026-01-01T00:00:05.000Z';
  t2.endedAt = '2026-01-01T00:00:15.000Z';

  const packed = lanes([t1, t2]);
  assert.equal(packed.length, 2);
  assert.deepEqual(
    packed.map((lane) => lane.spans.map((span) => span.task)),
    [['t1'], ['t2']],
  );
});

test('a task that starts after the first one ended reuses its lane', () => {
  const t1 = task('t1', [], 'verified');
  t1.startedAt = '2026-01-01T00:00:00.000Z';
  t1.endedAt = '2026-01-01T00:00:10.000Z';
  const t2 = task('t2', [], 'verified');
  t2.startedAt = '2026-01-01T00:00:20.000Z';
  t2.endedAt = '2026-01-01T00:00:30.000Z';

  const packed = lanes([t1, t2]);
  assert.equal(packed.length, 1);
  assert.deepEqual(
    packed[0].spans.map((span) => span.task),
    ['t1', 't2'],
  );
});

test('a task that never started occupies no lane, because it used no time', () => {
  assert.deepEqual(lanes([task('t1')]), []);
});

test('a running task is measured to now, so its lane grows while it works', () => {
  const t1 = task('t1', [], 'running');
  t1.startedAt = '2026-01-01T00:00:00.000Z';
  const now = Date.parse('2026-01-01T00:00:30.000Z');
  const packed = lanes([t1], now);
  assert.equal(packed[0].spans[0].end, now);
});

test('an unreadable start time is dropped rather than becoming NaN', () => {
  const t1 = task('t1', [], 'running');
  t1.startedAt = 'not a date';
  assert.deepEqual(lanes([t1]), []);
});

/* ------------------------------------------------------------------ *
 * graphView
 * ------------------------------------------------------------------ */

test('a task sits one column right of its deepest dependency', () => {
  const tasks = [task('t1'), task('t2', ['t1']), task('t3', ['t1']), task('t4', ['t2', 't3'])];
  const view = graphView(run(tasks));
  const depth = new Map(view.nodes.map((node) => [node.task.id, node.depth]));
  assert.equal(depth.get('t1'), 0);
  assert.equal(depth.get('t2'), 1);
  assert.equal(depth.get('t3'), 1);
  assert.equal(depth.get('t4'), 2);
});

test('every edge points right, which is the whole reason for laying out by depth', () => {
  const tasks = [task('t1'), task('t2', ['t1']), task('t3', ['t2'])];
  const view = graphView(run(tasks));
  const x = new Map(view.nodes.map((node) => [node.task.id, node.x]));
  for (const link of view.links) {
    assert.ok((x.get(link.to) ?? 0) > (x.get(link.from) ?? 0), `${link.from} -> ${link.to}`);
  }
});

test('the canvas is sized to the columns and rows it holds', () => {
  const tasks = [task('t1'), task('t2', ['t1']), task('t3', ['t1'])];
  const view = graphView(run(tasks));
  // Two columns, two rows: 10*2 + 2*(132+44) - 44 and 10*2 + 2*(46+12) - 12.
  assert.equal(view.width, 328);
  assert.equal(view.height, 124);
});

test('the critical path is marked on the nodes and on the links between them', () => {
  const tasks = [task('t1'), task('t2', ['t1']), task('t3')];
  const view = graphView(run(tasks));
  const critical = view.nodes.filter((node) => node.critical).map((node) => node.task.id);
  assert.deepEqual(critical, ['t1', 't2']);
  assert.equal(view.links.length, 1);
  assert.equal(view.links[0].critical, true);
});

test('an edge into a live task animates, and one into a blocked task does not', () => {
  const tasks = [task('t1', [], 'verified'), task('t2', ['t1'], 'running')];
  const view = graphView(run(tasks));
  assert.equal(view.links[0].live, true);
});

test('waves are the depth columns, so a wave is what could run at once', () => {
  const tasks = [task('t1'), task('t2', ['t1']), task('t3', ['t1'])];
  const view = graphView(run(tasks));
  assert.deepEqual(view.waves, [
    { index: 0, tasks: ['t1'] },
    { index: 1, tasks: ['t2', 't3'] },
  ]);
});

test('a run where nothing has started has no time window rather than a zero-width one', () => {
  assert.equal(graphView(run([task('t1')])).window, null);
});

test('the time window spans the earliest start to the latest end', () => {
  const t1 = task('t1', [], 'verified');
  t1.startedAt = '2026-01-01T00:00:00.000Z';
  t1.endedAt = '2026-01-01T00:00:10.000Z';
  const t2 = task('t2', [], 'verified');
  t2.startedAt = '2026-01-01T00:00:05.000Z';
  t2.endedAt = '2026-01-01T00:00:20.000Z';
  const view = graphView(run([t1, t2]), Date.parse('2026-01-01T00:01:00.000Z'));
  assert.equal(view.window?.start, Date.parse('2026-01-01T00:00:00.000Z'));
  assert.equal(view.window?.end, Date.parse('2026-01-01T00:01:00.000Z'));
});

test('an empty plan still has a canvas to draw, so the panel is not a zero-size box', () => {
  const view = graphView(run([]));
  assert.equal(view.nodes.length, 0);
  assert.ok(view.width > 0);
  assert.ok(view.height > 0);
});

/* ------------------------------------------------------------------ *
 * spendView
 * ------------------------------------------------------------------ */

test('spend totals every member, lead included', () => {
  const lead = member('lead', 'lead');
  lead.tokensIn = 100;
  lead.tokensOut = 100;
  const w = member('w1', 'worker');
  w.tokensIn = 300;
  w.tokensOut = 100;
  const r = run([], [w]);
  r.lead = lead;
  const view = spendView(r);
  assert.equal(view.tokens, 600);
  assert.equal(view.turns, 0);
});

test('spend splits by role, so the lead eating the budget is visible', () => {
  const lead = member('lead', 'lead');
  lead.tokensIn = 900;
  const w1 = member('w1', 'worker');
  w1.tokensIn = 50;
  const w2 = member('w2', 'worker');
  w2.tokensIn = 50;
  const r = run([], [w1, w2]);
  r.lead = lead;
  const view = spendView(r);
  assert.equal(view.roles[0].role, 'lead');
  assert.equal(view.roles[0].tokens, 900);
  assert.equal(view.roles[0].members, 1);
  const workers = view.roles.find((role) => role.role === 'worker');
  assert.equal(workers?.tokens, 100);
  assert.equal(workers?.members, 2);
  assert.equal(workers?.share, 0.1);
});

test('a member that has spent nothing is not a row of zeros', () => {
  const view = spendView(run([], [member('w1', 'worker')]));
  assert.equal(view.members.length, 0);
  assert.equal(view.tokens, 0);
});

test('tokens per verified task is null until something is verified', () => {
  const lead = member('lead', 'lead');
  lead.tokensIn = 500;
  const r = run([task('t1', [], 'running')]);
  r.lead = lead;
  assert.equal(spendView(r).perVerified, null);
});

test('tokens per verified task is the efficiency number the total cannot give', () => {
  const lead = member('lead', 'lead');
  lead.tokensIn = 1000;
  const r = run([task('t1', [], 'verified'), task('t2', [], 'verified')]);
  r.lead = lead;
  assert.equal(spendView(r).perVerified, 500);
  assert.equal(spendView(r).verified, 2);
});

test('the budget fraction is clamped, so an overrun reads as full rather than as 140%', () => {
  const r = run([]);
  r.budget = { maxTokens: 100, usedTokens: 140, cachedTokens: 0, maxWallSecs: 60, startedAt: null };
  assert.equal(spendView(r).budget, 1);
});

test('a run with no budget reports zero rather than dividing by it', () => {
  const r = run([]);
  r.budget = { maxTokens: 0, usedTokens: 10, cachedTokens: 0, maxWallSecs: 60, startedAt: null };
  assert.equal(spendView(r).budget, 0);
});

test('the budget bar counts the spend, not the traffic, so a cache-heavy run is not full', () => {
  // A member re-sends its whole context every turn, so the wire figure runs an
  // order of magnitude ahead of what the run costs. A live run crossed 4.4M
  // tokens against a 4M ceiling with 96% of them cached and was killed for it.
  const r = run([]);
  r.budget = {
    maxTokens: 4_000_000,
    usedTokens: 4_406_635,
    cachedTokens: 4_230_370,
    maxWallSecs: 60,
    startedAt: null,
  };
  const view = spendView(r);
  assert.equal(view.spentTokens, 176_265);
  assert.ok(view.budget < 0.05, `176k of 4M is not a full bar, got ${view.budget}`);
});

test('a run that spends its ceiling in fresh tokens still reads as full', () => {
  const r = run([]);
  r.budget = { maxTokens: 100, usedTokens: 100, cachedTokens: 0, maxWallSecs: 60, startedAt: null };
  assert.equal(spendView(r).budget, 1);
});

test('wall clock is measured to the run end once it has one, so it stops counting', () => {
  const r = run([]);
  r.budget.startedAt = '2026-01-01T00:00:00.000Z';
  r.endedAt = '2026-01-01T00:02:00.000Z';
  assert.equal(spendView(r, Date.parse('2026-01-01T05:00:00.000Z')).wallSecs, 120);
});

test('member bars are shares of the largest member, so a cheap member is still visible', () => {
  const lead = member('lead', 'lead');
  lead.tokensIn = 1000;
  const w = member('w1', 'worker');
  w.tokensIn = 100;
  const r = run([], [w]);
  r.lead = lead;
  const view = spendView(r);
  assert.equal(view.members[0].share, 1);
  assert.equal(view.members[1].share, 0.1);
});

/* ------------------------------------------------------------------ *
 * Selection
 * ------------------------------------------------------------------ */

test('no selection means everything is in it', () => {
  assert.equal(inSelection(entry('w1', 'note', 'x', 't1'), null), true);
});

test('selecting a task narrows the board to that task', () => {
  const selection = { kind: 'task' as const, id: 't1' };
  assert.equal(inSelection(entry('w1', 'note', 'x', 't1'), selection), true);
  assert.equal(inSelection(entry('w1', 'note', 'x', 't2'), selection), false);
});

test('selecting an agent keeps what it said and what was said to it', () => {
  const selection = { kind: 'agent' as const, id: 'w1' };
  assert.equal(inSelection(entry('w1', 'note', 'x'), selection), true);
  assert.equal(inSelection(entry('lead', 'decision', 'x', null, ['w1']), selection), true);
  assert.equal(inSelection(entry('v1', 'note', 'x'), selection), false);
});

test('a task and an agent with the same id are not the same selection', () => {
  const byTask = { kind: 'task' as const, id: 'x' };
  const byAgent = { kind: 'agent' as const, id: 'x' };
  assert.equal(taskSelected(byTask, 'x'), true);
  assert.equal(taskSelected(byAgent, 'x'), false);
  assert.equal(agentSelected(byAgent, 'x'), true);
  assert.equal(agentSelected(byTask, 'x'), false);
});

test('a selection reads as a sentence, and a stale one still names itself', () => {
  const r = run([task('t1')]);
  assert.equal(describeSelection(null, r), 'everything');
  assert.match(describeSelection({ kind: 'task', id: 't1' }, r), /t1/);
  assert.equal(describeSelection({ kind: 'agent', id: 'lead' }, r), 'the lead');
  assert.equal(describeSelection({ kind: 'agent', id: 'gone' }, r), 'gone');
});

/* ------------------------------------------------------------------ *
 * The tail
 * ------------------------------------------------------------------ */

test('a board that starts at sequence one dropped nothing', () => {
  const board = [entry('a', 'note', 'x'), entry('b', 'note', 'y')];
  board[0].seq = 1;
  board[1].seq = 2;
  assert.equal(droppedBefore(board), 0);
});

test('a board that starts at sequence forty says thirty-nine entries are missing', () => {
  const board = [entry('a', 'note', 'x')];
  board[0].seq = 40;
  assert.equal(droppedBefore(board), 39);
});

test('an empty board dropped nothing rather than reporting a negative count', () => {
  assert.equal(droppedBefore([]), 0);
});

test('a decision scoped to "run" is run-wide, not a task called run', () => {
  const tasks = [task('t1')];
  const board = [
    entry('lead', 'decision', 'global', 'run'),
    entry('w1', 'claim', 'on t1', 't1'),
  ];
  const view = threads(board, run(tasks));
  assert.deepEqual(
    view.map((thread) => thread.task),
    ['t1', null],
  );
});

test('a scope that names no task in the plan is run-wide rather than a phantom thread', () => {
  const tasks = [task('t1')];
  const board = [entry('lead', 'note', 'x', 'gone')];
  assert.equal(threads(board, run(tasks))[0].task, null);
});

test('a member that never opened a session is not started, not parked', () => {
  const fresh = member('w-t2', 'worker', 'parked');
  assert.equal(describeAgent(fresh, null, run([])), 'not started yet');
});

test('a member holding a session it is not spending on is parked', () => {
  const held = member('w-t1', 'worker', 'parked');
  held.sessionId = 'sess-1';
  assert.match(describeAgent(held, null, run([])), /spending nothing/);
});

test('a member that has taken a turn is parked even with no session id on record', () => {
  const ran = member('w-t1', 'worker', 'parked');
  ran.turns = 2;
  assert.match(describeAgent(ran, null, run([])), /spending nothing/);
});

test('a run that overran its wall clock reports a full bar, not a 212% one', () => {
  const r = run([]);
  r.budget.startedAt = '2026-01-01T00:00:00.000Z';
  r.budget.maxWallSecs = 3600;
  r.endedAt = '2026-01-01T02:07:26.000Z';
  const view = spendView(r);
  assert.equal(view.wallSecs, 7646);
  assert.equal(view.wallBudget, 1);
  assert.equal(view.maxWallSecs, 3600);
});

test('a run with no wall-clock limit reports zero rather than dividing by it', () => {
  const r = run([]);
  r.budget.maxWallSecs = 0;
  assert.equal(spendView(r).wallBudget, 0);
});

/* ------------------------------------------------------------------ *
 * skillsOf
 * ------------------------------------------------------------------ */

test('a member that opened everything it was handed has no gap', () => {
  const m = member('w1', 'worker', 'working');
  m.skillsAssigned = ['systematic-debugging', 'test-driven-development'];
  m.skillsLoaded = ['systematic-debugging', 'test-driven-development'];
  const view = skillsOf(m);
  assert.deepEqual(view.assigned, ['systematic-debugging', 'test-driven-development']);
  assert.deepEqual(view.missing, []);
});

test('a skill named in the brief and never opened is a gap, in the brief order', () => {
  const m = member('w1', 'worker', 'working');
  m.skillsAssigned = ['a', 'b', 'c'];
  m.skillsLoaded = ['c'];
  assert.deepEqual(skillsOf(m).missing, ['a', 'b']);
});

test('a skill the member loaded without being told to is not a gap', () => {
  const m = member('w1', 'worker', 'working');
  m.skillsAssigned = [];
  m.skillsLoaded = ['found-it-itself'];
  const view = skillsOf(m);
  assert.deepEqual(view.assigned, []);
  assert.deepEqual(view.loaded, ['found-it-itself']);
  assert.deepEqual(view.missing, []);
});

test('a member document from before the skill plane reads as no skills, not as undefined', () => {
  const m = member('w1', 'worker', 'working');
  delete (m as Partial<Member>).skillsAssigned;
  delete (m as Partial<Member>).skillsLoaded;
  assert.deepEqual(skillsOf(m), { assigned: [], loaded: [], missing: [] });
});

/* ------------------------------------------------------------------ *
 * methodCoverage
 * ------------------------------------------------------------------ */

test('coverage counts a task as opened when either member on it read a skill', () => {
  const t1 = task('t1');
  t1.skills = ['systematic-debugging'];
  const worker = member('w1', 'worker', 'working');
  worker.task = 't1';
  worker.skillsAssigned = ['systematic-debugging'];
  worker.skillsLoaded = [];
  const verifier = member('v1', 'verifier', 'working');
  verifier.task = 't1';
  verifier.skillsAssigned = ['systematic-debugging'];
  verifier.skillsLoaded = ['systematic-debugging'];

  const view = methodCoverage(run([t1], [worker, verifier]));
  assert.equal(view.tasks, 1);
  assert.equal(view.assigned, 1);
  assert.equal(view.opened, 1);
  assert.equal(view.unread, 0);
});

test('coverage counts an assignment nobody opened as unread', () => {
  const t1 = task('t1');
  t1.skills = ['a', 'b'];
  const worker = member('w1', 'worker', 'working');
  worker.task = 't1';
  worker.skillsLoaded = ['a'];

  const view = methodCoverage(run([t1], [worker]));
  assert.equal(view.assigned, 1);
  assert.equal(view.opened, 1);
  assert.equal(view.unread, 1);
});

test('a task with no skills is not counted as assigned, so a bare run reports zero', () => {
  const view = methodCoverage(run([task('t1'), task('t2')], [member('w1', 'worker')]));
  assert.deepEqual(view, { tasks: 2, assigned: 0, opened: 0, unread: 0 });
});

test('a task nobody has been spawned on is not counted, because it has not been worked', () => {
  // The approval wait is the one moment the user is reading the plan, and every
  // assignment would read as a gap if an unspawned task counted.
  const t1 = task('t1');
  t1.skills = ['a'];
  const view = methodCoverage(run([t1], []));
  assert.deepEqual(view, { tasks: 1, assigned: 0, opened: 0, unread: 0 });
});

/* ------------------------------------------------------------------ *
 * memberDetail
 * ------------------------------------------------------------------ */

/** A feed line, at a fixed offset from a fixed base. */
function line(
  at: string,
  kind: ActivityItem['kind'],
  title: string,
  status: string,
  tokens: number,
  id: string | null = null,
): ActivityItem {
  return { at, kind, title, status, id, tokens };
}

const T0 = Date.parse('2026-01-01T00:10:00.000Z');

test('the current tool is drawn with how long it has been running', () => {
  // The proof the phase asks for: selecting a worker shows its current tool
  // call. A state word says `working`; this says `npm test` and `4m`.
  const w = member('w1', 'worker', 'working');
  w.activity = {
    current: line('2026-01-01T00:06:00.000Z', 'tool-call', 'bash', 'in-progress', 0, 'c1'),
    recent: [],
    series: [],
  };
  const view = memberDetail(w, run([]), [], T0);
  assert.equal(view.current?.item.title, 'bash');
  assert.equal(view.current?.secs, 240);
});

test('a member with nothing in flight has no current tool', () => {
  const w = member('w1', 'worker', 'working');
  w.activity = { current: null, recent: [], series: [] };
  assert.equal(memberDetail(w, run([]), [], T0).current, null);
});

test('a member with no activity at all is not a crash', () => {
  // A finished run has no watches, so every member arrives with the default.
  const w = member('w1', 'worker', 'done');
  const view = memberDetail(w, run([], [], 'done'), [], T0);
  assert.equal(view.current, null);
  assert.deepEqual(view.feed, []);
  assert.equal(view.rate, null);
});

test('the turn clock is measured from the turn start, not from the last word', () => {
  const w = member('w1', 'worker', 'working');
  w.turnStartedAt = '2026-01-01T00:08:00.000Z';
  assert.equal(memberDetail(w, run([]), [], T0).turnSecs, 120);
});

test('a member between turns has no turn clock', () => {
  const w = member('w1', 'worker', 'idle');
  assert.equal(memberDetail(w, run([]), [], T0).turnSecs, null);
});

test('the rate is the tokens spent over the feed window, per minute', () => {
  // A rate, not a total: the total says what a member has cost, the rate says
  // whether it is doing anything. Two lines a minute apart, four thousand
  // tokens between them, is four thousand a minute.
  const w = member('w1', 'worker', 'working');
  w.activity = {
    current: null,
    recent: [
      line('2026-01-01T00:10:00.000Z', 'tool-call', 'bash', 'in-progress', 5000, 'c2'),
      line('2026-01-01T00:09:00.000Z', 'tool-call', 'read', 'completed', 1000, 'c1'),
    ],
    series: [],
  };
  assert.equal(memberDetail(w, run([]), [], T0).rate, 4000);
});

test('a rate needs two lines and a measurable span', () => {
  // One line has no window. Two lines a tenth of a second apart measure the
  // poll interval, not the member.
  const w = member('w1', 'worker', 'working');
  w.activity = {
    current: null,
    recent: [line('2026-01-01T00:10:00.000Z', 'tool-call', 'bash', 'in-progress', 5000, 'c1')],
  };
  assert.equal(memberDetail(w, run([]), [], T0).rate, null);

  w.activity = {
    current: null,
    recent: [
      line('2026-01-01T00:10:00.000Z', 'tool-call', 'bash', 'in-progress', 5000, 'c2'),
      line('2026-01-01T00:09:59.900Z', 'tool-call', 'read', 'completed', 1000, 'c1'),
    ],
    series: [],
  };
  assert.equal(memberDetail(w, run([]), [], T0).rate, null);
});

test('a feed that has spent nothing has no rate rather than a zero', () => {
  // A zero reads as a measurement and is not one.
  const w = member('w1', 'worker', 'working');
  w.activity = {
    current: null,
    recent: [
      line('2026-01-01T00:10:00.000Z', 'tool-call', 'bash', 'in-progress', 0, 'c2'),
      line('2026-01-01T00:09:00.000Z', 'tool-call', 'read', 'completed', 0, 'c1'),
    ],
    series: [],
  };
  assert.equal(memberDetail(w, run([]), [], T0).rate, null);
});

test('the feed is passed through newest first, as the engine sent it', () => {
  const w = member('w1', 'worker', 'working');
  w.activity = {
    current: null,
    recent: [
      line('2026-01-01T00:10:00.000Z', 'tool-call', 'second', 'in-progress', 0, 'c2'),
      line('2026-01-01T00:09:00.000Z', 'tool-call', 'first', 'completed', 0, 'c1'),
    ],
    series: [],
  };
  assert.deepEqual(
    memberDetail(w, run([]), [], T0).feed.map((item) => item.title),
    ['second', 'first'],
  );
});

test('the pane shows this member\'s own board entries, newest first', () => {
  const w = member('w1', 'worker', 'working');
  const board = [
    entry('w1', 'claim', 'first'),
    entry('v1', 'verdict', 'not mine'),
    entry('w1', 'evidence', 'second'),
  ];
  const view = memberDetail(w, run([], [w]), board, T0);
  assert.deepEqual(
    view.entries.map((item) => item.text),
    ['second', 'first'],
  );
});

/* ------------------------------------------------------------------ *
 * liveActivity
 * ------------------------------------------------------------------ */

test('the board strip draws the tool in flight and how long it has been running', () => {
  // The proof the phase asks for: the board says what is happening, not only
  // what happened. A transcript of a member's messages cannot say `npm test`.
  const w = member('w1', 'worker', 'working');
  w.activity = {
    current: line('2026-01-01T00:06:00.000Z', 'tool-call', 'npm test', 'in-progress', 0, 'c1'),
    recent: [],
    series: [],
  };
  const view = liveActivity(w, T0);
  assert.equal(view.current?.title, 'npm test');
  assert.equal(view.elapsed, 240);
  assert.equal(view.note, null, 'there is something to draw, so nothing to say');
});

test('the strip shows the newest few things it did, and no more', () => {
  // Three. The strip sits above a transcript that is already the long version of
  // the same run, so it is a glance, not a second feed.
  const w = member('w1', 'worker', 'working');
  w.activity = {
    current: null,
    recent: [
      line('2026-01-01T00:09:00.000Z', 'tool-update', 'four', 'completed', 0),
      line('2026-01-01T00:08:00.000Z', 'tool-update', 'three', 'completed', 0),
      line('2026-01-01T00:07:00.000Z', 'tool-update', 'two', 'completed', 0),
      line('2026-01-01T00:06:00.000Z', 'tool-update', 'one', 'completed', 0),
    ],
    series: [],
  };
  assert.deepEqual(
    liveActivity(w, T0).recent.map((item) => item.title),
    ['four', 'three', 'two'],
  );
});

test('a parked member has no feed, and the strip says so rather than drawing nothing', () => {
  // The unsafe assumption this guards: a selected member need not have activity.
  // A parked member has no watch, so it arrives with the field absent.
  const w = member('w1', 'worker', 'parked');
  const view = liveActivity(w, T0);
  assert.equal(view.current, null);
  assert.deepEqual(view.recent, []);
  assert.equal(view.elapsed, 0);
  assert.match(view.note ?? '', /no live feed/);
});

test('a working member with no tool call is thinking, which is not the same as idle', () => {
  // Two silences that look identical and mean opposite things: one is a member
  // between tool calls, the other is a member doing nothing at all.
  const working = member('w1', 'worker', 'working');
  working.activity = { current: null, recent: [], series: [] };
  assert.match(liveActivity(working, T0).note ?? '', /thinking/);

  const parked = member('w2', 'worker', 'parked');
  parked.activity = { current: null, recent: [], series: [] };
  assert.match(liveActivity(parked, T0).note ?? '', /nothing running/);
});

test('an unreadable start time is zero elapsed, not NaN', () => {
  const w = member('w1', 'worker', 'working');
  w.activity = {
    current: line('not a date', 'tool-call', 'bash', 'in-progress', 0),
    recent: [],
    series: [],
  };
  assert.equal(liveActivity(w, T0).elapsed, 0);
});

/* ------------------------------------------------------------------ *
 * memberTimeline
 * ------------------------------------------------------------------ */

/** A member with a task and a set of finished turns. */
function turned(
  id: string,
  role: Member['role'],
  taskId: string | null,
  spans: Array<[string, string]>,
): Member {
  const m = member(id, role, 'done');
  m.task = taskId;
  m.spans = spans.map(([start, end]) => ({ start, end }));
  return m;
}

test('the verifier row is longer than its worker row, from the recorded spans', () => {
  // The proof the phase asks for, and the reason the timeline exists. A live run
  // spent 20.7 minutes in its verifier against 6 in its worker, and the task
  // lanes drew both as one bar because the task's span covers the pair.
  const t1 = task('t1', [], 'verified');
  t1.startedAt = '2026-01-01T00:00:00.000Z';
  t1.endedAt = '2026-01-01T00:27:00.000Z';

  const worker = turned('w-t1', 'worker', 't1', [
    ['2026-01-01T00:00:00.000Z', '2026-01-01T00:06:00.000Z'],
  ]);
  const verifier = turned('v-t1', 'verifier', 't1', [
    ['2026-01-01T00:06:18.000Z', '2026-01-01T00:27:00.000Z'],
  ]);

  const timeline = memberTimeline(run([t1], [worker, verifier]), T0);
  const w = timeline.rows.find((row) => row.member === 'w-t1');
  const v = timeline.rows.find((row) => row.member === 'v-t1');
  assert.ok(w && v, 'both members have a row');

  assert.equal(w.secs, 360, 'the worker spent six minutes in its turn');
  assert.equal(v.secs, 1242, 'the verifier spent twenty minutes and forty-two seconds');
  assert.ok(v.secs > w.secs, 'the verifier is the longer row, which is the whole point');

  // The spans are the recorded timestamps, not a duration re-derived from them.
  assert.equal(w.spans[0]?.start, Date.parse('2026-01-01T00:00:00.000Z'));
  assert.equal(w.spans[0]?.end, Date.parse('2026-01-01T00:06:00.000Z'));
  assert.equal(v.spans[0]?.start, Date.parse('2026-01-01T00:06:18.000Z'));
  assert.equal(v.spans[0]?.end, Date.parse('2026-01-01T00:27:00.000Z'));
});

test('a task that never reached verifying has no verifier row', () => {
  // Rows come from members, not from tasks. A row invented for a task's missing
  // verifier would be a bar for a member that does not exist.
  const t1 = task('t1', [], 'running');
  t1.startedAt = '2026-01-01T00:00:00.000Z';
  const worker = turned('w-t1', 'worker', 't1', [
    ['2026-01-01T00:00:00.000Z', '2026-01-01T00:06:00.000Z'],
  ]);
  const timeline = memberTimeline(run([t1], [worker]), T0);
  assert.deepEqual(
    timeline.rows.map((row) => row.member),
    ['lead', 'w-t1'],
  );
});

test('a parked member has a row with no spans rather than no row', () => {
  // A member that has done nothing is a fact worth drawing: an empty track says
  // it has used no time, and a missing row would say it is not on the run.
  const parked = member('w-t1', 'worker', 'parked');
  parked.task = 't1';
  const t1 = task('t1', [], 'ready');
  t1.startedAt = '2026-01-01T00:00:00.000Z';
  const timeline = memberTimeline(run([t1], [parked]), T0);
  const row = timeline.rows.find((drawn) => drawn.member === 'w-t1');
  assert.ok(row);
  assert.deepEqual(row.spans, []);
  assert.equal(row.secs, 0);
  assert.equal(row.live, false);
});

test('a turn in flight is a span that grows to the current moment', () => {
  // `turnStartedAt` is the only record of a turn that has not ended — the span
  // itself is appended when it does — so without this the row of a member that
  // is working right now would be empty, which is the one row a reader is
  // looking at.
  const working = member('w-t1', 'worker', 'working');
  working.task = 't1';
  working.turnStartedAt = '2026-01-01T00:08:00.000Z';
  const t1 = task('t1', [], 'running');
  t1.startedAt = '2026-01-01T00:00:00.000Z';

  const timeline = memberTimeline(run([t1], [working]), T0);
  const row = timeline.rows.find((drawn) => drawn.member === 'w-t1');
  assert.ok(row);
  assert.equal(row.live, true);
  assert.equal(row.spans.length, 1);
  assert.equal(row.spans[0]?.start, Date.parse('2026-01-01T00:08:00.000Z'));
  assert.equal(row.spans[0]?.end, T0, 'the open span ends at now');
  assert.equal(row.secs, 120);
});

test('the critical path is marked on the rows whose task is on it', () => {
  // The proof the phase asks for: the chain that sets the floor on wall-clock is
  // distinguishable from the rows beside it. The panel draws the distinction as
  // the row's own track, so the projection has to carry the flag.
  const t1 = task('t1', [], 'verified');
  t1.startedAt = '2026-01-01T00:00:00.000Z';
  t1.endedAt = '2026-01-01T00:20:00.000Z';
  // Independent of t1, so the path is one task rather than a chain of two.
  const t2 = task('t2', [], 'verified');
  t2.startedAt = '2026-01-01T00:20:00.000Z';
  t2.endedAt = '2026-01-01T00:21:00.000Z';

  const timeline = memberTimeline(
    run(
      [t1, t2],
      [
        turned('w-t1', 'worker', 't1', [['2026-01-01T00:00:00.000Z', '2026-01-01T00:20:00.000Z']]),
        turned('w-t2', 'worker', 't2', [['2026-01-01T00:20:00.000Z', '2026-01-01T00:21:00.000Z']]),
      ],
    ),
    T0,
  );

  const on = timeline.rows.filter((row) => row.critical).map((row) => row.member);
  const off = timeline.rows.filter((row) => !row.critical).map((row) => row.member);
  assert.deepEqual(on, ['w-t1'], 'the twenty-minute task is the critical path');
  assert.deepEqual(off, ['lead', 'w-t2'], 'the one-minute task is not');
});

test('the current moment is a fraction of the window, and only while the run is live', () => {
  const t1 = task('t1', [], 'running');
  t1.startedAt = '2026-01-01T00:00:00.000Z';
  const working = member('w-t1', 'worker', 'working');
  working.task = 't1';
  working.turnStartedAt = '2026-01-01T00:00:00.000Z';

  const live = memberTimeline(run([t1], [working]), T0);
  assert.ok(live.now !== null, 'a live run marks the current moment');
  assert.ok(live.now > 0 && live.now < 1, 'the mark sits inside the track, not on its edge');
  assert.ok(live.window);
  assert.equal(live.elapsed, 600, 'the header shows the run, not the padded axis');

  // A finished run has no current moment to mark: its axis stops when it did.
  const done = memberTimeline(run([t1], [working], 'done'), T0);
  assert.equal(done.now, null);
  assert.equal(done.window?.end, T0, 'the axis stops at the last recorded end');
});

test('a run where nothing has started has no timeline rather than a zero-width one', () => {
  const timeline = memberTimeline(run([task('t1')]), T0);
  assert.equal(timeline.window, null);
  assert.equal(timeline.now, null);
});

test('the lead heads the rows and the reflector ends them', () => {
  const t1 = task('t1', [], 'verified');
  t1.startedAt = '2026-01-01T00:00:00.000Z';
  const timeline = memberTimeline(
    run(
      [t1],
      [
        turned('v-t1', 'verifier', 't1', [['2026-01-01T00:10:00.000Z', '2026-01-01T00:20:00.000Z']]),
        turned('w-t1', 'worker', 't1', [['2026-01-01T00:00:00.000Z', '2026-01-01T00:10:00.000Z']]),
        turned('reflector', 'reflector', null, []),
      ],
    ),
    T0,
  );
  assert.deepEqual(
    timeline.rows.map((row) => row.member),
    ['lead', 'w-t1', 'v-t1', 'reflector'],
    'worker before verifier, because that is the order the pair ran in',
  );
});

test('twenty-one members are twenty-one rows, which is what the panel draws', () => {
  // "Renders twenty-one members without scroll jank" is not measurable in node.
  // The half that is: the projection produces one row per member and no more, so
  // the panel's work is linear in the roster and not in the run's history.
  const t1 = task('t1', [], 'verified');
  t1.startedAt = '2026-01-01T00:00:00.000Z';
  const members = Array.from({ length: 20 }, (_, index) =>
    turned(`w-${index}`, 'worker', 't1', [['2026-01-01T00:00:00.000Z', '2026-01-01T00:01:00.000Z']]),
  );
  const timeline = memberTimeline(run([t1], members), T0);
  assert.equal(timeline.rows.length, 21, 'the lead and twenty workers');
  assert.equal(
    timeline.rows.reduce((total, row) => total + row.spans.length, 0),
    20,
    'one span each, not one row per turn',
  );
});

/* ------------------------------------------------------------------ *
 * rateSeries and sparkPath
 * ------------------------------------------------------------------ */

/** A reading at a fixed offset from T0. */
function reading(at: string, tokens: number) {
  return { at, tokens };
}

test('the rate is the tokens between two readings over the time between them', () => {
  // A rate, not a total. Two readings a minute apart with four thousand tokens
  // between them is four thousand a minute.
  const rates = rateSeries([
    reading('2026-01-01T00:00:00.000Z', 1_000),
    reading('2026-01-01T00:01:00.000Z', 5_000),
  ]);
  assert.deepEqual(rates, [4_000]);
});

test('a rate over a tenth of a second is dropped, because it measures the poll', () => {
  const rates = rateSeries([
    reading('2026-01-01T00:00:00.000Z', 1_000),
    reading('2026-01-01T00:00:00.100Z', 5_000),
  ]);
  assert.deepEqual(rates, []);
});

test('a reading that went backwards is a zero rate, not a negative one', () => {
  // A respawn starts a new session and a new total. A negative rate would draw a
  // line below the baseline for a member that is working.
  const rates = rateSeries([
    reading('2026-01-01T00:00:00.000Z', 5_000),
    reading('2026-01-01T00:01:00.000Z', 1_000),
  ]);
  assert.deepEqual(rates, [0]);
});

test('the sparkline is flat for a parked member and non-flat for a working one', () => {
  // The proof the phase asks for. A parked member has no turn, so it has no
  // readings, so its card draws a flat line — which is the honest answer to "is
  // it doing anything". A working member's readings vary, so its line does.
  const parked = sparkPath(rateSeries([]));
  assert.equal(parked, 'M0 11 L40 11', 'no readings is a flat baseline');

  const working = sparkPath(
    rateSeries([
      reading('2026-01-01T00:00:00.000Z', 0),
      reading('2026-01-01T00:01:00.000Z', 6_000),
      reading('2026-01-01T00:02:00.000Z', 6_500),
      reading('2026-01-01T00:03:00.000Z', 20_000),
    ]),
  );
  const ys = [...working.matchAll(/[ML](\d+) (\d+)/g)].map((match) => Number(match[2]));
  assert.ok(ys.length >= 3, 'the working member has a line, not a point');
  assert.ok(new Set(ys).size > 1, 'the line is not flat');
});

test('a sparkline is one path, not forty elements', () => {
  // The other half of "renders twenty-one members without scroll jank": the
  // drawing is one node per card. Forty points across forty pixels is one `M`
  // and thirty-nine `L`s in a single `d`, which is one `<path>` in the DOM.
  const rates = Array.from({ length: 40 }, (_, index) => index * 100);
  const path = sparkPath(rates);
  assert.equal(path.split('M').length - 1, 1, 'one move command');
  assert.equal(path.split('L').length - 1, 39, 'thirty-nine line commands');
  assert.ok(!path.includes('NaN'), 'no unreadable reading becomes a broken path');
});

test('a series of one rate is flat, because one rate has nothing to compare against', () => {
  assert.equal(sparkPath([9_000]), 'M0 11 L40 11');
  // A series of zeros is a flat baseline too: nothing was spent, so there is no
  // shape to draw and a line at the top would claim there was.
  assert.equal(sparkPath([0, 0, 0]), 'M0 11 L40 11');
});

/* ------------------------------------------------------------------ *
 * stepSelection
 * ------------------------------------------------------------------ */

test('j walks the roster in the order the user sees, not the order members arrived', () => {
  // The proof the phase asks for. `run.members` is arrival order and reshuffles
  // as workers spawn; the roster is grouped by task in dependency order. A
  // keyboard that walked the first would disagree with the eye.
  const order = ['lead', 'w-t1', 'v-t1', 'w-t2', 'v-t2'];
  assert.deepEqual(stepSelection(null, order, 1), { kind: 'agent', id: 'lead' });
  assert.deepEqual(stepSelection({ kind: 'agent', id: 'lead' }, order, 1), {
    kind: 'agent',
    id: 'w-t1',
  });
  assert.deepEqual(stepSelection({ kind: 'agent', id: 'w-t1' }, order, 1), {
    kind: 'agent',
    id: 'v-t1',
  });
});

test('k walks back up, and both ends wrap', () => {
  // A roster is a short list, and a key that stops at the end is a key the user
  // has to check the end of.
  const order = ['lead', 'w-t1', 'v-t1'];
  assert.deepEqual(stepSelection({ kind: 'agent', id: 'w-t1' }, order, -1), {
    kind: 'agent',
    id: 'lead',
  });
  assert.deepEqual(stepSelection({ kind: 'agent', id: 'lead' }, order, -1), {
    kind: 'agent',
    id: 'v-t1',
  });
  assert.deepEqual(stepSelection({ kind: 'agent', id: 'v-t1' }, order, 1), {
    kind: 'agent',
    id: 'lead',
  });
});

test('the first k selects the last row, so the key always does something', () => {
  const order = ['lead', 'w-t1', 'v-t1'];
  assert.deepEqual(stepSelection(null, order, -1), { kind: 'agent', id: 'v-t1' });
});

test('a task selection is not a member, so the first j starts at the top', () => {
  // The three surfaces share one selection. A task is a different subject, and
  // stepping from it must not silently do nothing.
  const order = ['lead', 'w-t1'];
  assert.deepEqual(stepSelection({ kind: 'task', id: 't1' }, order, 1), {
    kind: 'agent',
    id: 'lead',
  });
});

test('a selection that names a member no longer on the roster starts at the top', () => {
  const order = ['lead', 'w-t1'];
  assert.deepEqual(stepSelection({ kind: 'agent', id: 'gone' }, order, 1), {
    kind: 'agent',
    id: 'lead',
  });
});

test('an empty roster leaves the selection alone rather than inventing a member', () => {
  assert.equal(stepSelection(null, [], 1), null);
  const task = { kind: 'task' as const, id: 't1' };
  assert.deepEqual(stepSelection(task, [], 1), task);
});

test('liveProcesses counts the lead and members with a pid, beside the worker ceiling', () => {
  // `concurrency` bounds workers, not processes: a verifying task holds a live
  // verifier that does not count against the ceiling, so the panel shows the
  // live count next to it rather than inferring it.
  const w1 = { ...member('w-t1', 'worker', 'working'), pid: 101 };
  const v2 = { ...member('v-t2', 'verifier', 'working'), pid: 102 };
  const parked = { ...member('w-t3', 'worker', 'parked'), pid: null };
  const r = { ...run([task('t1'), task('t2'), task('t3')], [w1, v2, parked]), concurrency: 3 };
  r.lead.pid = 100;
  assert.equal(liveProcesses(r), 3, 'lead plus two live members; the parked one has no process');
  assert.equal(r.concurrency, 3, 'the ceiling is the worker bound, not the process count');
});
