/**
 * Unit tests for the method block in the preamble.
 *
 * Aegis and the mastery bundles are installed on this machine, and neither is
 * discoverable from inside a session: Aegis is a skill the agent has to route
 * through, and the mastery skills are not in the skill catalog at all. The
 * preamble is the only channel the client has to say so, so the block has to be
 * present in every preamble, survive the strip that a resumed chat performs, and
 * not be mistaken for part of what the user typed.
 *
 * Run with `npm run test:unit`.
 */

import assert from 'node:assert/strict';
import { test } from 'node:test';

import { preamble, stripPreamble, withPreamble } from '../src/lib/preamble.ts';

test('the method block names both halves of the requirement', () => {
  const text = preamble();
  assert.match(text, /using-aegis/);
  assert.match(text, /mcp_mastery\.load/);
});

test('the method block is in the preamble whatever the mode', () => {
  // Creator mode appends a paragraph; it must not be the only mode that carries
  // the method, because the requirement is about how the work is done, not
  // about what the work is.
  for (const creator of [false, true]) {
    assert.match(preamble({}, creator), /using-aegis/);
  }
});

test('the method block survives the strip a resumed chat performs', () => {
  // The strip cuts at the marker, so anything before it is the user's text and
  // anything after it is the harness's. The method block sits after, which is
  // what keeps it out of the composer when a chat is reopened.
  const sent = withPreamble('fix the parser', { pendingDir: '/tmp/pending' });
  assert.equal(stripPreamble(sent), 'fix the parser');
  assert.match(sent, /using-aegis/);
});

test('the method block does not claim the agent can apply a change', () => {
  // The contract's whole point is that the agent proposes and the user decides.
  // A method paragraph that reads like an instruction to act would undo it.
  const text = preamble();
  assert.match(text, /cannot apply/);
});

test('the capability block names every rewrite the agent will observe', () => {
  // Each of these is a way a tool result can come back changed. An agent that
  // does not recognise one re-reads the file to "see it properly", which is the
  // cost the capability exists to remove.
  const text = preamble();
  assert.match(text, /arrive as a card/);
  assert.match(text, /offset` and `limit/);
  assert.match(text, /\.\.\. omitted \.\.\./);
  assert.match(text, /saved to a file and you get a notice/);
  assert.match(text, /folded into summaries/);
  assert.match(text, /MCP tools, not shell commands/);
});

test('the capability block says the card is not a failure', () => {
  // The failure mode is the agent treating a card as a truncated read and
  // re-reading the whole file. Saying "not a failure" is the whole fix.
  const text = preamble();
  assert.match(text, /not a truncated read or a\s+failure/);
});

test('the method block says to load a skill before starting', () => {
  const text = preamble();
  assert.match(text, /before\s+the first edit/);
});
