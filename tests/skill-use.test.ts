import assert from 'node:assert/strict';
import { test } from 'node:test';
import { skillLoads, skillsUsed } from '../src/lib/skill-use.ts';
import type { ToolCall } from '../src/types/acp.ts';

const call = (over: Partial<ToolCall>): ToolCall =>
  ({ toolCallId: 'c1', ...over }) as ToolCall;

test('a mastery load names the skill and its bundle', () => {
  const loads = skillLoads(
    call({ title: 'mcp_mastery.load', rawInput: { bundle: 'vibe-mastery', skill: 'assay' } }),
  );
  assert.deepEqual(loads, [{ name: 'assay', bundle: 'vibe-mastery' }]);
});

test('the CLI skill tool names the skill', () => {
  const loads = skillLoads(call({ title: 'skill', rawInput: { name: 'using-aegis' } }));
  assert.deepEqual(loads, [{ name: 'using-aegis', bundle: null }]);
});

test('a read with a name argument is not a skill load', () => {
  assert.deepEqual(skillLoads(call({ title: 'read', rawInput: { name: 'skate.cpp' } })), []);
});

test('a call with no arguments is not a skill load', () => {
  assert.deepEqual(skillLoads(call({ title: 'mcp_mastery.load' })), []);
  assert.deepEqual(skillLoads(call({ title: 'skill', rawInput: null })), []);
});

test('a blank skill name is refused rather than shown as an empty chip', () => {
  assert.deepEqual(skillLoads(call({ title: 'skill', rawInput: { name: '   ' } })), []);
});

test('skillsUsed deduplicates and keeps the first order', () => {
  const used = skillsUsed([
    call({ toolCallId: 'a', title: 'skill', rawInput: { name: 'assay' } }),
    call({ toolCallId: 'b', title: 'read', rawInput: { path: 'x' } }),
    call({ toolCallId: 'c', title: 'skill', rawInput: { name: 'assay' } }),
    call({ toolCallId: 'd', title: 'skill', rawInput: { name: 'ui-ux-governance' } }),
  ]);
  assert.deepEqual(
    used.map((entry) => entry.name),
    ['assay', 'ui-ux-governance'],
  );
});

test('skillsUsed on an empty run is empty', () => {
  assert.deepEqual(skillsUsed([]), []);
});

test('the app server title is read when rawInput is not forwarded', () => {
  const loads = skillLoads(call({ title: 'Loading skill: vibe-mastery:assay' }));
  assert.deepEqual(loads, [{ name: 'assay', bundle: 'vibe-mastery' }]);
});

test('a qualified name is split into the catalog name and its bundle', () => {
  assert.deepEqual(skillLoads(call({ title: 'skill', rawInput: { name: 'vibe:create-plugin' } })), [
    { name: 'create-plugin', bundle: 'vibe' },
  ]);
  assert.deepEqual(skillLoads(call({ title: 'skill', rawInput: { name: 'assay' } })), [
    { name: 'assay', bundle: null },
  ]);
});

test('a title that only mentions a skill is not a load', () => {
  assert.deepEqual(skillLoads(call({ title: 'Reading skill: assay' })), []);
  assert.deepEqual(skillLoads(call({ title: 'Loading skill: ' })), []);
  assert.deepEqual(skillLoads(call({ title: 'bash', rawInput: { command: 'ls' } })), []);
});

test('the title form and the rawInput form agree on the same load', () => {
  const fromTitle = skillLoads(call({ title: 'Loading skill: vibe-mastery:assay' }));
  const fromInput = skillLoads(
    call({ title: 'mcp_mastery.load', rawInput: { bundle: 'vibe-mastery', skill: 'assay' } }),
  );
  assert.deepEqual(fromTitle, fromInput);
});

test('reading a skill file is a load', () => {
  const loads = skillLoads(
    call({
      title: 'read',
      kind: 'read',
      rawInput: { path: 'C:\\Users\\Santera\\.vibe\\skills\\systematic-debugging\\SKILL.md' },
    }),
  );
  assert.deepEqual(loads, [{ name: 'systematic-debugging', bundle: null }]);
});

test('a mastery skill file is a load to the bare name', () => {
  const loads = skillLoads(
    call({
      title: 'read',
      rawInput: {
        path: 'C:/Users/Santera/.vibe/mastery/sources/Anti-Slop/skills/antislop/SKILL.md',
      },
    }),
  );
  assert.deepEqual(loads, [{ name: 'antislop', bundle: null }]);
});

test('a location is enough when rawInput is not forwarded', () => {
  const loads = skillLoads(
    call({
      title: 'read',
      locations: [{ path: 'C:/Users/Santera/.vibe/skills/assay/SKILL.md' }],
    }),
  );
  assert.deepEqual(loads, [{ name: 'assay', bundle: null }]);
});

test('a shell command that opens a skill file is a load', () => {
  const loads = skillLoads(
    call({
      title: 'bash',
      rawInput: { command: 'cat "C:/Users/Santera/.vibe/skills/assay/SKILL.md"' },
    }),
  );
  assert.deepEqual(loads, [{ name: 'assay', bundle: null }]);
});

test('a reference file inside a skill directory is a load', () => {
  const loads = skillLoads(
    call({
      title: 'read',
      rawInput: { path: 'C:/Users/Santera/.vibe/skills/3dviz-pro-max/reference/materials.md' },
    }),
  );
  assert.deepEqual(loads, [{ name: '3dviz-pro-max', bundle: null }]);
});

test('prose that mentions a skill file is not a load', () => {
  assert.deepEqual(
    skillLoads(call({ title: 'write', rawInput: { content: 'Read the SKILL.md file first.' } })),
    [],
  );
});

test('an ordinary markdown read is not a load', () => {
  assert.deepEqual(
    skillLoads(
      call({ title: 'read', rawInput: { path: 'D:/VibeDesk/VibeDeskProjects/orbital/README.md' } }),
    ),
    [],
  );
});
