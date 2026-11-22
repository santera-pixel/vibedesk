'use strict';

/**
 * Where the bundles are, and what the defaults are.
 *
 * This file is deliberately *not* in `PLUGIN_DATA`. Vibe hands each session its
 * own `PLUGIN_DATA`, which is exactly right for the switches and the counters
 * and exactly wrong for "which clones do I read": that answer is the same for
 * every session and belongs to the user, not to a conversation.
 */

const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const PLUGIN_ROOT = process.env.PLUGIN_ROOT || path.resolve(__dirname, '..');

/** The user's mastery directory: config, sources, update cache. */
function masteryHome() {
  if (process.env.VIBE_MASTERY_HOME) return path.resolve(process.env.VIBE_MASTERY_HOME);
  const vibeHome = process.env.VIBE_HOME || path.join(os.homedir(), '.vibe');
  return path.join(vibeHome, 'mastery');
}

/** Where the default clones live. `VIBE_MASTERY_SOURCES` moves all of them. */
function sourcesDir() {
  if (process.env.VIBE_MASTERY_SOURCES) return path.resolve(process.env.VIBE_MASTERY_SOURCES);
  return path.join(masteryHome(), 'sources');
}

function configPath() {
  return path.join(masteryHome(), 'config.json');
}

function updatesPath() {
  return path.join(masteryHome(), 'updates.json');
}

/**
 * The four bundles, in the order the chips were in.
 *
 * `authored` means the skills ship inside this package, so there is no upstream
 * to be behind and no clone to point at. Everything else is somebody else's
 * repository, read where it lies and never rewritten.
 */
const DEFAULT_BUNDLES = [
  {
    id: '3dviz',
    label: '3Dviz',
    blurb: 'Expressive 3D scenes, explainers and interactive models',
    repo: 'https://github.com/viettranx/3dviz-pro-max',
    dir: '3dviz-pro-max',
  },
  {
    id: 'antislop',
    label: 'Anti-Slop',
    blurb: 'The core filter plus its code, copy, layout, UI and human-writing variants',
    repo: 'https://github.com/miqdadbadjuber/Anti-Slop',
    dir: 'Anti-Slop',
  },
  {
    id: 'uiux',
    label: 'UI/UX',
    blurb: 'Design intelligence: styles, palettes, typography, UX rules, charts',
    repo: 'https://github.com/nextlevelbuilder/ui-ux-pro-max-skill',
    dir: 'ui-ux-pro-max-skill',
  },
  {
    id: 'assay',
    label: 'Assay',
    blurb: 'A procedure for work where being wrong costs something',
    authored: true,
  },
];

const DEFAULT_UPDATES = {
  autoCheck: true,
  autoPull: false,
  intervalHours: 6,
  timeoutSeconds: 60,
};

function defaultBundle(entry) {
  if (entry.authored) {
    return {
      id: entry.id,
      label: entry.label,
      blurb: entry.blurb,
      authored: true,
      root: path.join(PLUGIN_ROOT, 'skills', entry.id),
      enabled: true,
    };
  }
  return {
    id: entry.id,
    label: entry.label,
    blurb: entry.blurb,
    repo: entry.repo,
    root: path.join(sourcesDir(), entry.dir),
    enabled: true,
  };
}

function readJson(file) {
  try {
    return JSON.parse(fs.readFileSync(file, 'utf8'));
  } catch {
    return null;
  }
}

function writeJson(file, value) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, `${JSON.stringify(value, null, 2)}\n`, 'utf8');
}

/**
 * Read the config, filling in anything missing.
 *
 * A bundle the user deleted stays deleted: the defaults are only appended for
 * ids the file has never seen. That is the same rule the dsh plugin used, and
 * it is the difference between a default and an override.
 */
function loadConfig() {
  const file = configPath();
  const stored = readJson(file);
  const bundles = [];
  const seen = new Set();

  if (stored && Array.isArray(stored.bundles)) {
    for (const entry of stored.bundles) {
      if (!entry || typeof entry.id !== 'string' || seen.has(entry.id)) continue;
      seen.add(entry.id);
      const fallback = DEFAULT_BUNDLES.find((b) => b.id === entry.id);
      bundles.push({
        id: entry.id,
        label: typeof entry.label === 'string' ? entry.label : (fallback?.label ?? entry.id),
        blurb: typeof entry.blurb === 'string' ? entry.blurb : (fallback?.blurb ?? ''),
        authored: entry.authored === true || fallback?.authored === true,
        repo: typeof entry.repo === 'string' ? entry.repo : fallback?.repo,
        root:
          typeof entry.root === 'string' && entry.root.length > 0
            ? path.resolve(entry.root)
            : defaultBundle(fallback ?? { id: entry.id, label: entry.id, blurb: '' }).root,
        enabled: entry.enabled !== false,
      });
    }
  }

  for (const entry of DEFAULT_BUNDLES) {
    if (seen.has(entry.id)) continue;
    bundles.push(defaultBundle(entry));
  }

  const updates = { ...DEFAULT_UPDATES, ...(stored && stored.updates ? stored.updates : {}) };

  const config = { version: 2, updates, bundles };

  // Write it back when the file is absent or older, so the user has something
  // to edit rather than a shape they have to guess.
  if (!stored || stored.version !== 2) writeJson(file, config);

  return config;
}

module.exports = {
  PLUGIN_ROOT,
  DEFAULT_BUNDLES,
  masteryHome,
  sourcesDir,
  configPath,
  updatesPath,
  loadConfig,
  readJson,
  writeJson,
};
