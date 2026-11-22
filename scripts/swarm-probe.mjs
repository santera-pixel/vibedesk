/**
 * Drive the panel's projections against a real run document from disk.
 *
 * The unit tests use fixtures, and a fixture is a claim about the shape of the
 * data. This reads a run the engine actually wrote, so the claim is checked.
 */
import { readFileSync } from 'node:fs';
import { describeTask } from '../src/lib/swarm.ts';
import {
  droppedBefore,
  graphView,
  rosterView,
  spendView,
  threads,
} from '../src/lib/swarm-view.ts';

const dir = process.argv[2];
if (!dir) {
  console.error('usage: node scripts/swarm-probe.mjs <run-dir>');
  process.exit(2);
}

const run = JSON.parse(readFileSync(`${dir}/run.json`, 'utf8'));
const board = readFileSync(`${dir}/board.jsonl`, 'utf8')
  .split('\n')
  .filter((line) => line.trim().length > 0)
  .map((line) => JSON.parse(line));

const roster = rosterView(run, board);
const spend = spendView(run);
const graph = graphView(run);
const view = threads(board, run);

console.log(`run ${run.id}  state=${run.state}  tasks=${run.tasks.length}`);
console.log(`board ${board.length} entries, dropped ${droppedBefore(board)}`);
console.log('');
console.log('ROSTER');
console.log(`  lead ${roster.lead.member.id}: ${roster.lead.doing}`);
for (const group of roster.groups) {
  console.log(`  ${group.task?.id ?? 'unassigned'}  ${group.task?.title ?? ''}`);
  for (const agent of group.agents) {
    console.log(
      `    ${agent.member.id.padEnd(14)} ${agent.role.padEnd(9)} ${agent.doing.padEnd(28)} ` +
        `${agent.spend.tokens} tok  ${agent.lines} said`,
    );
  }
}
console.log(
  `  queued=${roster.queued} working=${roster.working} parked=${roster.parked} ` +
    `broken=${roster.broken} of ${roster.all.length}`,
);
console.log('');
console.log('SPEND');
console.log(
  `  ${spend.tokens} tokens, ${Math.round(spend.cacheRate * 100)}% cached, ` +
    `${spend.wallSecs}s wall, ${spend.turns} turns`,
);
for (const role of spend.roles) {
  console.log(`  ${role.label.padEnd(9)} ${role.tokens} tok  x${role.members}`);
}
console.log(`  per verified task: ${spend.perVerified ?? 'n/a'} (${spend.verified} verified)`);
console.log('');
console.log('GRAPH');
for (const node of graph.nodes) {
  console.log(
    `  ${node.task.id.padEnd(6)} depth=${node.depth} ${node.task.state.padEnd(9)} ` +
      `${node.duration}s${node.critical ? '  CRITICAL' : ''}`,
  );
  // What the node's aria-label and the plan's state chip say. A blocked task
  // that cannot name its dependency is the guess this phase removes.
  console.log(`         ${describeTask(node.task, run.tasks)}`);
}
console.log(`  critical path: ${graph.criticalPath.join(' -> ') || 'none'}`);
console.log(`  lanes: ${graph.lanes.length}`);
for (const lane of graph.lanes) {
  console.log(`    ${lane.index}: ${lane.spans.map((s) => s.task).join(', ')}`);
}
console.log(`  window: ${graph.window ? `${graph.window.start}..${graph.window.end}` : 'none'}`);
console.log('');
console.log('THREADS');
for (const thread of view) {
  console.log(
    `  ${(thread.task ?? 'run-wide').padEnd(8)} ${thread.entries.length} entries, ` +
      `${thread.exchanges.length} exchanges, kinds=${thread.kinds.join('/')}`,
  );
  for (const block of thread.blocks) {
    if (block.kind === 'exchange') {
      console.log(
        `    [${block.exchange.resolved ? 'answered' : 'OPEN'}] ` +
          `${block.exchange.opener.kind}: ${block.exchange.opener.text.slice(0, 70)}`,
      );
      for (const reply of block.exchange.replies) {
        console.log(`        ${reply.kind}: ${reply.text.slice(0, 70)}`);
      }
    } else {
      console.log(`    ${block.entry.kind}: ${block.entry.text.slice(0, 70)}`);
    }
  }
}
