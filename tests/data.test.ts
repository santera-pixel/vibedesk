/**
 * Unit tests for the data plane's frontend half.
 *
 * `src/lib/data.ts` is the module every panel binding goes through: it evaluates
 * the expression grammar, formats the result, and decides how much of a series
 * a chart is allowed to draw. Until now it was covered by the Rust vectors in
 * `src/lib/expr-vectors.json` (which prove the two implementations agree) and by
 * the live suite (which proves a panel renders). Neither of those can say what
 * `decimate` does to a spike, so this file does.
 *
 * Run with `npm run test:unit`. Node strips the types; the module's only import
 * is `import type`, so there is nothing to resolve and no build step.
 */

import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  MAX_SERIES,
  MISSING,
  decimate,
  display,
  evaluate,
  numeric,
  readPath,
  rows,
  series,
  seriesCount,
  truthy,
  visible,
} from '../src/lib/data.ts';

/* ---------------------------------------------------------------- *
 * readPath
 * ---------------------------------------------------------------- */

test('readPath walks objects and arrays', () => {
  const data = { status: { state: 'ready', pids: [11, 22] } };
  assert.equal(readPath(data, '/status/state'), 'ready');
  assert.equal(readPath(data, '/status/pids/1'), 22);
});

test('readPath returns MISSING for a path that is not there', () => {
  assert.equal(readPath({ a: 1 }, '/b'), MISSING);
  assert.equal(readPath({ a: 1 }, '/a/b'), MISSING);
});

/* ---------------------------------------------------------------- *
 * evaluate
 * ---------------------------------------------------------------- */

test('a literal evaluates to itself', () => {
  assert.equal(evaluate(42, {}), 42);
  assert.equal(evaluate('text', {}), 'text');
  assert.equal(evaluate(null, {}), null);
});

test('@path reads the model', () => {
  assert.equal(evaluate({ '@path': '/a/b' }, { a: { b: 7 } }), 7);
});

test('@exists and @missing are opposites', () => {
  const data = { a: 1 };
  assert.equal(evaluate({ '@exists': '/a' }, data), true);
  assert.equal(evaluate({ '@missing': '/a' }, data), false);
  assert.equal(evaluate({ '@exists': '/b' }, data), false);
  assert.equal(evaluate({ '@missing': '/b' }, data), true);
});

test('@if evaluates only the branch it takes', () => {
  // The untaken branch reads a path that does not exist. If it were evaluated
  // the result would still be right, so the assertion is on the value the taken
  // branch produces, not on the absence of a side effect.
  assert.equal(evaluate({ '@if': [true, 'yes', 'no'] }, {}), 'yes');
  assert.equal(evaluate({ '@if': [false, 'yes', 'no'] }, {}), 'no');
  assert.equal(evaluate({ '@if': [false, 1, 2, 3] }, {}), MISSING);
});

test('@and and @or short-circuit to a boolean', () => {
  assert.equal(evaluate({ '@and': [true, true] }, {}), true);
  assert.equal(evaluate({ '@and': [true, false] }, {}), false);
  assert.equal(evaluate({ '@or': [false, true] }, {}), true);
  assert.equal(evaluate({ '@or': [false, false] }, {}), false);
});

test('comparison operators compare numbers and strings', () => {
  assert.equal(evaluate({ '@eq': [2, 2] }, {}), true);
  assert.equal(evaluate({ '@ne': [2, 3] }, {}), true);
  assert.equal(evaluate({ '@gt': [3, 2] }, {}), true);
  assert.equal(evaluate({ '@gte': [2, 2] }, {}), true);
  assert.equal(evaluate({ '@lt': [1, 2] }, {}), true);
  assert.equal(evaluate({ '@lte': [2, 2] }, {}), true);
  assert.equal(evaluate({ '@lt': ['a', 'b'] }, {}), true);
});

test('arithmetic operators do arithmetic', () => {
  assert.equal(evaluate({ '@add': [2, 3] }, {}), 5);
  assert.equal(evaluate({ '@sub': [5, 3] }, {}), 2);
  assert.equal(evaluate({ '@mul': [4, 3] }, {}), 12);
  assert.equal(evaluate({ '@div': [9, 3] }, {}), 3);
});

test('division by zero is MISSING, not Infinity', () => {
  assert.equal(evaluate({ '@div': [1, 0] }, {}), MISSING);
});

test('an object with two keys is not an expression', () => {
  assert.equal(evaluate({ '@path': '/a', '@not': true }, { a: 1 }), MISSING);
});

test('@format renders through the formatter', () => {
  assert.equal(evaluate({ '@format': 'percent', '@of': 0.42 }, {}), '42%');
  assert.equal(evaluate({ '@format': 'duration', '@of': 90 }, {}), '1m 30s');
  assert.equal(evaluate({ '@format': 'duration', '@of': 3725 }, {}), '1h 2m');
  assert.equal(evaluate({ '@format': 'number', '@of': 1234 }, {}), '1,234');
});

test('@format of a missing value is a dash', () => {
  assert.equal(evaluate({ '@format': 'number', '@of': { '@path': '/nope' } }, {}), '—');
});

/* ---------------------------------------------------------------- *
 * truthy, visible, display, numeric
 * ---------------------------------------------------------------- */

test('truthy is strict: only true is true', () => {
  // Deliberate. A `when` guard is a boolean expression, and a guard that is
  // "truthy" because it happens to be a non-empty string is a guard that shows
  // a block the author did not mean to show. Everything that is not `true` is
  // false, including 1 and 'x'.
  assert.equal(truthy(true), true);
  assert.equal(truthy(false), false);
  assert.equal(truthy(0), false);
  assert.equal(truthy(1), false);
  assert.equal(truthy(''), false);
  assert.equal(truthy('x'), false);
  assert.equal(truthy(MISSING), false);
  assert.equal(truthy(null), false);
});

test('visible treats an absent condition as visible', () => {
  assert.equal(visible(null, {}), true);
  assert.equal(visible(undefined, {}), true);
  assert.equal(visible(false, {}), false);
  assert.equal(visible({ '@eq': [1, 1] }, {}), true);
});

test('display renders each kind of value', () => {
  assert.equal(display(MISSING), '—');
  assert.equal(display(null), '—');
  assert.equal(display('text'), 'text');
  assert.equal(display(1234), '1,234');
  assert.equal(display(true), 'yes');
  assert.equal(display(false), 'no');
  assert.equal(display([1, 2, 3]), '3 items');
});

test('numeric is null for anything that is not a number', () => {
  assert.equal(numeric(3), 3);
  assert.equal(numeric('3'), 3);
  assert.equal(numeric('three'), null);
  assert.equal(numeric(MISSING), null);
  assert.equal(numeric(null), null);
});

/* ---------------------------------------------------------------- *
 * decimate — the reason this file exists
 * ---------------------------------------------------------------- */

test('a series under the limit is returned untouched', () => {
  const points = [1, 2, 3, 4];
  assert.deepEqual(decimate(points), points);
});

test('decimate never returns more than the limit', () => {
  const points = Array.from({ length: 10_000 }, (_, index) => Math.sin(index / 40));
  const out = decimate(points, 512);
  assert.ok(out.length <= 512, `got ${out.length}`);
  assert.ok(out.length > 0);
});

test('decimate keeps the first and last sample', () => {
  const points = Array.from({ length: 5000 }, (_, index) => index);
  const out = decimate(points, 100);
  assert.equal(out[0], 0);
  assert.equal(out[out.length - 1], 4999);
});

test('decimate keeps a spike that a stride would drop', () => {
  // One sample in ten thousand is far above the rest. Taking every Nth point
  // would miss it; taking each bucket's extremes cannot.
  const points = new Array<number>(10_000).fill(0);
  points[4321] = 1000;
  const out = decimate(points, 200);
  assert.ok(out.includes(1000), 'the spike survived');
});

test('decimate keeps a trough as well as a spike', () => {
  const points = new Array<number>(10_000).fill(5);
  points[777] = -1000;
  const out = decimate(points, 200);
  assert.ok(out.includes(-1000), 'the trough survived');
});

test('decimate preserves order within a bucket', () => {
  // A bucket whose minimum comes before its maximum must emit them in that
  // order, or the line doubles back on itself.
  const points = [0, 10, 0, 10, 0, 10, 0, 10];
  const out = decimate(points, 4);
  assert.deepEqual(out, [0, 10, 0, 10, 0, 10, 0, 10].slice(0, out.length));
});

test('decimate of an empty array is empty', () => {
  assert.deepEqual(decimate([]), []);
});

/* ---------------------------------------------------------------- *
 * series, seriesCount, rows
 * ---------------------------------------------------------------- */

test('series drops non-numeric entries and caps the result', () => {
  assert.deepEqual(series([1, 'x', 2, null, 3]), [1, 2, 3]);
  assert.deepEqual(series('not an array'), []);
  assert.deepEqual(series(MISSING), []);
  const long = Array.from({ length: 5000 }, (_, index) => index);
  assert.ok(series(long).length <= MAX_SERIES);
});

test('seriesCount counts the numbers before decimation', () => {
  assert.equal(seriesCount([1, 'x', 2, null]), 2);
  assert.equal(seriesCount(Array.from({ length: 5000 }, (_, index) => index)), 5000);
  assert.equal(seriesCount('nope'), 0);
});

test('rows caps at 200 and reads objects by column', () => {
  const many = Array.from({ length: 500 }, (_, index) => ({ name: `row ${index}` }));
  assert.equal(rows(many, ['name']).length, 200);
  assert.deepEqual(rows([{ name: 'a' }, { name: 'b' }], ['name']), [['a'], ['b']]);
  assert.deepEqual(rows([[1, 2]], ['a', 'b']), [['1', '2']]);
  assert.deepEqual(rows([{ name: 'a' }], ['missing']), [['—']]);
});
