'use strict';

/**
 * Session-start catalog.
 *
 * A chip row is a GUI affordance and Vibe has no GUI plugin surface, so the
 * nearest honest equivalent is a few lines of context: which bundles exist,
 * which are on, and which tool does what. Without it the model has to notice
 * five MCP tools among forty and guess their purpose.
 *
 * Failure mode is deliberate: anything unexpected exits 0 with no stdout, which
 * Vibe reads as "pass through". A broken catalog must never cost the user a
 * session.
 */

const config = require('../server/config');
const bundles = require('../server/bundles');
const state = require('../server/state');

function build() {
  const cfg = config.loadConfig();
  if (!cfg.bundles.length) return '';

  const session = state.load();
  const rows = [];

  for (const bundle of cfg.bundles) {
    const on = bundle.enabled !== false && state.isOn(session, bundle.id);
    const skills = bundles.discover(bundle.root);
    const count =
      skills.length === 0
        ? bundle.authored
          ? 'no skills found'
          : 'not cloned'
        : `${skills.length} skill${skills.length === 1 ? '' : 's'}`;
    rows.push(`  ${on ? 'ON ' : 'OFF'} ${bundle.label} (${bundle.id}) — ${count}`);
  }

  return [
    '[mastery] Skill bundles are installed. Their skills are not in the skill catalog: load one and the body arrives as a tool result.',
    ...rows,
    '  mcp_mastery.bundles — full list, source status, what has been loaded',
    '  mcp_mastery.find {query} — search names and descriptions',
    '  mcp_mastery.load {bundle, skill} — pull one skill body into context',
    '  mcp_mastery.toggle {bundle, on} — switch a bundle for this conversation',
    '  mcp_mastery.update {action} — check or fast-forward the clones',
  ].join('\n');
}

function main() {
  let text = '';
  try {
    text = build();
  } catch {
    process.exit(0);
  }

  if (!text) process.exit(0);

  process.stdout.write(`${JSON.stringify({ hook_specific_output: { additional_context: text } })}\n`);
}

// Drain stdin so the parent's write never blocks, then answer.
process.stdin.resume();
process.stdin.on('end', main);
process.stdin.on('error', () => process.exit(0));
