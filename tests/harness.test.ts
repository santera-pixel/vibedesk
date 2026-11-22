/**
 * Unit tests for telling a harness diagnostic apart from a model reply.
 *
 * The case that motivated this: the agent reports a failed connector fetch as
 * an assistant message, so every new chat opened with "Couldn't load your
 * connectors: Failed to load connectors (HTTP 502)" as if the model had said
 * it. A reply requires a prompt, so the check is structural.
 */
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { isStartupDiagnostic } from '../src/lib/harness.ts';

test('an empty transcript is a startup diagnostic', () => {
  assert.equal(isStartupDiagnostic([]), true);
});

test('a transcript holding only notices and chips is still a startup', () => {
  const items = [
    { type: 'notice' },
    { type: 'context' },
    { type: 'notice' },
  ];
  assert.equal(isStartupDiagnostic(items), true);
});

test('a user turn means the agent is replying, not reporting', () => {
  assert.equal(isStartupDiagnostic([{ type: 'user' }]), false);
});

test('an agent turn means the session has already started speaking', () => {
  assert.equal(isStartupDiagnostic([{ type: 'agent' }]), false);
});

test('a trimmed long chat keeps both speakers and is never mistaken for a fresh one', () => {
  const items = [
    { type: 'user' },
    { type: 'agent' },
    { type: 'tools' },
    { type: 'reasoning' },
  ];
  assert.equal(isStartupDiagnostic(items), false);
});

test('a reasoning block alone does not count as the session speaking', () => {
  // Reasoning is the model thinking, which also requires a prompt — but the
  // reducer only asks this question for message chunks, so a lone reasoning
  // row must not silently promote a diagnostic into the transcript.
  assert.equal(isStartupDiagnostic([{ type: 'reasoning' }]), true);
});
