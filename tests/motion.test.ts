/**
 * Unit tests for the motion primitives.
 *
 * `springToLinear` is the one piece of the motion vocabulary that is arithmetic
 * rather than CSS, and it is the piece a stylesheet cannot check: a curve that
 * starts at 0.4 or never reaches 1 is a curve that makes every arrival in the
 * app look wrong, and it would still parse. So the shape is asserted here.
 *
 * Run with `npm run test:unit`.
 */

import assert from 'node:assert/strict';
import { test } from 'node:test';

import { SPRINGS, springToLinear, stagger } from '../src/lib/motion.ts';

/** The numbers inside a `linear(...)` string, in order. */
function samples(curve: string): number[] {
  const inner = curve.slice('linear('.length, -1);
  return inner.split(',').map((part) => Number.parseFloat(part.trim()));
}

test('a spring is a linear() easing', () => {
  const curve = springToLinear(SPRINGS.soft);
  assert.match(curve, /^linear\(/);
  assert.match(curve, /\)$/);
});

test('a spring starts at 0 and ends at 1', () => {
  const values = samples(springToLinear(SPRINGS.soft));
  assert.equal(values[0], 0);
  assert.equal(values[values.length - 1], 1);
});

test('a spring is monotonic in its first half', () => {
  // The rise has to be a rise. A curve that dips before it climbs reads as a
  // stutter, and it is the failure a wrong sign in the oscillator produces.
  const values = samples(springToLinear(SPRINGS.soft));
  const half = Math.floor(values.length / 2);
  for (let index = 1; index < half; index += 1) {
    assert.ok(values[index] >= values[index - 1], `dipped at ${index}`);
  }
});

test('an underdamped spring overshoots', () => {
  const values = samples(springToLinear(SPRINGS.snappy));
  assert.ok(Math.max(...values) > 1, 'the curve passes its target');
});

test('a critically damped spring does not overshoot', () => {
  const values = samples(springToLinear({ stiffness: 120, damping: 22, mass: 1 }));
  assert.ok(Math.max(...values) <= 1.0001, 'the curve arrives without passing');
});

test('the sample count is honoured', () => {
  assert.equal(samples(springToLinear({ ...SPRINGS.soft, samples: 12 })).length, 12);
  assert.equal(samples(springToLinear({ ...SPRINGS.soft, samples: 64 })).length, 64);
});

test('the curve depends on the damping ratio, not on stiffness alone', () => {
  // The samples are normalised to the settle window, so two springs with the
  // same damping ratio produce the same curve and differ only in how long it
  // takes. Stiffness alone must not change the shape.
  const soft = springToLinear({ stiffness: 120, damping: 16, mass: 1 });
  const stiff = springToLinear({ stiffness: 480, damping: 32, mass: 1 });
  assert.equal(soft, stiff);

  // A different ratio is a different curve.
  const looser = springToLinear({ stiffness: 120, damping: 8, mass: 1 });
  assert.notEqual(looser, soft);
  assert.ok(Math.max(...samples(looser)) > Math.max(...samples(soft)), 'looser overshoots more');
});

test('stagger writes the index as a custom property', () => {
  assert.deepEqual(stagger(0), { '--vd-i': '0' });
  assert.deepEqual(stagger(7), { '--vd-i': '7' });
});
