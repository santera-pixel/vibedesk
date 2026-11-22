/**
 * Unit tests for what Enter does in the slash palette.
 *
 * The palette completes a command into the composer. For a command that takes an
 * argument that is the right thing: `/mcp` becomes `/mcp ` and the user types
 * `status`. For a command that takes none it was a no-op that ate the keypress —
 * `/status` completed into `/status`, the palette closed, and nothing ran. Enter
 * looked dead until a second press, which is how it was found: a script typed
 * `/capabilities`, pressed Enter, and the text was still sitting in the box.
 *
 * The cases below pin the decision down without a browser, and the last two run
 * against the real catalogue so that adding an `inputHint` to a command that has
 * no argument fails here rather than in someone's hands.
 *
 * Run with `npm run test:unit`.
 */

import assert from 'node:assert/strict';
import { test } from 'node:test';

import { filterCommands, slashEnterAction, type SlashCommand } from '../src/lib/commands.ts';

const complete: SlashCommand = {
  name: 'status',
  description: 'Agent statistics for this session',
  group: 'context',
  transport: 'local',
  action: 'open-status',
};

const takesInput: SlashCommand = {
  name: 'mcp',
  description: 'Show MCP server and connector status',
  group: 'config',
  transport: 'agent',
  inputHint: 'status | login <alias> | logout <alias>',
};

test('Enter runs a command that takes no argument', () => {
  const action = slashEnterAction([complete], 0);
  assert.equal(action.kind, 'run');
  assert.equal(action.kind === 'run' && action.command.name, 'status');
});

test('Enter completes a command that takes an argument', () => {
  // The next thing the user types is that argument, so running it here would
  // send `/mcp` with nothing after it.
  assert.equal(slashEnterAction([takesInput], 0).kind, 'accept');
});

test('Enter runs the highlighted command, not the first one', () => {
  const action = slashEnterAction([takesInput, complete], 1);
  assert.equal(action.kind, 'run');
  assert.equal(action.kind === 'run' && action.command.name, 'status');
});

test('Enter completes when the highlight is on a command that takes an argument', () => {
  assert.equal(slashEnterAction([complete, takesInput], 1).kind, 'accept');
});

test('Enter completes when there is nothing to run', () => {
  // The palette can be open with no results; the caller guards on that, but the
  // decision must not invent a command out of an empty list.
  assert.equal(slashEnterAction([], 0).kind, 'accept');
});

test('Enter completes when the highlight is past the end of the list', () => {
  assert.equal(slashEnterAction([complete], 4).kind, 'accept');
});

test('the real catalogue: /status runs on Enter', () => {
  const results = filterCommands('status');
  assert.equal(results[0]?.name, 'status');
  assert.equal(slashEnterAction(results, 0).kind, 'run');
});

test('the real catalogue: /mcp completes on Enter', () => {
  const results = filterCommands('mcp');
  assert.equal(results[0]?.name, 'mcp');
  assert.equal(slashEnterAction(results, 0).kind, 'accept');
});

test('the real catalogue: every command without an inputHint runs', () => {
  // A command with no argument has nothing to complete, so Enter must run it.
  // This is the invariant the bug violated, checked over the whole catalogue
  // rather than the two commands that happened to be typed.
  const bare = filterCommands('').filter((c) => !c.inputHint);
  assert.ok(bare.length > 10, 'the catalogue should have bare commands');
  for (const command of bare) {
    const action = slashEnterAction([command], 0);
    assert.equal(action.kind, 'run', `/${command.name} should run on Enter`);
  }
});
