import assert from 'node:assert/strict';
import { test } from 'node:test';
import {
  formatClock,
  formatDuration,
  formatSpan,
  turnElapsed,
} from '../src/lib/time.ts';

/* A fixed local clock, so the assertions do not depend on the machine's zone. */
const at = (y: number, m: number, d: number, h: number, min: number, s: number) =>
  new Date(y, m - 1, d, h, min, s).getTime();

test('formatClock shows seconds for today', () => {
  const now = at(2026, 3, 14, 18, 0, 0);
  assert.equal(formatClock(at(2026, 3, 14, 14, 32, 7), now), '14:32:07');
  assert.equal(formatClock(at(2026, 3, 14, 0, 0, 0), now), '00:00:00');
  assert.equal(formatClock(at(2026, 3, 14, 23, 59, 59), now), '23:59:59');
});

test('formatClock dates anything not from today', () => {
  const now = at(2026, 3, 14, 18, 0, 0);
  assert.equal(formatClock(at(2026, 3, 13, 14, 32, 7), now), 'Mar 13, 14:32:07');
  assert.equal(formatClock(at(2025, 12, 31, 9, 5, 1), now), 'Dec 31, 09:05:01');
});

test('formatClock survives a bad timestamp', () => {
  assert.equal(formatClock(Number.NaN), '');
});

test('formatDuration keeps sub-second precision', () => {
  assert.equal(formatDuration(0), '0.0s');
  assert.equal(formatDuration(420), '0.4s');
  assert.equal(formatDuration(999), '1.0s');
});

test('formatDuration rounds to whole seconds past one', () => {
  assert.equal(formatDuration(1000), '1s');
  assert.equal(formatDuration(12_400), '12s');
  assert.equal(formatDuration(59_600), '1m');
  assert.equal(formatDuration(134_000), '2m 14s');
  assert.equal(formatDuration(3_600_000), '1h');
  assert.equal(formatDuration(3_840_000), '1h 04m');
});

test('formatDuration refuses nonsense', () => {
  assert.equal(formatDuration(-1), '—');
  assert.equal(formatDuration(Number.NaN), '—');
});

test('formatSpan is coarser than formatDuration', () => {
  assert.equal(formatSpan(0), '0s');
  assert.equal(formatSpan(41_000), '41s');
  assert.equal(formatSpan(41 * 60_000), '41m');
  assert.equal(formatSpan(2 * 3_600_000 + 14 * 60_000), '2h 14m');
  assert.equal(formatSpan(2 * 3_600_000), '2h');
  assert.equal(formatSpan(3 * 86_400_000 + 4 * 3_600_000), '3d 4h');
  assert.equal(formatSpan(3 * 86_400_000), '3d');
});

test('turnElapsed ticks while running and freezes when settled', () => {
  const startedAt = at(2026, 3, 14, 14, 0, 0);
  assert.equal(turnElapsed(null), null);
  assert.equal(turnElapsed({ startedAt, endedAt: null }, startedAt + 5000), 5000);
  assert.equal(turnElapsed({ startedAt, endedAt: startedAt + 9000 }, startedAt + 60_000), 9000);
});

test('turnElapsed never goes negative on a clock that stepped back', () => {
  const startedAt = at(2026, 3, 14, 14, 0, 0);
  assert.equal(turnElapsed({ startedAt, endedAt: null }, startedAt - 5000), 0);
});
