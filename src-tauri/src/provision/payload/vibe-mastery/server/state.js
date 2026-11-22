'use strict';

/**
 * Per-session state.
 *
 * Vibe gives every session its own `PLUGIN_DATA` directory, so this file is
 * already scoped to one conversation. That is why nothing here is keyed by
 * session id: the isolation is the directory, not a field.
 *
 * What lives here is deliberately small. Durable choices (which bundles exist,
 * where they come from) belong in the user-level config; this file only holds
 * what the user did *in this conversation* — which bundles they switched off,
 * and how often each skill body was actually pulled into context.
 */

const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const VERSION = 1;

function dataDir() {
  const dir = process.env.PLUGIN_DATA;
  if (dir && dir.trim()) return dir;
  return path.join(os.tmpdir(), 'vibe-mastery');
}

function statePath() {
  return path.join(dataDir(), 'state.json');
}

function empty() {
  return { version: VERSION, bundles: {} };
}

function load() {
  let raw;
  try {
    raw = fs.readFileSync(statePath(), 'utf8');
  } catch {
    return empty();
  }

  let parsed;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return empty();
  }

  if (!parsed || typeof parsed !== 'object' || parsed.version !== VERSION) return empty();
  if (!parsed.bundles || typeof parsed.bundles !== 'object') return empty();

  const state = empty();
  for (const [id, entry] of Object.entries(parsed.bundles)) {
    if (!entry || typeof entry !== 'object') continue;
    const loads = {};
    if (entry.loads && typeof entry.loads === 'object') {
      for (const [skill, count] of Object.entries(entry.loads)) {
        const n = Number(count);
        if (Number.isFinite(n) && n > 0) loads[skill] = Math.floor(n);
      }
    }
    state.bundles[id] = { on: entry.on !== false, loads };
  }
  return state;
}

function save(state) {
  const dir = dataDir();
  fs.mkdirSync(dir, { recursive: true });
  const target = statePath();
  const temp = `${target}.${process.pid}.tmp`;
  fs.writeFileSync(temp, `${JSON.stringify(state, null, 2)}\n`, 'utf8');
  fs.renameSync(temp, target);
}

/** Session-scoped switch. Absent means on; only an explicit `false` turns a bundle off. */
function toggle(id, on) {
  const state = load();
  const entry = state.bundles[id] || { on: true, loads: {} };
  entry.on = Boolean(on);
  state.bundles[id] = entry;
  save(state);
  return entry.on;
}

function isOn(state, id) {
  const entry = state.bundles[id];
  return !entry || entry.on !== false;
}

/** Record that a skill body was handed to the model, so `status` can show real usage. */
function countLoad(id, skill) {
  const state = load();
  const entry = state.bundles[id] || { on: true, loads: {} };
  entry.loads[skill] = (entry.loads[skill] || 0) + 1;
  state.bundles[id] = entry;
  save(state);
  return entry.loads[skill];
}

function loadsFor(state, id) {
  const entry = state.bundles[id];
  return entry && entry.loads ? entry.loads : {};
}

module.exports = { load, save, toggle, isOn, countLoad, loadsFor, statePath, dataDir, VERSION };
