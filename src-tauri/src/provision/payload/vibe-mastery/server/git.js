'use strict';

/**
 * The only file that shells out to git.
 *
 * Two rules, both learned from the dsh plugin this ports:
 *
 *  * `GIT_TERMINAL_PROMPT=0`, so a missing credential fails instead of blocking
 *    a turn on a prompt nobody can see.
 *  * Fast-forward only. These are somebody else's clones; a merge commit or a
 *    rebase would rewrite their history, and a diverged branch is a thing to
 *    report, not to force.
 */

const { execFileSync } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');

const DEFAULT_TIMEOUT_MS = 60_000;

function run(root, args, timeoutMs) {
  return execFileSync('git', args, {
    cwd: root,
    timeout: timeoutMs ?? DEFAULT_TIMEOUT_MS,
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
    env: { ...process.env, GIT_TERMINAL_PROMPT: '0' },
  }).trim();
}

function isRepo(root) {
  try {
    return fs.statSync(path.join(root, '.git')).isDirectory();
  } catch {
    return false;
  }
}

/**
 * What this bundle's source looks like right now.
 *
 * `behind` is null when there is no upstream to compare against, which is a
 * different answer from zero and has to stay different: "up to date" and "I
 * cannot tell" are not the same claim.
 */
function status(root, timeoutMs) {
  if (!fs.existsSync(root)) {
    return { kind: 'missing', behind: null, error: 'not cloned' };
  }
  if (!isRepo(root)) {
    return { kind: 'plain', behind: null };
  }

  const out = { kind: 'git', behind: null, branch: null, head: null, date: null, dirty: 0 };

  try {
    out.branch = run(root, ['rev-parse', '--abbrev-ref', 'HEAD'], timeoutMs);
  } catch {
    /* a detached or unborn HEAD is not worth failing over */
  }
  try {
    out.head = run(root, ['rev-parse', '--short', 'HEAD'], timeoutMs);
  } catch {
    /* empty repository */
  }
  try {
    out.date = run(root, ['log', '-1', '--format=%cs'], timeoutMs);
  } catch {
    /* no commits */
  }
  try {
    const porcelain = run(root, ['status', '--porcelain'], timeoutMs);
    out.dirty = porcelain ? porcelain.split('\n').length : 0;
  } catch {
    /* ignore */
  }
  try {
    out.behind = Number.parseInt(run(root, ['rev-list', '--count', 'HEAD..@{u}'], timeoutMs), 10);
    if (!Number.isFinite(out.behind)) out.behind = null;
  } catch {
    out.behind = null;
  }

  return out;
}

/** Shallow-ish clone into a directory that does not exist yet. */
function clone(repo, root, timeoutMs) {
  if (fs.existsSync(root)) {
    return { ok: false, error: `${root} already exists` };
  }
  try {
    fs.mkdirSync(path.dirname(root), { recursive: true });
    run(path.dirname(root), ['clone', '--quiet', repo, root], timeoutMs);
    return { ok: true };
  } catch (error) {
    return { ok: false, error: firstLine(error) };
  }
}

/** Network only, no working-tree writes. */
function fetch(root, timeoutMs) {
  if (!isRepo(root)) return { ok: false, error: 'not a git clone' };
  try {
    run(root, ['fetch', '--quiet', '--prune'], timeoutMs);
    return { ok: true };
  } catch (error) {
    return { ok: false, error: firstLine(error) };
  }
}

/** Fast-forward only. A diverged branch is reported, never forced. */
function pull(root, timeoutMs) {
  if (!isRepo(root)) return { ok: false, error: 'not a git clone' };
  try {
    run(root, ['pull', '--ff-only', '--no-rebase', '--quiet'], timeoutMs);
    return { ok: true };
  } catch (error) {
    return { ok: false, error: firstLine(error) };
  }
}

/**
 * The one line worth showing.
 *
 * Git puts the actual reason on a `fatal:` or `error:` line and buries it under
 * `hint:` advice, so taking the first line of stderr reports the advice and
 * hides the cause.
 */
function firstLine(error) {
  const text = String((error && (error.stderr || error.message)) || 'git failed');
  const lines = text.split('\n').map((line) => line.trim()).filter(Boolean);
  const fatal = lines.find((line) => /^(fatal|error):/i.test(line));
  const chosen = fatal || lines.find((line) => !/^hint:/i.test(line)) || lines[0] || 'git failed';
  return chosen.replace(/^(fatal|error):\s*/i, '');
}

module.exports = { status, clone, fetch, pull, isRepo, DEFAULT_TIMEOUT_MS };
