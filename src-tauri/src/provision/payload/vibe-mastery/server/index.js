'use strict';

/**
 * The MCP server.
 *
 * Newline-delimited JSON-RPC 2.0 over stdio, no dependencies, no build step.
 * stdout carries protocol frames and nothing else — a stray `console.log` here
 * corrupts the stream and the client reports the server as dead, so logging is
 * redirected to stderr at the top of this file.
 */

const fs = require('node:fs');
const readline = require('node:readline');

const config = require('./config');
const bundles = require('./bundles');
const git = require('./git');
const state = require('./state');

// Anything that tries to log goes to stderr. stdout is the wire.
console.log = console.error;
console.info = console.error;
console.debug = console.error;

const SUPPORTED_PROTOCOLS = ['2024-11-05', '2025-06-18'];
const FALLBACK_PROTOCOL = '2024-11-05';

/* ------------------------------------------------------------------ helpers */

function findBundle(cfg, key) {
  const needle = String(key ?? '').trim().toLowerCase();
  if (!needle) return null;
  return (
    cfg.bundles.find((b) => b.id.toLowerCase() === needle) ||
    cfg.bundles.find((b) => b.label.toLowerCase() === needle) ||
    cfg.bundles.find((b) => b.id.toLowerCase().startsWith(needle)) ||
    null
  );
}

function plural(n, one, many) {
  return `${n} ${n === 1 ? one : many}`;
}

function timeoutMs(cfg) {
  const seconds = Number(cfg.updates && cfg.updates.timeoutSeconds);
  return Number.isFinite(seconds) && seconds > 0 ? seconds * 1000 : git.DEFAULT_TIMEOUT_MS;
}

function sourceLine(bundle, ms) {
  if (bundle.authored) return 'source: bundled with this plugin';
  const st = git.status(bundle.root, ms);
  if (st.kind === 'missing') {
    return `source: not cloned — mcp_mastery.update {action:"pull", bundle:"${bundle.id}"}`;
  }
  if (st.kind === 'plain') return 'source: present, but not a git clone';

  const parts = [];
  if (st.behind === null) parts.push('no upstream to compare against');
  else if (st.behind === 0) parts.push('up to date with upstream');
  else parts.push(`${plural(st.behind, 'commit', 'commits')} behind upstream`);

  if (st.branch && st.branch !== 'HEAD') parts.push(`on ${st.branch}`);
  if (st.head) parts.push(`@ ${st.head}`);
  if (st.date) parts.push(`(${st.date})`);
  if (st.dirty) parts.push(`${plural(st.dirty, 'edited file', 'edited files')} locally`);

  return `source: ${parts.join(', ')}`;
}

function lastChecked() {
  const cache = config.readJson(config.updatesPath());
  return cache && typeof cache.checkedAt === 'string' ? cache.checkedAt : null;
}

/* -------------------------------------------------------------------- tools */

function toolBundles(args) {
  const cfg = config.loadConfig();
  const session = state.load();
  const verbose = args && args.verbose === true;
  const ms = timeoutMs(cfg);

  if (cfg.bundles.length === 0) {
    return 'No bundles configured. Add one to ' + config.configPath() + ' and call mcp_mastery.bundles again.';
  }

  const blocks = cfg.bundles.map((bundle) => {
    const on = bundle.enabled !== false && state.isOn(session, bundle.id);
    const skills = bundles.discover(bundle.root);
    const loads = state.loadsFor(session, bundle.id);
    const lines = [];

    const flag = on ? 'ON ' : 'OFF';
    const count = !fs.existsSync(bundle.root)
      ? 'not cloned'
      : skills.length === 0
        ? 'no skills found'
        : plural(skills.length, 'skill', 'skills');
    lines.push(`${flag} ${bundle.label} (${bundle.id}) — ${count}`);
    if (bundle.blurb) lines.push(`    ${bundle.blurb}`);
    lines.push(`    ${sourceLine(bundle, ms)}`);

    if (bundle.enabled === false) {
      lines.push('    disabled in config — mcp_mastery.toggle cannot override that');
    }

    if (skills.length > 0) {
      const shown = verbose ? skills : skills.slice(0, 12);
      for (const skill of shown) {
        const used = loads[skill.name] || 0;
        const suffix = used > 0 ? ` · loaded ${used}×` : '';
        const desc = verbose && skill.description ? ` — ${skill.description}` : '';
        lines.push(`    · ${skill.name}${suffix}${desc}`);
      }
      if (!verbose && skills.length > shown.length) {
        lines.push(`    · … ${skills.length - shown.length} more (mcp_mastery.bundles {verbose:true})`);
      }
    }

    return lines.join('\n');
  });

  const checked = lastChecked();
  const footer = [
    '',
    `Load a skill body with mcp_mastery.load {bundle, skill}. Search across bundles with mcp_mastery.find {query}.`,
    checked ? `Updates last checked ${checked}.` : 'Updates never checked — mcp_mastery.update {action:"check"}.',
  ].join('\n');

  return `${blocks.join('\n\n')}\n${footer}`;
}

function toolToggle(args) {
  const cfg = config.loadConfig();
  const bundle = findBundle(cfg, args && args.bundle);
  if (!bundle) {
    return {
      isError: true,
      text: `No bundle matches ${JSON.stringify(args && args.bundle)}. Known: ${cfg.bundles.map((b) => b.id).join(', ')}.`,
    };
  }
  if (typeof args.on !== 'boolean') {
    return { isError: true, text: 'mcp_mastery.toggle needs `on` as true or false.' };
  }
  if (bundle.enabled === false && args.on) {
    return {
      isError: true,
      text: `${bundle.id} is disabled in ${config.configPath()}. Set "enabled": true there first — a session switch cannot override the config.`,
    };
  }

  state.toggle(bundle.id, args.on);
  const scope = args.on
    ? `${bundle.label} is on for this session. mcp_mastery.load will serve its skills.`
    : `${bundle.label} is off for this session. mcp_mastery.load will refuse it until you turn it back on.`;
  return { isError: false, text: `${scope}\nThis switch is per-conversation; the config file is untouched.` };
}

function toolLoad(args) {
  const cfg = config.loadConfig();
  const bundle = findBundle(cfg, args && args.bundle);
  if (!bundle) {
    return {
      isError: true,
      text: `No bundle matches ${JSON.stringify(args && args.bundle)}. Known: ${cfg.bundles.map((b) => b.id).join(', ')}.`,
    };
  }
  if (bundle.enabled === false) {
    return { isError: true, text: `${bundle.id} is disabled in ${config.configPath()}.` };
  }
  const session = state.load();
  if (!state.isOn(session, bundle.id)) {
    return {
      isError: true,
      text: `${bundle.id} is switched off in this session. mcp_mastery.toggle {bundle:"${bundle.id}", on:true} to turn it back on.`,
    };
  }

  const skills = bundles.discover(bundle.root);
  if (skills.length === 0) {
    return {
      isError: true,
      text: `${bundle.id} has no skills at ${bundle.root}. ${
        bundle.authored ? 'The package looks incomplete.' : 'Clone it with mcp_mastery.update {action:"pull", bundle:"' + bundle.id + '"}.'}`,
    };
  }

  const wanted = String((args && args.skill) || '').trim().toLowerCase();
  if (!wanted) {
    return { isError: true, text: `mcp_mastery.load needs a skill name. Available in ${bundle.id}: ${skills.map((s) => s.name).join(', ')}.` };
  }

  const skill =
    skills.find((s) => s.name.toLowerCase() === wanted) ||
    skills.find((s) => s.name.toLowerCase().replace(/[\s_]+/g, '-') === wanted.replace(/[\s_]+/g, '-')) ||
    skills.find((s) => s.name.toLowerCase().includes(wanted));

  if (!skill) {
    const near = skills.filter((s) => s.name.toLowerCase().split(/[-_\s]+/).some((word) => word.startsWith(wanted)));
    const hint = near.length > 0 ? `Closest: ${near.slice(0, 8).map((s) => s.name).join(', ')}.` : `Available: ${skills.map((s) => s.name).join(', ')}.`;
    return { isError: true, text: `No skill ${JSON.stringify(args.skill)} in ${bundle.id}. ${hint}` };
  }

  state.countLoad(bundle.id, skill.name);

  const body = skill.body.trim();
  const note = skill.truncated ? `\n\n[truncated at ${Math.round(skill.body.length / 1024)} KiB — read ${skill.file} for the rest]` : '';
  return { isError: false, text: `${body}${note}` };
}

function toolFind(args) {
  const cfg = config.loadConfig();
  const session = state.load();
  const query = String((args && args.query) || '').trim().toLowerCase();
  if (!query) return { isError: true, text: 'mcp_mastery.find needs a query.' };

  const only = args && args.bundle ? findBundle(cfg, args.bundle) : null;
  if (args && args.bundle && !only) {
    return { isError: true, text: `No bundle matches ${JSON.stringify(args.bundle)}.` };
  }

  const hits = [];
  for (const bundle of cfg.bundles) {
    if (only && bundle.id !== only.id) continue;
    const on = bundle.enabled !== false && state.isOn(session, bundle.id);
    for (const skill of bundles.discover(bundle.root)) {
      const name = skill.name.toLowerCase();
      const desc = (skill.description || '').toLowerCase();
      let score = 0;
      if (name === query) score = 100;
      else if (name.startsWith(query)) score = 80;
      else if (name.includes(query)) score = 60;
      else if (desc.includes(query)) score = 30;
      else if (name.split(/[-_\s]+/).some((word) => word.startsWith(query))) score = 40;
      if (score === 0) continue;
      hits.push({ score, bundle: bundle.id, on, name: skill.name, description: skill.description });
    }
  }

  if (hits.length === 0) {
    return { isError: false, text: `Nothing matches ${JSON.stringify(args.query)}. mcp_mastery.bundles lists everything.` };
  }

  hits.sort((a, b) => b.score - a.score || a.name.localeCompare(b.name));
  const top = hits.slice(0, 12);
  const lines = top.map((hit) => {
    const flag = hit.on ? '' : ' [bundle off]';
    const desc = hit.description ? ` — ${hit.description}` : '';
    return `${hit.bundle}/${hit.name}${flag}${desc}`;
  });
  const more = hits.length > top.length ? `\n… ${hits.length - top.length} more.` : '';
  return { isError: false, text: `${lines.join('\n')}${more}\n\nLoad one with mcp_mastery.load {bundle, skill}.` };
}

function toolUpdate(args) {
  const cfg = config.loadConfig();
  const action = String((args && args.action) || 'check').toLowerCase();
  const ms = timeoutMs(cfg);

  if (action !== 'check' && action !== 'pull') {
    return { isError: true, text: 'mcp_mastery.update takes action "check" or "pull".' };
  }

  const targets = args && args.bundle ? [findBundle(cfg, args.bundle)] : cfg.bundles;
  if (targets.some((t) => !t)) {
    return { isError: true, text: `No bundle matches ${JSON.stringify(args.bundle)}.` };
  }

  const lines = [];
  const cache = { checkedAt: new Date().toISOString(), bundles: {} };
  let failed = false;

  for (const bundle of targets) {
    if (bundle.authored) {
      lines.push(`${bundle.label}: bundled with this plugin, nothing to update.`);
      continue;
    }
    if (!bundle.repo) {
      lines.push(`${bundle.label}: no repo configured, skipped.`);
      continue;
    }

    if (action === 'pull') {
      if (!git.isRepo(bundle.root)) {
        const cloned = git.clone(bundle.repo, bundle.root, ms);
        if (!cloned.ok) {
          failed = true;
          lines.push(`${bundle.label}: clone failed — ${cloned.error}`);
          continue;
        }
        lines.push(`${bundle.label}: cloned into ${bundle.root}.`);
      } else {
        const pulled = git.pull(bundle.root, ms);
        if (!pulled.ok) {
          failed = true;
          lines.push(`${bundle.label}: pull failed — ${pulled.error}`);
          continue;
        }
        lines.push(`${bundle.label}: fast-forwarded.`);
      }
    } else {
      if (!git.isRepo(bundle.root)) {
        lines.push(`${bundle.label}: not cloned yet.`);
        continue;
      }
      const fetched = git.fetch(bundle.root, ms);
      if (!fetched.ok) {
        failed = true;
        lines.push(`${bundle.label}: fetch failed — ${fetched.error}`);
        continue;
      }
    }

    const st = git.status(bundle.root, ms);
    cache.bundles[bundle.id] = { behind: st.behind, head: st.head, date: st.date, dirty: st.dirty };
    if (st.behind === null) lines.push(`${bundle.label}: no upstream to compare against.`);
    else if (st.behind === 0) lines.push(`${bundle.label}: up to date @ ${st.head ?? '?'}.`);
    else lines.push(`${bundle.label}: ${plural(st.behind, 'commit', 'commits')} behind @ ${st.head ?? '?'}.`);
  }

  config.writeJson(config.updatesPath(), cache);

  const tail =
    action === 'check' && Object.values(cache.bundles).some((b) => b.behind > 0)
      ? '\nRun mcp_mastery.update {action:"pull"} to fast-forward.'
      : '';
  return { isError: failed, text: `${lines.join('\n')}${tail}` };
}

/* -------------------------------------------------------------- definitions */

const TOOLS = [
  {
    name: 'bundles',
    description:
      'List the mastery bundles: whether each is on, how many skills it holds, where its source stands, and which skills have actually been loaded this conversation. Start here when you do not know what is available.',
    annotations: { readOnlyHint: true },
    inputSchema: {
      type: 'object',
      properties: {
        verbose: { type: 'boolean', description: 'Include each skill description and list every skill instead of the first twelve.' },
      },
      additionalProperties: false,
    },
  },
  {
    name: 'find',
    description:
      'Search skill names and descriptions across bundles. Use this when you know the kind of work you need but not which skill covers it.',
    annotations: { readOnlyHint: true },
    inputSchema: {
      type: 'object',
      properties: {
        query: { type: 'string', description: 'Words to match against skill names and descriptions.' },
        bundle: { type: 'string', description: 'Restrict the search to one bundle id.' },
      },
      required: ['query'],
      additionalProperties: false,
    },
  },
  {
    name: 'load',
    description:
      'Return the full body of one skill so it is in context for the rest of the turn. This is the substitute for a skill being registered with the client: the text arrives as a tool result instead of as a catalog entry.',
    annotations: { readOnlyHint: true },
    inputSchema: {
      type: 'object',
      properties: {
        bundle: { type: 'string', description: 'Bundle id, label, or an unambiguous prefix.' },
        skill: { type: 'string', description: 'Skill name as listed by bundles.' },
      },
      required: ['bundle', 'skill'],
      additionalProperties: false,
    },
  },
  {
    name: 'toggle',
    description:
      'Switch a bundle on or off for this conversation only. Off means load refuses it; the config file is untouched and the next conversation starts from the config again.',
    annotations: { readOnlyHint: false, destructiveHint: false },
    inputSchema: {
      type: 'object',
      properties: {
        bundle: { type: 'string', description: 'Bundle id, label, or an unambiguous prefix.' },
        on: { type: 'boolean', description: 'true to enable, false to disable.' },
      },
      required: ['bundle', 'on'],
      additionalProperties: false,
    },
  },
  {
    name: 'update',
    description:
      'Check the bundle clones for upstream commits, or fast-forward them. Pull clones a bundle that is missing and never merges or rebases: a diverged branch is reported, not forced.',
    annotations: { readOnlyHint: false, destructiveHint: false },
    inputSchema: {
      type: 'object',
      properties: {
        action: { type: 'string', enum: ['check', 'pull'], description: 'check fetches and reports; pull fast-forwards.' },
        bundle: { type: 'string', description: 'Limit to one bundle. Omit for all.' },
      },
      required: ['action'],
      additionalProperties: false,
    },
  },
];

const HANDLERS = {
  bundles: (args) => ({ isError: false, text: toolBundles(args) }),
  find: toolFind,
  load: toolLoad,
  toggle: toolToggle,
  update: toolUpdate,
};

/* ------------------------------------------------------------------ protocol */

function send(message) {
  process.stdout.write(`${JSON.stringify(message)}\n`);
}

function reply(id, result) {
  send({ jsonrpc: '2.0', id, result });
}

function fail(id, code, message) {
  send({ jsonrpc: '2.0', id: id ?? null, error: { code, message } });
}

function handleCall(id, params) {
  const name = params && params.name;
  const handler = HANDLERS[name];
  if (!handler) {
    fail(id, -32602, `Unknown tool ${JSON.stringify(name)}.`);
    return;
  }

  let outcome;
  try {
    outcome = handler(params.arguments || {});
  } catch (error) {
    outcome = { isError: true, text: `${name} failed: ${error && error.message ? error.message : String(error)}` };
  }

  reply(id, {
    content: [{ type: 'text', text: outcome.text }],
    isError: outcome.isError === true,
  });
}

function handle(message) {
  const { id, method, params } = message;
  const isNotification = id === undefined || id === null;

  switch (method) {
    case 'initialize': {
      const requested = params && params.protocolVersion;
      const protocolVersion = SUPPORTED_PROTOCOLS.includes(requested) ? requested : FALLBACK_PROTOCOL;
      reply(id, {
        protocolVersion,
        capabilities: { tools: {} },
        serverInfo: { name: 'vibe-mastery', version: '1.0.0' },
      });
      return;
    }
    case 'notifications/initialized':
    case 'notifications/cancelled':
      return;
    case 'ping':
      if (!isNotification) reply(id, {});
      return;
    case 'tools/list':
      reply(id, { tools: TOOLS });
      return;
    case 'tools/call':
      handleCall(id, params);
      return;
    default:
      if (!isNotification) fail(id, -32601, `Method not found: ${method}`);
  }
}

function start() {
  const rl = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });

  rl.on('line', (line) => {
    const trimmed = line.trim();
    if (!trimmed) return;

    let message;
    try {
      message = JSON.parse(trimmed);
    } catch {
      fail(null, -32700, 'Parse error.');
      return;
    }

    if (!message || typeof message !== 'object' || typeof message.method !== 'string') {
      fail(message && message.id, -32600, 'Invalid request.');
      return;
    }

    try {
      handle(message);
    } catch (error) {
      fail(message.id, -32603, error && error.message ? error.message : 'Internal error.');
    }
  });

  rl.on('close', () => process.exit(0));
}

process.on('uncaughtException', (error) => {
  process.stderr.write(`vibe-mastery: ${error && error.stack ? error.stack : String(error)}\n`);
  process.exit(1);
});

start();
