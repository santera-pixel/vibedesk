import assert from 'node:assert/strict';
import { test } from 'node:test';
import {
  MIN_SPEEDUP,
  MIN_WALL_SECS,
  activityTone,
  boardKindTone,
  criticalPathLabel,
  describeRunState,
  describeTask,
  describeTaskState,
  edges,
  formatDuration,
  formatTokens,
  isLive,
  isTerminal,
  memberTone,
  orderedTasks,
  outrunsTheHour,
  perStepLabel,
  progress,
  roleGlyph,
  roleLabel,
  roleMedianLine,
  runTone,
  shapeWarningMessage,
  taskDuration,
  taskTone,
  worthRunning,
  type ActivityItem,
  type BoardEntry,
  type Member,
  type Rehearsal,
  type RoleMedian,
  type Run,
  type Task,
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
    worktree: null,
    startedAt: null,
    endedAt: null,
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
    parkedAt: null,
    lastAt: null,
    error: null,
  };
}

function run(tasks: Task[], state: Run['state'] = 'running'): Run {
  return {
    id: 'run-1',
    goal: 'g',
    workspace: '/tmp',
    state,
    createdAt: '2026-01-01T00:00:00.000Z',
    updatedAt: '2026-01-01T00:00:00.000Z',
    lead: member('lead', 'lead', 'working'),
    members: [],
    tasks,
    budget: { maxTokens: 1000, usedTokens: 0, maxWallSecs: 60, startedAt: null },
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
    roleUsage: [],
  };
}

function rehearsal(
  speedup: number,
  projectedWallSecs = 1200,
  medianTaskSecs = 600,
  measured = false,
): Rehearsal {
  return {
    waves: [],
    criticalPath: [],
    maxParallel: 1,
    serialSteps: 3,
    parallelSteps: 2,
    speedup,
    projectedWallSecs,
    // The same derivation the engine applies, so a fixture cannot describe a
    // budget the run would never be given.
    budgetWallSecs: Math.max(MIN_WALL_SECS, projectedWallSecs * 2),
    medianTaskSecs,
    measured,
    estimatedTokens: 100,
    singleAgentTokens: 50,
  };
}

function roleMedian(role: RoleMedian['role'], turnSecs: number, runs = 2): RoleMedian {
  return { role, turnSecs, turnTokens: 25_000, runs };
}

/* ------------------------------------------------------------------ *
 * Tones
 * ------------------------------------------------------------------ */

test('every run state maps to a tone, and the live ones are the live ones', () => {
  assert.equal(runTone('planning'), 'live');
  assert.equal(runTone('awaiting-approval'), 'live');
  assert.equal(runTone('running'), 'live');
  assert.equal(runTone('integrating'), 'live');
  assert.equal(runTone('reflecting'), 'live');
  assert.equal(runTone('done'), 'good');
  assert.equal(runTone('failed'), 'bad');
  assert.equal(runTone('cancelled'), 'bad');
  assert.equal(runTone('interrupted'), 'idle');
});

test('a verifying task is live, not good: it has not passed yet', () => {
  assert.equal(taskTone('verifying'), 'live');
  assert.equal(taskTone('verified'), 'good');
  assert.equal(taskTone('running'), 'live');
  assert.equal(taskTone('conflict'), 'bad');
  assert.equal(taskTone('blocked'), 'idle');
  assert.equal(taskTone('ready'), 'idle');
});

test('a parked member is idle, not live: it is spending nothing', () => {
  assert.equal(memberTone('parked'), 'idle');
  assert.equal(memberTone('working'), 'live');
  assert.equal(memberTone('starting'), 'live');
  assert.equal(memberTone('lost'), 'bad');
  assert.equal(memberTone('done'), 'good');
});

test('a verdict reads as good and a conflict as bad', () => {
  assert.equal(boardKindTone('verdict'), 'good');
  assert.equal(boardKindTone('conflict'), 'bad');
  assert.equal(boardKindTone('escalation'), 'bad');
  assert.equal(boardKindTone('decision'), 'live');
  assert.equal(boardKindTone('claim'), 'idle');
});

/* ------------------------------------------------------------------ *
 * Liveness
 * ------------------------------------------------------------------ */

test('isLive and isTerminal never both hold, and cover every state', () => {
  const states: Run['state'][] = [
    'planning',
    'awaiting-approval',
    'running',
    'integrating',
    'reflecting',
    'done',
    'failed',
    'cancelled',
    'interrupted',
  ];
  for (const state of states) {
    assert.equal(isLive(state) && isTerminal(state), false, state);
  }
  assert.equal(isLive('interrupted'), false);
  assert.equal(isTerminal('interrupted'), false);
});

/* ------------------------------------------------------------------ *
 * Labels
 * ------------------------------------------------------------------ */

test('a role has a word and a glyph, and the glyph is one letter', () => {
  assert.equal(roleLabel('lead'), 'Lead');
  assert.equal(roleLabel('worker'), 'Worker');
  assert.equal(roleLabel('verifier'), 'Verifier');
  assert.equal(roleLabel('reflector'), 'Reflector');
  for (const role of ['lead', 'worker', 'verifier', 'reflector'] as const) {
    assert.equal(roleGlyph(role).length, 1);
  }
});

test('every role glyph is distinct, so a dense roster row is unambiguous', () => {
  const glyphs = (['lead', 'worker', 'verifier', 'reflector'] as const).map(roleGlyph);
  assert.equal(new Set(glyphs).size, glyphs.length);
});

test('the critical path reads as a chain, and an empty one says so', () => {
  assert.equal(criticalPathLabel([]), 'none');
  assert.equal(criticalPathLabel(['t1']), 't1 alone');
  assert.equal(criticalPathLabel(['t1', 't2', 't3']), 't1 → t2 → t3');
});

/* ------------------------------------------------------------------ *
 * Numbers
 * ------------------------------------------------------------------ */

test('formatTokens shortens without lying about zero', () => {
  assert.equal(formatTokens(0), '0');
  assert.equal(formatTokens(-5), '0');
  assert.equal(formatTokens(999), '999');
  assert.equal(formatTokens(1500), '1.5k');
  assert.equal(formatTokens(25_000), '25k');
  assert.equal(formatTokens(2_500_000), '2.5M');
});

test('formatDuration picks the unit that fits', () => {
  assert.equal(formatDuration(0), '0s');
  assert.equal(formatDuration(45), '45s');
  assert.equal(formatDuration(90), '1m');
  assert.equal(formatDuration(3700), '1h 1m');
});

test('progress is the verified fraction, and an empty plan is zero not NaN', () => {
  assert.equal(progress(run([])), 0);
  assert.equal(progress(run([task('t1', [], 'verified')])), 1);
  assert.equal(
    progress(run([task('t1', [], 'verified'), task('t2', [], 'running')])),
    0.5,
  );
});

test('a rehearsal below 1.5x is not worth running', () => {
  assert.equal(worthRunning(rehearsal(1.0)), false);
  assert.equal(worthRunning(rehearsal(1.49)), false);
  assert.equal(worthRunning(rehearsal(1.5)), true);
  assert.equal(worthRunning(rehearsal(3)), true);
});

/* ------------------------------------------------------------------ *
 * The shape, said out loud
 * ------------------------------------------------------------------ */

test('a shape warning names the task it is about', () => {
  // The observed plan: one task could start and it owned 22 files. Both
  // warnings have to name it, because "a task is too large" is not actionable.
  const oneWide = shapeWarningMessage({ kind: 'one-wide-first-rank', task: 't1' });
  assert.match(oneWide, /t1/);
  assert.match(oneWide, /single-threaded/);

  const tooLarge = shapeWarningMessage({ kind: 'task-too-large', task: 't1', files: 22 });
  assert.match(tooLarge, /t1/);
  assert.match(tooLarge, /22/);
});

test('a not-worth-it warning carries both step counts and the threshold', () => {
  const message = shapeWarningMessage({
    kind: 'not-worth-it',
    speedup: 1.2,
    serialSteps: 10,
    parallelSteps: 8,
  });
  assert.match(message, /1\.2×/);
  assert.match(message, /10 steps serially/);
  assert.match(message, /8 at its widest/);
  assert.match(message, /1\.5×/);
});

test('the panel and the gate agree about what worth it means', () => {
  // `plan::MIN_SPEEDUP` on the Rust side. A panel that said 1.5 while the gate
  // said 1.4 would show a warning on a plan the gate accepted silently.
  assert.equal(MIN_SPEEDUP, 1.5);
  assert.equal(worthRunning(rehearsal(MIN_SPEEDUP)), true);
  assert.equal(worthRunning(rehearsal(MIN_SPEEDUP - 0.01)), false);
});

test('a plan projected past the flat hour is called out', () => {
  // The budget is derived from the projection, so "the projection exceeds the
  // budget" can never be true. The line that matters is the hour every run used
  // to be given: seven steps at 600s each is 70 minutes, and that is the first
  // depth that crosses it.
  assert.equal(MIN_WALL_SECS, 3600);
  assert.equal(outrunsTheHour(rehearsal(2, 3000)), false, '50 minutes fits the hour');
  assert.equal(outrunsTheHour(rehearsal(2, 3600)), false, 'exactly the hour is not past it');
  assert.equal(outrunsTheHour(rehearsal(2, 4200)), true, '70 minutes is past it');
  assert.equal(outrunsTheHour(rehearsal(2, 6000)), true, '100 minutes is past it');
});

test('a plan past the hour still gets a budget that covers it', () => {
  // The warning is about the plan, not about the budget being wrong: the budget
  // is still twice the projection, so the run is not set up to fail.
  const long = rehearsal(2, 4200);
  assert.equal(outrunsTheHour(long), true);
  assert.ok(long.budgetWallSecs > long.projectedWallSecs, 'the budget covers the plan');
  assert.equal(long.budgetWallSecs, 8400);
});

/* ------------------------------------------------------------------ *
 * The measured budget
 * ------------------------------------------------------------------ */

test('the review says whether the per-step figure was measured or assumed', () => {
  // The budget used to be derived from a constant that understated a real task by
  // more than half. A projection built on a guess and one built on this machine's
  // own history are different claims, and the user is deciding whether to spend a
  // run on the second one.
  assert.equal(perStepLabel(rehearsal(2, 1200, 600, false)), '10m per step, assumed');
  assert.equal(perStepLabel(rehearsal(2, 1200, 1620, true)), '27m per step, measured here');
});

test('the derived budget moves when the median moves', () => {
  // The fourth proof, on the panel's side. The observed run's worker turn was 6
  // minutes and its verifier's 20.7, so a task-pair is about 27 minutes — 1620
  // seconds, not the 600 the constant assumed. At that figure the same five-deep
  // plan projects at 135 minutes instead of 50.
  const assumed = rehearsal(2, 5 * 600, 600, false);
  const measured = rehearsal(2, 5 * 1620, 1620, true);

  assert.equal(assumed.projectedWallSecs, 3000);
  assert.equal(measured.projectedWallSecs, 8100);
  assert.ok(
    measured.budgetWallSecs > assumed.budgetWallSecs,
    'a slower workspace must get a longer clock',
  );
  assert.equal(outrunsTheHour(assumed), false, '50 minutes fits the hour');
  assert.equal(outrunsTheHour(measured), true, '135 minutes does not');
});

test('the per-role median is absent until two runs exist', () => {
  // One observation is not a median, and a review that showed a single run's
  // number under that word would be presenting an anecdote as a measurement.
  assert.equal(roleMedianLine([]), null);
});

test('the per-role median names each role and the weakest run count behind it', () => {
  // The observed run's verifier model took 20.7 minutes against the worker's 6.
  // A user choosing a verifier model should see that number before the run.
  const line = roleMedianLine([roleMedian('worker', 360), roleMedian('verifier', 1242)]);
  assert.equal(line, 'per turn, median over 2 runs: worker 6m · verifier 20m');

  // The count is the smallest behind any one role: a role measured over two runs
  // beside one measured over five is a line whose weakest figure is the two.
  const uneven = roleMedianLine([roleMedian('worker', 360, 5), roleMedian('verifier', 1242, 2)]);
  assert.match(uneven ?? '', /median over 2 runs/);
});

/* ------------------------------------------------------------------ *
 * The graph
 * ------------------------------------------------------------------ */

test('orderedTasks puts a dependency before its dependent', () => {
  const tasks = [task('t3', ['t1', 't2']), task('t1'), task('t2', ['t1'])];
  const order = orderedTasks(tasks).map((t) => t.id);
  assert.ok(order.indexOf('t1') < order.indexOf('t2'), order.join(','));
  assert.ok(order.indexOf('t2') < order.indexOf('t3'), order.join(','));
  assert.equal(order.length, 3);
});

test('orderedTasks terminates on a cycle instead of hanging', () => {
  const tasks = [task('t1', ['t2']), task('t2', ['t1'])];
  const order = orderedTasks(tasks).map((t) => t.id);
  assert.equal(order.length, 2);
});

test('orderedTasks keeps every task exactly once', () => {
  const tasks = [task('t1'), task('t2', ['t1']), task('t3', ['t1']), task('t4', ['t2', 't3'])];
  const order = orderedTasks(tasks).map((t) => t.id);
  assert.equal(new Set(order).size, 4);
  assert.equal(order.length, 4);
});

test('edges drops a dependency that names a task the plan does not have', () => {
  const tasks = [task('t1', ['ghost']), task('t2', ['t1'])];
  const links = edges(tasks);
  assert.deepEqual(links, [{ from: 't1', to: 't2' }]);
});

test('edges is empty for a plan with no dependencies', () => {
  assert.deepEqual(edges([task('t1'), task('t2')]), []);
});

/* ------------------------------------------------------------------ *
 * The board
 * ------------------------------------------------------------------ */

test('a board entry carries the fields the panel reads', () => {
  const entry: BoardEntry = {
    seq: 1,
    at: '2026-01-01T00:00:00.000Z',
    from: 'w-t1',
    to: [],
    kind: 'decision',
    text: 'chose a token bucket',
    task: 't1',
    evidence: [],
  };
  assert.equal(boardKindTone(entry.kind), 'live');
  assert.equal(entry.to.length, 0);
});

/* ------------------------------------------------------------------ *
 * The strip in the conversation
 * ------------------------------------------------------------------ */

test('every run state has a sentence, and no two read the same', () => {
  const states: Run['state'][] = [
    'planning',
    'awaiting-approval',
    'running',
    'integrating',
    'reflecting',
    'done',
    'failed',
    'replanning',
    'escalated',
    'cancelled',
    'interrupted',
  ];
  const sentences = states.map((state) => describeRunState(state));
  for (const [index, sentence] of sentences.entries()) {
    assert.ok(sentence.length > 0, `${states[index]} has no sentence`);
    // The strip is one line above the composer; a sentence that needs a second
    // line is a sentence that will be truncated.
    assert.ok(sentence.length <= 28, `${states[index]} is too long: ${sentence}`);
  }
  assert.equal(new Set(sentences).size, states.length, 'two states read the same');
});

test('every task state has a sentence, and no two read the same', () => {
  const states: Task['state'][] = [
    'blocked',
    'ready',
    'running',
    'verifying',
    'verified',
    'failed',
    'conflict',
  ];
  const sentences = states.map((state) => describeTaskState(state));
  for (const [index, sentence] of sentences.entries()) {
    assert.ok(sentence.length > 0, `${states[index]} has no sentence`);
  }
  assert.equal(new Set(sentences).size, states.length, 'two task states read the same');
});

test('blocked and ready are told apart, because they look identical in a list', () => {
  // One is waiting on a dependency, the other on a slot. A panel that says
  // "waiting" for both hides the only thing the user can act on.
  assert.notEqual(describeTaskState('blocked'), describeTaskState('ready'));
});

/* ------------------------------------------------------------------ *
 * describeTask
 * ------------------------------------------------------------------ */

test('a blocked task names the dependency it waits on and what that one is doing', () => {
  // The state word alone says "waiting on a dependency" and stops there, which
  // leaves the user to work out which one by reading the graph. The name and the
  // dependency's own state are the two facts that answer it.
  const tasks = [task('t1', [], 'running'), task('t2', ['t1'], 'blocked')];
  const text = describeTask(tasks[1]!, tasks);
  assert.match(text, /t1/, 'the dependency is named');
  assert.match(text, /implementing/, "the dependency's state is named");
  assert.doesNotMatch(text, /t2/, 'the blocked task does not name itself');
});

test('the dependency named is the one that has not committed, not the first listed', () => {
  // t1 is verified, so t3 is not waiting on it. Naming t1 would send the user to
  // look at work that is already done.
  const tasks = [
    task('t1', [], 'verified'),
    task('t2', [], 'running'),
    task('t3', ['t1', 't2'], 'blocked'),
  ];
  const text = describeTask(tasks[2]!, tasks);
  assert.match(text, /t2/);
  assert.doesNotMatch(text, /t1/);
});

test('a dependency that is verifying counts as committed, so it is not the one named', () => {
  // A worker posts its evidence and stops, so the commit exists and a dependent
  // may build on it. Waiting for the verdict would put verification between every
  // link of a chain.
  const tasks = [
    task('t1', [], 'verifying'),
    task('t2', [], 'running'),
    task('t3', ['t1', 't2'], 'blocked'),
  ];
  assert.match(describeTask(tasks[2]!, tasks), /t2/);
});

test('a dependency the plan no longer carries is named as missing, not dropped', () => {
  // A blocked task waiting on nothing is a plan that disagrees with itself, and
  // that is worth seeing rather than smoothing over.
  const tasks = [task('t2', ['t9'], 'blocked')];
  const text = describeTask(tasks[0]!, tasks);
  assert.match(text, /t9/);
  assert.match(text, /not in the plan/);
});

test('a blocked task with no dependencies falls back to the general sentence', () => {
  const tasks = [task('t1', [], 'blocked')];
  assert.equal(describeTask(tasks[0]!, tasks), describeTaskState('blocked'));
});

test('every other state reads exactly as the state word does', () => {
  // `describeTask` adds a name to one state and changes nothing about the rest.
  const states: Task['state'][] = ['ready', 'running', 'verifying', 'verified', 'failed', 'conflict'];
  for (const state of states) {
    const tasks = [task('t1', ['t0'], state)];
    assert.equal(describeTask(tasks[0]!, tasks), describeTaskState(state), state);
  }
});

test('a live state never reads as a finished one', () => {
  const live: Run['state'][] = [
    'planning',
    'awaiting-approval',
    'running',
    'integrating',
    'reflecting',
    'replanning',
  ];
  const finished = ['done', 'failed', 'escalated', 'cancelled', 'interrupted'];
  const liveWords = new Set(live.map((state) => describeRunState(state)));
  for (const state of finished) {
    assert.ok(!liveWords.has(describeRunState(state)), `${state} reads as live`);
  }
});

test('escalated reads as a question, not as a crash', () => {
  // A run that stopped because a member asked something is not a run that went
  // wrong. The strip is the only place the user sees the difference, so the two
  // must not share a word.
  assert.notEqual(describeRunState('escalated'), describeRunState('failed'));
  assert.notEqual(describeRunState('escalated'), describeRunState('cancelled'));
  assert.ok(isTerminal('escalated'), 'a stopped run holds no processes');
  assert.ok(!isLive('escalated'));
  assert.equal(runTone('escalated'), 'idle');
});

test('a task that has not started has no duration', () => {
  // Zero, not "now minus the epoch". A blocked task has not been waiting since
  // 1970 and the graph must not say it has.
  const task = { startedAt: null, endedAt: null } as never;
  assert.equal(taskDuration(task), 0);
});

test('a running task is measured to now, a finished one to its own end', () => {
  const started = '2026-11-18T20:00:00.000Z';
  const running = { startedAt: started, endedAt: null } as never;
  const at = new Date('2026-11-18T20:02:00.000Z').getTime();
  assert.equal(taskDuration(running, at), 120);

  const finished = { startedAt: started, endedAt: '2026-11-18T20:01:00.000Z' } as never;
  // The end time wins even when "now" is much later: a finished task's duration
  // must stop moving, or the graph rewrites history on every render.
  assert.equal(taskDuration(finished, at), 60);
});

test('a task with an unreadable timestamp reports no duration rather than NaN', () => {
  const task = { startedAt: 'not a date', endedAt: null } as never;
  assert.equal(taskDuration(task), 0);
});

/* ------------------------------------------------------------------ *
 * activityTone
 * ------------------------------------------------------------------ */

test('a feed line is toned by its status, not by its kind', () => {
  // A tool call that is running and one that failed are the same kind of line
  // and mean opposite things. The status is the only field that tells them
  // apart, so it is the only field the tone may read.
  const item = (status: string): ActivityItem => ({
    at: '2026-01-01T00:00:00.000Z',
    kind: 'tool-call',
    title: 'bash',
    status,
    id: 'c1',
    tokens: 0,
  });
  assert.equal(activityTone(item('in-progress')), 'live');
  assert.equal(activityTone(item('pending')), 'live');
  assert.equal(activityTone(item('completed')), 'good');
  assert.equal(activityTone(item('failed')), 'bad');
  assert.equal(activityTone(item('said')), 'idle');
  assert.equal(activityTone(item('answered')), 'idle');
});
