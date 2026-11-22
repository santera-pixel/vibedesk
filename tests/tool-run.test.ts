/**
 * Unit tests for the "on it" bar's state.
 *
 * The bar folds a run of tool calls into one dim line, and that line is the only
 * thing the transcript shows while the agent works. Two claims have to hold
 * without the bar being opened: that it is live while a call is unsettled, and
 * that it says when something failed.
 *
 * The first version of this got the live claim wrong — it tested for `pending`
 * alone, and a call that is actually running reports `in_progress`, so the bar
 * was permanently settled and the pulsing dot never appeared. A browser test
 * caught it only because the harness happened to sample the turn while a call
 * was open. These cases pin the predicate down without a browser.
 *
 * Run with `npm run test:unit`.
 */

import assert from 'node:assert/strict';
import { test } from 'node:test';

import { toolRunLabels, toolRunState } from '../src/lib/tool-run.ts';

test('a run with a call in progress is live', () => {
  const state = toolRunState([{ status: 'completed' }, { status: 'in_progress' }]);
  assert.equal(state.live, true);
  assert.equal(state.count, 2);
  assert.equal(state.failed, 0);
});

test('a call that has only been announced is live', () => {
  assert.equal(toolRunState([{ status: 'pending' }]).live, true);
});

test('a call with no status yet is live', () => {
  // The store adds the call before the agent reports anything about it.
  assert.equal(toolRunState([{}]).live, true);
});

test('a run whose calls all completed is settled', () => {
  const state = toolRunState([{ status: 'completed' }, { status: 'completed' }]);
  assert.equal(state.live, false);
  assert.equal(state.count, 2);
});

test('a failed call is settled, not live', () => {
  // A run whose only call failed is done and broken. Showing a pulse would say
  // the agent is still working on it.
  const state = toolRunState([{ status: 'failed' }]);
  assert.equal(state.live, false);
  assert.equal(state.failed, 1);
});

test('a failure is counted alongside a call still running', () => {
  const state = toolRunState([
    { status: 'failed' },
    { status: 'in_progress' },
    { status: 'completed' },
  ]);
  assert.equal(state.live, true);
  assert.equal(state.failed, 1);
  assert.equal(state.count, 3);
});

test('every failure is counted', () => {
  const state = toolRunState([
    { status: 'failed' },
    { status: 'failed' },
    { status: 'completed' },
  ]);
  assert.equal(state.failed, 2);
  assert.equal(state.live, false);
});

test('an empty run is settled and empty', () => {
  // The bar is not rendered for an empty run, but the predicate must not claim
  // it is live if it ever is.
  assert.deepEqual(toolRunState([]), { live: false, count: 0, failed: 0 });
});

test('toolRunLabels names each call, falling back to its kind', () => {
  const { labels, hidden } = toolRunLabels([
    { title: 'read skate.cpp', kind: 'read' },
    { kind: 'execute' },
    { title: '  ', kind: 'search' },
  ]);
  assert.deepEqual(
    labels.map((label) => label.text),
    ['read skate.cpp', 'shell', 'search'],
  );
  assert.equal(hidden, 0);
});

test('toolRunLabels truncates a long title rather than wrapping the bar', () => {
  const long = 'Read the file at src-tauri/src/commands/workspace.rs and report';
  const { labels } = toolRunLabels([{ title: long }], 4, 20);
  assert.equal(labels[0]?.text.length, 20);
  assert.ok(labels[0]?.text.endsWith('…'));
  assert.ok(long.startsWith(labels[0]?.text.slice(0, -1) ?? ''));
});

test('toolRunLabels counts the tail instead of listing it', () => {
  const calls = Array.from({ length: 9 }, (_, index) => ({ title: `call ${index}` }));
  const { labels, hidden } = toolRunLabels(calls, 4);
  assert.equal(labels.length, 4);
  assert.equal(hidden, 5);
});

test('toolRunLabels marks a failed call so the bar can colour it', () => {
  const { labels } = toolRunLabels([
    { title: 'build', status: 'failed' },
    { title: 'test', status: 'completed' },
  ]);
  assert.equal(labels[0]?.failed, true);
  assert.equal(labels[1]?.failed, false);
});

test('toolRunLabels survives an empty run and a zero limit', () => {
  assert.deepEqual(toolRunLabels([]), { labels: [], hidden: 0 });
  assert.deepEqual(toolRunLabels([{ title: 'a' }], 0), { labels: [], hidden: 1 });
});
