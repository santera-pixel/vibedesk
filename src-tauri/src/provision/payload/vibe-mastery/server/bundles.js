'use strict';

/**
 * Finding skills inside a bundle root.
 *
 * A bundle is a directory, usually somebody else's clone, and the only contract
 * is "somewhere under here there are SKILL.md files". So this walks, bounded,
 * and parses just enough YAML to get a name and a one-line description. The
 * body is kept because loading it is the whole point.
 */

const fs = require('node:fs');
const path = require('node:path');

const MAX_DEPTH = 6;
const MAX_FILES = 8000;
const MAX_SKILL_BYTES = 512 * 1024;
const MAX_BODY_BYTES = 256 * 1024;

/** Directories that never contain a skill worth loading. */
const SKIP = new Set([
  '.git',
  'node_modules',
  'dist',
  'build',
  'out',
  '.next',
  '.nuxt',
  'coverage',
  'vendor',
  '__pycache__',
  '.venv',
  'venv',
  '.cache',
]);

/**
 * Parse the frontmatter of a SKILL.md.
 *
 * Handles the three shapes that actually appear: a bare scalar, a quoted
 * scalar, and a folded or literal block. Anything else is left to the caller's
 * fallback rather than guessed at.
 */
function parseFrontmatter(text) {
  const match = /^---\r?\n([\s\S]*?)\r?\n---\r?\n?/.exec(text);
  if (!match) return { fields: {}, body: text };

  const fields = {};
  const lines = match[1].split(/\r?\n/);
  let index = 0;

  while (index < lines.length) {
    const line = lines[index];
    index += 1;
    const pair = /^([A-Za-z0-9_-]+)\s*:\s*(.*)$/.exec(line);
    if (!pair) continue;

    const key = pair[1];
    let value = pair[2].trim();

    if (value === '>' || value === '|' || value === '>-' || value === '|-') {
      const block = [];
      while (index < lines.length && /^\s+\S/.test(lines[index])) {
        block.push(lines[index].trim());
        index += 1;
      }
      value = block.join(value.startsWith('>') ? ' ' : '\n');
    } else if (
      (value.startsWith('"') && value.endsWith('"') && value.length > 1) ||
      (value.startsWith("'") && value.endsWith("'") && value.length > 1)
    ) {
      value = value.slice(1, -1);
    }

    fields[key] = value;
  }

  return { fields, body: text.slice(match[0].length) };
}

/**
 * Every skill under `root`, sorted by name.
 *
 * A missing root is not an error: it means the clone has not been made yet, and
 * the caller reports that as "not cloned" rather than as a failure.
 */
function discover(root) {
  const skills = [];
  let visited = 0;

  const walk = (dir, depth) => {
    if (depth > MAX_DEPTH || visited >= MAX_FILES) return;

    let entries;
    try {
      entries = fs.readdirSync(dir, { withFileTypes: true });
    } catch {
      return;
    }

    for (const entry of entries) {
      if (visited >= MAX_FILES) return;
      visited += 1;

      const full = path.join(dir, entry.name);

      if (entry.isDirectory()) {
        if (SKIP.has(entry.name) || entry.name.startsWith('.')) continue;
        walk(full, depth + 1);
        continue;
      }

      if (!entry.isFile() || entry.name !== 'SKILL.md') continue;

      let stat;
      try {
        stat = fs.statSync(full);
      } catch {
        continue;
      }
      if (stat.size > MAX_SKILL_BYTES) continue;

      let text;
      try {
        text = fs.readFileSync(full, 'utf8');
      } catch {
        continue;
      }

      const { fields, body } = parseFrontmatter(text);
      const name = (fields.name || path.basename(dir)).trim();
      if (!name) continue;

      skills.push({
        name,
        description: (fields.description || '').trim(),
        body: body.length > MAX_BODY_BYTES ? body.slice(0, MAX_BODY_BYTES) : body,
        file: full,
        truncated: body.length > MAX_BODY_BYTES,
      });
    }
  };

  if (!fs.existsSync(root)) return skills;
  walk(root, 0);

  skills.sort((a, b) => a.name.localeCompare(b.name));
  return skills;
}

module.exports = { discover, parseFrontmatter, MAX_DEPTH, MAX_FILES };
