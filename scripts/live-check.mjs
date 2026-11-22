#!/usr/bin/env node
/**
 * Live verification over the Chrome DevTools Protocol.
 *
 * The app exposes a CDP endpoint when it is started with
 * `VIBEDESK_REMOTE_DEBUGGING=<port>` and built in debug. This script attaches to
 * it over loopback and does the things a unit test cannot: read the console,
 * evaluate expressions against the real DOM, drive the UI, and take a
 * screenshot.
 *
 * No dependencies. Node 24 has `fetch`, `WebSocket` and `node:http` built in,
 * and the plan forbids adding packages for this.
 *
 *   node scripts/live-check.mjs smoke
 *   node scripts/live-check.mjs panels
 *   node scripts/live-check.mjs data
 *   node scripts/live-check.mjs plugins
 *   node scripts/live-check.mjs motion
 *   node scripts/live-check.mjs chats
 *   node scripts/live-check.mjs all
 *   node scripts/live-check.mjs eval "document.title"
 *   node scripts/live-check.mjs shot out.png
 *
 * Exit code is 0 when every check passed, 1 when one failed, 2 when the app
 * could not be reached.
 *
 * The suites that install a panel or a plugin write into the app's debug data
 * directory and, for `plugins`, into `~/.vibe/plugins`. Both are restored in a
 * `finally`, so a failing check still leaves the machine as it was. `--keep`
 * skips the cleanup when you want to look at what a suite built.
 */

import { mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { homedir } from 'node:os';
import { dirname, join } from 'node:path';
import { Cdp } from './cdp-client.mjs';

const DEFAULT_PORT = 9222;
const DEFAULT_DATA_DIR = 'src-tauri/target/debug/data';
const PLUGIN_NAME = 'vibedesk-live-check';
const PLUGIN_DISPLAY = 'Live Check';



/* ---------------------------------------------------------------- *
 * Console capture
 * ---------------------------------------------------------------- */

/**
 * Collect console errors and uncaught exceptions.
 *
 * `Runtime.consoleAPICalled` carries `console.error` and friends;
 * `Runtime.exceptionThrown` carries what escaped a handler. Both are what the
 * plan's "zero console errors" evidence means, so both are collected.
 */
async function watchConsole(cdp) {
  const errors = [];
  const warnings = [];

  await cdp.send('Runtime.enable');
  await cdp.send('Log.enable');

  cdp.on('Runtime.consoleAPICalled', (event) => {
    const text = (event.args ?? [])
      .map((arg) => arg.value ?? arg.description ?? arg.type)
      .join(' ');
    if (event.type === 'error') errors.push(text);
    else if (event.type === 'warning') warnings.push(text);
  });

  cdp.on('Runtime.exceptionThrown', (event) => {
    const details = event.exceptionDetails;
    errors.push(details?.exception?.description ?? details?.text ?? 'uncaught exception');
  });

  cdp.on('Log.entryAdded', (event) => {
    const entry = event.entry;
    if (entry.level === 'error') errors.push(entry.text);
    else if (entry.level === 'warning') warnings.push(entry.text);
  });

  return { errors, warnings };
}

/* ---------------------------------------------------------------- *
 * Page helpers
 * ---------------------------------------------------------------- */

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/**
 * Poll an expression until it is truthy.
 *
 * A fixed delay is a guess, and a wrong guess reports a slow boot as a broken
 * app. Polling for the actual condition costs time instead of a false failure.
 */
async function waitFor(cdp, expression, { timeoutMs = 15000, label = expression } = {}) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      if (await cdp.evaluate(expression)) return true;
    } catch {
      // The page may be mid-navigation; try again.
    }
    await sleep(200);
  }
  throw new Error(`timed out waiting for ${label}`);
}

/** Click the first button whose title matches, and fail loudly if there is none. */
async function clickByTitle(cdp, title) {
  const clicked = await cdp.evaluate(`(() => {
    const button = [...document.querySelectorAll('button')]
      .find((b) => b.getAttribute('title') === ${JSON.stringify(title)});
    if (!button) return false;
    button.click();
    return true;
  })()`);
  if (!clicked) throw new Error(`no button titled ${JSON.stringify(title)}`);
}

/** Click the first button whose title starts with the given text. */
async function clickByTitlePrefix(cdp, prefix) {
  const clicked = await cdp.evaluate(`(() => {
    const button = [...document.querySelectorAll('button')]
      .find((b) => (b.getAttribute('title') ?? '').startsWith(${JSON.stringify(prefix)}));
    if (!button) return false;
    button.click();
    return true;
  })()`);
  if (!clicked) throw new Error(`no button titled ${JSON.stringify(prefix)}…`);
}

/**
 * Click a plugin's enable/disable toggle, scoped to that plugin's card.
 *
 * The drawer lists every installed plugin, so an unscoped "first Enable plugin
 * button" click acts on whichever plugin happens to sort first. That is how
 * this suite broke the moment a second plugin was installed: it enabled the
 * probe plugin and then disabled the other one, so the probe's panel never
 * went away.
 *
 * The toggle lives in the Plugins drawer, which the rail button opens and which
 * opening a panel closes again. Clicking the rail button unconditionally would
 * close a drawer that is already open, so the card is looked for first and the
 * drawer is opened only when it is not there.
 */
async function clickPluginToggle(cdp, title, displayName) {
  const click = () =>
    cdp.evaluate(`(() => {
      const card = [...document.querySelectorAll('.vd-card')].find((node) =>
        (node.textContent ?? '').includes(${JSON.stringify(displayName)}));
      if (!card) return 'no-card';
      const button = [...card.querySelectorAll('button')]
        .find((b) => b.getAttribute('title') === ${JSON.stringify(title)});
      if (!button) return 'wrong-state';
      button.click();
      return 'clicked';
    })()`);

  const wrongState = () => new Error(`${displayName} has no ${JSON.stringify(title)} toggle`);

  let state = await click();
  if (state === 'clicked') return;
  if (state === 'wrong-state') throw wrongState();

  await openSection(cdp, 'Plugins');
  for (let attempt = 0; attempt < 20; attempt += 1) {
    state = await click();
    if (state === 'clicked') return;
    if (state === 'wrong-state') throw wrongState();
    await sleep(250);
  }
  throw new Error(`could not open the drawer to reach ${displayName}`);
}

/** The enable/disable title of one plugin's toggle, or null when it is not on screen. */
function pluginToggleExpr(displayName) {
  return `(() => {
    const card = [...document.querySelectorAll('.vd-card')].find((node) =>
      (node.textContent ?? '').includes(${JSON.stringify(displayName)}));
    if (!card) return null;
    const button = [...card.querySelectorAll('button')]
      .find((b) => ['Enable plugin', 'Disable plugin'].includes(b.getAttribute('title')));
    return button ? button.getAttribute('title') : null;
  })()`;
}

/** Click the first button whose visible text matches exactly. */
async function clickByText(cdp, text) {
  const clicked = await cdp.evaluate(`(() => {
    const button = [...document.querySelectorAll('button')]
      .find((b) => b.textContent.trim() === ${JSON.stringify(text)});
    if (!button) return false;
    button.click();
    return true;
  })()`);
  if (!clicked) throw new Error(`no button labelled ${JSON.stringify(text)}`);
}

/**
 * Open a panel, whether or not it is already open.
 *
 * The rail button toggles, so clicking it on an open panel closes it. A suite
 * that assumes it starts closed fails the moment a previous run left it open,
 * which is exactly the kind of flake that makes a harness untrustworthy.
 */
async function openPanel(cdp, title) {
  const already = await cdp.evaluate(`(() => {
    const aside = [...document.querySelectorAll('aside')]
      .find((a) => a.innerText.startsWith(${JSON.stringify(title)}));
    return Boolean(aside);
  })()`);
  if (already) return;
  await clickByTitle(cdp, title);
  await waitFor(
    cdp,
    `[...document.querySelectorAll('aside')].some((a) => a.innerText.startsWith(${JSON.stringify(title)}))`,
    { label: `the ${title} panel to open` },
  );
}

/**
 * Open one settings section.
 *
 * The rail has a single Settings entry now, so reaching a section is two
 * clicks: the directory, then the row. `openPanel` cannot do it, because it
 * looks for a button titled with the panel's own name and that button no longer
 * exists. Clicking Settings while a section is open lands on the directory
 * rather than closing it, so this is safe to call when the section is already
 * open.
 */
async function openSection(cdp, label) {
  // The rail button toggles, so clicking it while the directory is already
  // showing closes the panel and the wait below times out. Every caller has to
  // be able to reach a section from any state, so the click is conditional.
  const directoryOpen = await cdp.evaluate(
    `[...document.querySelectorAll('aside')].some((a) => a.querySelector('h2')?.textContent === 'Settings')`,
  );
  if (!directoryOpen) await clickByTitle(cdp, 'Settings');
  await waitFor(
    cdp,
    `[...document.querySelectorAll('aside')].some((a) => a.querySelector('h2')?.textContent === 'Settings')`,
    { label: 'the settings directory' },
  );
  const clicked = await cdp.evaluate(`(() => {
    const aside = [...document.querySelectorAll('aside')]
      .find((a) => a.querySelector('h2')?.textContent === 'Settings');
    const body = aside?.querySelector(':scope > div');
    const row = [...(body?.querySelectorAll('button') ?? [])]
      .find((b) => b.querySelector('span span')?.textContent === ${JSON.stringify(label)});
    if (!row) return false;
    row.click();
    return true;
  })()`);
  if (!clicked) throw new Error(`no settings section titled ${JSON.stringify(label)}`);
  await sleep(350);
}

/**
 * The header text of one panel.
 *
 * `document.querySelector('aside')` finds the workstation rail, which is also
 * an aside and comes first in the document. Scoping by the panel's own title is
 * the only way to read the badge that belongs to the panel under test.
 */
function panelHeader(cdp, title) {
  return cdp.evaluate(`(() => {
    const aside = [...document.querySelectorAll('aside')]
      .find((a) => a.innerText.startsWith(${JSON.stringify(title)}));
    return aside?.querySelector('header')?.innerText ?? '';
  })()`);
}

/** Every button title on the page, for assertions about the rail. */
function railTitles(cdp) {
  return cdp.evaluate(
    `[...document.querySelectorAll('button')].map((b) => b.getAttribute('title')).filter(Boolean)`,
  );
}

/* ---------------------------------------------------------------- *
 * Fixtures: panels and plugins
 * ---------------------------------------------------------------- */

/**
 * Install a `panels.json`, remembering what was there.
 *
 * The app watches the config directory, so writing the file is enough to make
 * the frontend re-read it — no restart, no IPC call.
 */
async function installPanels(dataDir, panels) {
  const path = join(dataDir, 'config', 'panels.json');
  let previous = null;
  try {
    previous = await readFile(path, 'utf8');
  } catch {
    previous = null;
  }
  await mkdir(dirname(path), { recursive: true });
  await writeFile(path, JSON.stringify({ panels }, null, 2));
  return async function restore() {
    if (previous === null) await rm(path, { force: true });
    else await writeFile(path, previous);
  };
}

/**
 * Write a `widgets/<name>.html` next to `panels.json`, remembering what was
 * there.
 *
 * A widget's html may live in a file the block points at with `src`. The file is
 * the source of truth: it is read into `html` on load and stripped back out on
 * save, so the two cannot drift. This helper is what lets the suite prove that
 * round trip against the real loader rather than against a mock.
 */
async function installWidgetFile(dataDir, name, html) {
  const path = join(dataDir, 'config', 'widgets', name);
  let previous = null;
  try {
    previous = await readFile(path, 'utf8');
  } catch {
    previous = null;
  }
  await mkdir(dirname(path), { recursive: true });
  await writeFile(path, html);
  return async function restore() {
    if (previous === null) await rm(path, { force: true });
    else await writeFile(path, previous);
  };
}

/** Install an `actions.json`, remembering what was there. */
async function installActions(dataDir, actions) {
  const path = join(dataDir, 'config', 'actions.json');
  let previous = null;
  try {
    previous = await readFile(path, 'utf8');
  } catch {
    previous = null;
  }
  await mkdir(dirname(path), { recursive: true });
  await writeFile(path, JSON.stringify({ actions }, null, 2));
  return async function restore() {
    if (previous === null) await rm(path, { force: true });
    else await writeFile(path, previous);
  };
}

/** Install a plugin that ships one panel, remembering whether the name was free. */
async function installPlugin(panelBody) {
  const root = join(
    process.env.HOME ?? process.env.USERPROFILE ?? '.',
    '.vibe',
    'plugins',
    PLUGIN_NAME,
  );
  await rm(root, { recursive: true, force: true });
  await mkdir(join(root, 'vibedesk'), { recursive: true });
  await writeFile(
    join(root, 'plugin.json'),
    JSON.stringify(
      {
        name: PLUGIN_NAME,
        version: '0.0.1',
        description: 'Installed by scripts/live-check.mjs and removed again.',
        extensions: { 'ai.mistral.vibe': { displayName: 'Live Check' } },
      },
      null,
      2,
    ),
  );
  await writeFile(join(root, 'vibedesk', 'panels.json'), JSON.stringify(panelBody, null, 2));
  return async function restore() {
    await rm(root, { recursive: true, force: true });
  };
}

/** A loopback server that counts its own requests, for the external source path. */
function startLoopback() {
  let hits = 0;
  const server = createServer((request, response) => {
    hits += 1;
    response.writeHead(200, { 'content-type': 'application/json' });
    response.end(JSON.stringify({ value: hits, at: Date.now() }));
  });
  return new Promise((resolve) => {
    server.listen(0, '127.0.0.1', () => {
      resolve({
        port: server.address().port,
        hits: () => hits,
        stop: () => new Promise((done) => server.close(done)),
      });
    });
  });
}

/**
 * The bundle `dist/index.html` loads, or null when there is no build.
 *
 * The frontend is embedded at compile time, so a binary can be running a bundle
 * that no longer exists on disk. Comparing the two is the difference between
 * verifying the code you just wrote and verifying the code you wrote last time.
 */
async function builtBundle() {
  try {
    const html = await readFile(new URL('../dist/index.html', import.meta.url), 'utf8');
    const match = html.match(/\/assets\/(index-[^"]+\.js)/);
    return match ? match[1] : null;
  } catch {
    return null;
  }
}

/* ---------------------------------------------------------------- *
 * Suites
 * ---------------------------------------------------------------- */

/**
 * Boot health. Nothing here is about customization; it is the floor.
 */
async function smoke(cdp) {
  const checks = [];
  const check = async (name, expression) => {
    try {
      checks.push({ name, ok: Boolean(await cdp.evaluate(expression)) });
    } catch (error) {
      checks.push({ name, ok: false, detail: error.message });
    }
  };

  // Checked first, because every other result is meaningless if the app is
  // running a bundle that is not the one on disk.
  const expected = await builtBundle();
  const running = await cdp.evaluate(
    `[...document.querySelectorAll('script')].map((s) => s.src).find((s) => s.includes('/assets/')) ?? null`,
  );
  if (expected !== null && running !== null) {
    const ok = running.endsWith(expected);
    checks.push({
      name: 'the running bundle is the one in dist',
      ok,
      detail: ok
        ? undefined
        : `running ${running.split('/').pop()}, dist has ${expected} — run \`npm run build\` then \`cargo build\``,
    });
  }

  await check('the app mounted', `(document.querySelector('#root')?.children.length ?? 0) > 0`);
  await check(
    'the duration tokens resolve to a time',
    // A custom property returns its token stream, not a resolved time, so the
    // only honest check is to use it in a real transition and read that back.
    `(() => {
      const probe = document.createElement('div');
      probe.style.transition = 'opacity var(--vd-fast) linear, transform var(--vd-slow) linear';
      document.body.appendChild(probe);
      const value = getComputedStyle(probe).transitionDuration;
      probe.remove();
      return value.split(',').every((part) => Number.parseFloat(part) > 0);
    })()`,
  );
  await check(
    'the curve tokens resolve to a curve',
    `(() => {
      const probe = document.createElement('div');
      probe.style.transition = 'opacity var(--vd-fast) var(--vd-ease)';
      document.body.appendChild(probe);
      const value = getComputedStyle(probe).transitionTimingFunction;
      probe.remove();
      return value.startsWith('cubic-bezier');
    })()`,
  );
  await check('the rail rendered', `document.querySelectorAll('[aria-pressed]').length > 0`);
  await check(
    'no element overflows the window horizontally',
    `document.documentElement.scrollWidth <= window.innerWidth + 1`,
  );
  // A debugging port lets any local process drive this window. It is opt-in and
  // debug-only, but a capability the user cannot see is one they cannot reason
  // about, so the app has to say when it is on. Read from the Help drawer rather
  // than the boot notice: the notice is a chat item, and switching chats clears
  // it, which made this check pass or fail on how long the app had been up.
  const port = await cdp.evaluate(`window.__TAURI_INTERNALS__.invoke('debug_endpoint')`);
  if (port === null) {
    checks.push({ name: 'a debugging endpoint is announced when it is on', ok: true });
  } else {
    await openSection(cdp, 'Help');
    const said = await cdp.evaluate(`(() => {
      const aside = [...document.querySelectorAll('aside')].find((a) =>
        a.innerText.startsWith('Help'),
      );
      return aside ? aside.innerText.includes('Remote debugging is on') : false;
    })()`);
    await cdp.evaluate(
      `window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' })), null`,
    );
    checks.push({
      name: 'a debugging endpoint is announced when it is on',
      ok: said,
      detail: said ? undefined : `port ${port} is open but the Help drawer does not say so`,
    });
  }

  return checks;
}

/**
 * The layout tree, end to end.
 *
 * This is the Phase 3 evidence as a repeatable suite: nested tabs, a guard that
 * hides its block, split weights, selection surviving a data tick, keyboard
 * navigation, and Escape closing the panel.
 */
async function panels(cdp, { dataDir }) {
  const checks = [];
  const check = (name, ok, detail) => checks.push({ name, ok: Boolean(ok), detail });

  const restore = await installPanels(dataDir, [
    {
      id: 'live-check',
      title: 'Live Check',
      icon: 'folder',
      width: 340,
      sources: [{ id: 'clock', type: 'builtin', name: 'clock' }],
      layout: {
        node: 'tabs',
        tabs: [
          {
            label: 'Live',
            children: [
              { kind: 'data', label: 'Epoch', value: { '@path': '/clock/epoch' } },
              { kind: 'data', label: 'Minute', value: { '@path': '/clock/minute' } },
            ],
          },
          {
            label: 'Nested',
            children: [
              {
                node: 'tabs',
                tabs: [
                  { label: 'Inner A', children: [{ kind: 'text', text: 'inner a' }] },
                  { label: 'Inner B', children: [{ kind: 'text', text: 'inner b' }] },
                ],
              },
            ],
          },
          {
            label: 'Guarded',
            children: [
              { kind: 'text', text: 'always shown' },
              {
                kind: 'text',
                text: 'never shown',
                when: { '@gt': [{ '@path': '/clock/epoch' }, 99999999999] },
              },
            ],
          },
          {
            label: 'Split',
            children: [
              {
                node: 'split',
                weights: [2, 1],
                children: [
                  { kind: 'text', text: 'left pane' },
                  { kind: 'text', text: 'right pane' },
                ],
              },
            ],
          },
        ],
      },
    },
  ]);

  try {
    await waitFor(
      cdp,
      `[...document.querySelectorAll('button')].some((b) => b.getAttribute('title') === 'Live Check')`,
      { label: 'the panel to appear in the rail' },
    );
    await openPanel(cdp, 'Live Check');
    await waitFor(cdp, `document.querySelectorAll('[role=tab]').length === 4`, {
      label: 'four tabs',
    });

    const aria = await cdp.evaluate(`(() => {
      const tabs = [...document.querySelectorAll('[role=tab]')];
      return {
        selected: tabs.filter((t) => t.getAttribute('aria-selected') === 'true').length,
        tabbable: tabs.filter((t) => t.getAttribute('tabindex') === '0').length,
        labelled: tabs.every((t) => t.getAttribute('aria-controls')),
      };
    })()`);
    check('exactly one tab is selected', aria.selected === 1, JSON.stringify(aria));
    check('exactly one tab is tabbable', aria.tabbable === 1, JSON.stringify(aria));
    check('every tab names its panel', aria.labelled, JSON.stringify(aria));

    // The clock is the only built-in that moves on its own, so it is the one
    // that proves the data plane is live rather than a snapshot.
    const before = await cdp.evaluate(
      `[...document.querySelectorAll('.vd-tabular')].map((e) => e.textContent)`,
    );
    await sleep(3200);
    const after = await cdp.evaluate(
      `[...document.querySelectorAll('.vd-tabular')].map((e) => e.textContent)`,
    );
    check(
      'a built-in source advances without a reload',
      JSON.stringify(before) !== JSON.stringify(after),
      `${JSON.stringify(before)} -> ${JSON.stringify(after)}`,
    );
    check(
      'a live integer stays whole',
      after.every((text) => !text.includes('.')),
      JSON.stringify(after),
    );
    check(
      'tab selection survives a data tick',
      (await cdp.evaluate(
        `[...document.querySelectorAll('[role=tab]')].filter((t) => t.getAttribute('aria-selected') === 'true').map((t) => t.textContent).join()`,
      )) === 'Live',
    );

    await clickByText(cdp, 'Nested');
    await sleep(300);
    const nested = await cdp.evaluate(
      `[...document.querySelectorAll('[role=tab]')].map((t) => t.textContent)`,
    );
    check(
      'a tabs node nests inside a tab',
      nested.includes('Inner A') && nested.includes('Inner B'),
      JSON.stringify(nested),
    );

    await clickByText(cdp, 'Guarded');
    await sleep(300);
    const guarded = await cdp.evaluate(
      `document.querySelector('[role=tabpanel]')?.innerText ?? ''`,
    );
    check('a true guard shows its block', guarded.includes('always shown'), guarded);
    check('a false guard hides its block', !guarded.includes('never shown'), guarded);

    await clickByText(cdp, 'Split');
    await sleep(300);
    const panes = await cdp.evaluate(
      `[...document.querySelectorAll('[role=tabpanel] > div > div')].map((d) => d.style.flex)`,
    );
    check(
      'split weights become proportions',
      panes[0]?.includes('66.6') && panes[1]?.includes('33.3'),
      JSON.stringify(panes),
    );

    await clickByText(cdp, 'Live');
    await sleep(300);
    const keyboard = await cdp.evaluate(`(async () => {
      const tabs = [...document.querySelectorAll('[role=tab]')];
      tabs[0].focus();
      tabs[0].dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true }));
      await new Promise((r) => setTimeout(r, 200));
      const moved = [...document.querySelectorAll('[role=tab]')]
        .filter((t) => t.getAttribute('aria-selected') === 'true')
        .map((t) => t.textContent);
      const focused = document.activeElement?.textContent;
      document.activeElement?.dispatchEvent(new KeyboardEvent('keydown', { key: 'End', bubbles: true }));
      await new Promise((r) => setTimeout(r, 200));
      const ended = [...document.querySelectorAll('[role=tab]')]
        .filter((t) => t.getAttribute('aria-selected') === 'true')
        .map((t) => t.textContent);
      return { moved, focused, ended };
    })()`);
    check(
      'ArrowRight moves the selection',
      keyboard.moved.includes('Nested'),
      JSON.stringify(keyboard),
    );
    check('focus follows the selection', keyboard.focused === 'Nested', JSON.stringify(keyboard));
    check('End jumps to the last tab', keyboard.ended.includes('Split'), JSON.stringify(keyboard));

    const closed = await cdp.evaluate(`(async () => {
      document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
      await new Promise((r) => setTimeout(r, 400));
      return document.querySelectorAll('[role=tab]').length === 0;
    })()`);
    check('Escape closes the panel', closed);
  } finally {
    await restore();
  }

  return checks;
}

/**
 * The external source path, against a real loopback server.
 *
 * This is the half of the data plane that unit tests can only simulate: a
 * `panels.json` declaring an `http` source, the consent prompt it raises, the
 * fetch that starts once the user allows it, and the value updating in the
 * webview without a reload.
 */
async function data(cdp, { dataDir }) {
  const checks = [];
  const check = (name, ok, detail) => checks.push({ name, ok: Boolean(ok), detail });

  const server = await startLoopback();
  const origin = `http://127.0.0.1:${server.port}`;
  const restore = await installPanels(dataDir, [
    {
      id: 'live-data',
      title: 'Live Data',
      icon: 'folder',
      width: 300,
      sources: [
        {
          id: 'ci',
          type: 'http',
          url: `${origin}/status`,
          mode: 'poll',
          intervalMs: 1000,
          timeoutMs: 5000,
        },
      ],
      layout: {
        node: 'stack',
        children: [{ kind: 'data', label: 'Hits', value: { '@path': '/ci/value' } }],
      },
    },
  ]);

  try {
    await waitFor(
      cdp,
      `[...document.querySelectorAll('button')].some((b) => b.getAttribute('title') === 'Live Data')`,
      { label: 'the panel to appear in the rail' },
    );
    await openPanel(cdp, 'Live Data');

    // A declared source is not a permitted one. The prompt is the only way the
    // ledger ever gains an entry, so if this never appears the feature is dead.
    await waitFor(
      cdp,
      `[...document.querySelectorAll('button')].some((b) => b.textContent.trim() === 'Allow')`,
      { label: 'the consent prompt', timeoutMs: 20000 },
    );
    check('a declared source asks before it fetches', true);
    check('nothing is fetched before consent', server.hits() === 0, `${server.hits()} hits`);

    const badge = await panelHeader(cdp, 'Live Data');
    check('the panel says it needs permission', /needs permission/i.test(badge), badge);

    await clickByText(cdp, 'Allow');

    await waitFor(
      cdp,
      `[...document.querySelectorAll('.vd-tabular')].some((e) => e.textContent !== '—')`,
      { label: 'the first fetch to land', timeoutMs: 20000 },
    );

    const first = await cdp.evaluate(
      `[...document.querySelectorAll('.vd-tabular')].map((e) => e.textContent).join()`,
    );
    await sleep(3500);
    const second = await cdp.evaluate(
      `[...document.querySelectorAll('.vd-tabular')].map((e) => e.textContent).join()`,
    );

    check('a loopback source answers once allowed', first !== '—' && first !== '', first);
    check('a loopback source keeps polling', first !== second, `${first} -> ${second}`);
    check('the server saw more than one request', server.hits() > 1, `${server.hits()} hits`);

    const live = await panelHeader(cdp, 'Live Data');
    check('the panel reports itself live', /live/i.test(live), live);
  } finally {
    await restore();
    await server.stop();
    // The ledger is the user's, not the harness's: clear the decision so a dead
    // port is not remembered as allowed.
    await cdp
      .evaluate(
        `window.__TAURI_INTERNALS__.invoke('data_consent_set', { origin: ${JSON.stringify(origin)}, decision: null })`,
      )
      .catch(() => {});
  }

  return checks;
}

/**
 * A plugin that ships a panel, and what happens when it is disabled.
 *
 * The plan's Phase 4 evidence, as a suite. It writes into `~/.vibe/plugins`
 * under a name it owns and removes it again.
 */
async function plugins(cdp) {
  const checks = [];
  const check = (name, ok, detail) => checks.push({ name, ok: Boolean(ok), detail });

  const restore = await installPlugin({
    panels: [
      {
        id: 'status',
        title: 'Plugin Panel',
        icon: 'folder',
        width: 300,
        layout: {
          node: 'stack',
          children: [{ kind: 'text', text: 'shipped by a plugin' }],
        },
      },
    ],
  });

  try {
    // Nothing watches the plugin directory, so the plugin is discovered by the
    // same command the "Reload plugins" button runs — which also re-reads the
    // panel list. Reloading the page would not help: the Rust side caches the
    // plugin list, so a remount re-reads the cache, not the disk.
    await clickByTitlePrefix(cdp, 'Reload plugins');
    await waitFor(
      cdp,
      `[...document.querySelectorAll('button')].some((b) => b.getAttribute('title') === 'Settings')`,
      { label: 'the rail' },
    );

    // A newly discovered plugin is disabled, so its panels are not merged. The
    // enable step is part of the lifecycle, not a setup detail. The enabled set
    // is persisted by name, so a plugin left enabled by an earlier run is
    // already on and the toggle reads the other way.
    // The rail button toggles the drawer, so it is opened only when this
    // plugin's card is not in the DOM — clicking it on an open drawer closes it.
    const toggleExpr = pluginToggleExpr(PLUGIN_DISPLAY);
    if ((await cdp.evaluate(toggleExpr)) === null) await openSection(cdp, 'Plugins');
    await waitFor(cdp, `${toggleExpr} !== null`, {
      label: 'the plugin toggle',
      timeoutMs: 20000,
    });
    if ((await cdp.evaluate(toggleExpr)) === 'Enable plugin') {
      await clickPluginToggle(cdp, 'Enable plugin', PLUGIN_DISPLAY);
    }
    await waitFor(
      cdp,
      `[...document.querySelectorAll('button')].some((b) => (b.getAttribute('title') ?? '').includes('Plugin Panel'))`,
      { label: 'the plugin panel to appear', timeoutMs: 20000 },
    );

    const title = await cdp.evaluate(
      `[...document.querySelectorAll('button')].map((b) => b.getAttribute('title')).find((t) => t && t.includes('Plugin Panel'))`,
    );
    check('enabling a plugin adds its panel to the rail', Boolean(title), String(title));
    check('a plugin panel is marked as one', String(title).includes(PLUGIN_NAME), String(title));

    await clickPluginToggle(cdp, 'Disable plugin', PLUGIN_DISPLAY);
    await waitFor(
      cdp,
      `![...document.querySelectorAll('button')].some((b) => (b.getAttribute('title') ?? '').includes('Plugin Panel'))`,
      { label: 'the plugin panel to disappear', timeoutMs: 20000 },
    );
    check('disabling the plugin removes its panel', true);
  } finally {
    await restore();
  }

  return checks;
}

/**
 * The motion budget.
 *
 * The plan's claim is not that the app looks a certain way — it is that the app
 * spends nothing on motion it does not need. Every check here measures that
 * budget rather than judging the taste, so a regression is a number that moved
 * and not an opinion.
 */
async function motion(cdp, { dataDir }) {
  const checks = [];
  const check = (name, ok, detail) => checks.push({ name, ok: Boolean(ok), detail });

  /** Every element whose computed value for one property is not the default. */
  const offenders = (property, ignore) =>
    cdp.evaluate(`(() => {
      const hits = [];
      for (const el of document.querySelectorAll('*')) {
        const value = getComputedStyle(el).${property};
        if (value && value !== ${JSON.stringify(ignore)}) hits.push(el.className || el.tagName);
      }
      return hits.slice(0, 8);
    })()`);

  // 1. Nothing is animating at rest. A loop that runs with no work in flight is
  //    a loop that is spending frames on nothing.
  await sleep(800);
  const running = await cdp.evaluate(`document.getAnimations()
    .filter((a) => a.playState === 'running')
    .filter((a) => a.timeline?.constructor?.name !== 'ScrollTimeline')
    .map((a) => a.animationName ?? a.transitionProperty ?? '?')`);
  check('nothing is animating at idle', running.length === 0, running.join(', '));

  // 2. No element asks the GPU for a per-pixel backdrop pass.
  const backdrop = await offenders('backdropFilter', 'none');
  check('no element resolves a backdrop-filter', backdrop.length === 0, backdrop.join(', '));

  // 3. No SVG filter on an HTML element: Chromium renders those on the CPU.
  const svgFilter = await cdp.evaluate(`(() => {
    const hits = [];
    for (const el of document.querySelectorAll('*')) {
      const value = getComputedStyle(el).filter;
      if (value && value.includes('url(')) hits.push(el.className || el.tagName);
    }
    return hits.slice(0, 8);
  })()`);
  check('no element resolves an SVG filter', svgFilter.length === 0, svgFilter.join(', '));

  // 4. No layer is promoted at rest. `will-change` allocates a full-resolution
  //    texture per element, and at rest there is nothing to promote for.
  const promoted = await offenders('willChange', 'auto');
  check('no element promotes a layer at rest', promoted.length === 0, promoted.join(', '));

  // 5. The transcript is contained, which is the largest single cost in the app.
  //
  //    The rule is read from the stylesheet, not from live rows. A fresh install
  //    has no conversation, and the `commands` suite ends with `/clear`, so a
  //    check that needs rows to exist fails for a reason that has nothing to do
  //    with containment. The rule is what ships; the rows are asserted on top of
  //    it whenever a conversation happens to be open.
  const containment = await cdp.evaluate(`(() => {
    const found = [];
    const walk = (rules) => {
      for (const rule of rules) {
        if (rule.selectorText === '.vd-row') found.push(rule);
        if (rule.cssRules && rule.cssRules.length) walk(rule.cssRules);
      }
    };
    for (const sheet of document.styleSheets) {
      try {
        walk(sheet.cssRules);
      } catch {
        // A cross-origin sheet cannot be read; there are none in this app.
      }
    }
    const rule = found[0];
    const rows = [...document.querySelectorAll('.vd-row')];
    return {
      found: Boolean(rule),
      contentVisibility: rule?.style?.contentVisibility ?? null,
      intrinsic: rule?.style?.getPropertyValue('contain-intrinsic-size') ?? null,
      count: rows.length,
      contained: rows.filter((el) => getComputedStyle(el).contentVisibility === 'auto').length,
    };
  })()`);
  check(
    'the transcript row rule contains its rows',
    containment.contentVisibility === 'auto',
    JSON.stringify(containment),
  );
  check(
    'the transcript row rule reserves an intrinsic size',
    Boolean(containment.intrinsic),
    String(containment.intrinsic),
  );
  if (containment.count > 0) {
    check(
      'every live transcript row is contained',
      containment.contained === containment.count,
      `${containment.contained}/${containment.count} rows`,
    );
  }

  // 6. A panel opens inside a view transition, and the snapshot is bounded to
  //    the panel column. A window snapshot to animate a 380 px drawer is the
  //    wrong tool; a named snapshot of the one element that changed is the
  //    right one, and the root snapshots have to be neutralised while it runs
  //    or the window freezes on a stale picture.
  //
  //    The counters are installed once and removed in the `finally`: a suite
  //    that leaves a wrapper on `document` makes the next run count its own
  //    wrapper, and a check that fails on the second run is a check nobody
  //    trusts.
  await cdp.evaluate(`(() => {
    window.__vdTransitions = 0;
    window.__vdMask = 0;
    const start = document.startViewTransition?.bind(document);
    const animate = document.documentElement.animate.bind(document.documentElement);
    document.startViewTransition = (callback) => {
      window.__vdTransitions += 1;
      return start(callback);
    };
    document.documentElement.animate = (keyframes, options) => {
      if (options?.pseudoElement === '::view-transition-new(root)') window.__vdMask += 1;
      return animate(keyframes, options);
    };
    window.__vdRestore = () => {
      if (start) document.startViewTransition = start;
      else delete document.startViewTransition;
      document.documentElement.animate = animate;
      delete window.__vdRestore;
    };
    return true;
  })()`);

  const themeBefore = await cdp.evaluate(`document.documentElement.dataset.theme`);
  // Read before the `try`, because the tiered check after the `finally`
  // restores to it and a `const` declared inside the `try` is out of scope.
  const motionBefore = await cdp.evaluate(`document.documentElement.dataset.motion`);
  // The button's title names the theme it switches *to*, so it flips with the
  // theme. Reading the current value each time is the only way a suite that
  // toggles more than once can find it.
  const toggleTheme = async () => {
    const now = await cdp.evaluate(`document.documentElement.dataset.theme`);
    await clickByTitle(cdp, now === 'ink' ? 'Switch to paper' : 'Switch to ink');
  };
  try {
    await openSection(cdp, 'Configuration');
    await sleep(400);
    const onPanel = await cdp.evaluate(`window.__vdTransitions`);
    check('a panel open runs a view transition', onPanel >= 1, `${onPanel} transitions`);

    // The snapshot has to be the panel column, and the window snapshots have to
    // be neutralised while it runs. Both are read from the stylesheet rather
    // than from a live pseudo-element, because the pseudo-element tree only
    // exists for the length of a transition and a check that samples it is a
    // check that races.
    const bounded = await cdp.evaluate(`(() => {
      const decls = (needle) => {
        const out = [];
        const walk = (rules) => {
          for (const rule of rules) {
            if (rule.selectorText && rule.selectorText.includes(needle)) {
              out.push(rule.style.cssText);
            }
            if (rule.cssRules) walk(rule.cssRules);
          }
        };
        for (const sheet of document.styleSheets) {
          try { walk(sheet.cssRules); } catch { /* the app has no cross-origin sheet */ }
        }
        return out.join(' ');
      };
      return {
        panelName: decls('data-vt'),
        rootOld: decls('::view-transition-old(root)'),
        rootNew: decls('::view-transition-new(root)'),
        neutral: decls(':root[data-vt]'),
      };
    })()`);
    check(
      'the panel column is the named snapshot',
      /view-transition-name:\s*vd-panel/.test(bounded.panelName),
      bounded.panelName.slice(0, 120),
    );
    check(
      'the window snapshot is neutralised during a bounded transition',
      /--vd-vt-root-old:\s*0/.test(bounded.neutral) && /--vd-vt-root-mask:\s*none/.test(bounded.neutral),
      bounded.neutral.slice(0, 120),
    );
    check(
      'the theme reveal still owns the window snapshot',
      /--vd-vt-root-old/.test(bounded.rootOld) && /--vd-vt-root-mask/.test(bounded.rootNew),
      `${bounded.rootOld.slice(0, 60)} | ${bounded.rootNew.slice(0, 60)}`,
    );

    // 7. The theme reveal is the one whole-window effect, and it is one
    //    animation on one snapshot layer with both pseudo-elements frozen.
    //    The reveal is skipped when the OS asks for reduced motion, which is
    //    the correct behaviour, so the check asks for the opposite first.
    await cdp.send('Emulation.setEmulatedMedia', {
      features: [{ name: 'prefers-reduced-motion', value: 'no-preference' }],
    });
    await sleep(150);
    await cdp.evaluate(`((window.__vdTransitions = 0), (window.__vdMask = 0), true)`);
    await clickByTitle(cdp, themeBefore === 'ink' ? 'Switch to paper' : 'Switch to ink');
    await sleep(150);
    const reveal = await cdp.evaluate(`({
      transitions: window.__vdTransitions,
      masks: window.__vdMask,
      old: getComputedStyle(document.documentElement, '::view-transition-old(root)').animationName,
      next: getComputedStyle(document.documentElement, '::view-transition-new(root)').animationName,
      theme: document.documentElement.dataset.theme,
    })`);
    check(
      'the theme reveal starts one view transition',
      reveal.transitions === 1,
      JSON.stringify(reveal),
    );
    check('the theme reveal is one mask animation', reveal.masks === 1, JSON.stringify(reveal));
    check(
      'both snapshots are frozen',
      reveal.old === 'none' && reveal.next === 'none',
      JSON.stringify(reveal),
    );
    check('the theme actually changed', reveal.theme !== themeBefore, JSON.stringify(reveal));

    // 7b. The mask has to be sized to nothing in CSS. `auto` on a
    //     `closest-side` gradient is a circle inscribed in the box, so every
    //     frame where the animation is not applying would paint a circle in
    //     the middle of the window — which is exactly what it used to do.
    const maskDefault = await cdp.evaluate(`(() => {
      const walk = (rules) => {
        for (const rule of rules) {
          if (rule.selectorText?.includes('::view-transition-new(root)')) {
            const value = rule.style.getPropertyValue('mask-size');
            if (value) return value;
          }
          if (rule.cssRules) {
            const hit = walk(rule.cssRules);
            if (hit) return hit;
          }
        }
        return null;
      };
      for (const sheet of document.styleSheets) {
        try {
          const hit = walk(sheet.cssRules);
          if (hit) return hit;
        } catch {
          /* a cross-origin sheet cannot be read; the app has none */
        }
      }
      return null;
    })()`);
    check(
      'the incoming snapshot is masked to nothing by default',
      maskDefault === '0px 0px',
      String(maskDefault),
    );

    const fill = await cdp.evaluate(`(() => {
      const animation = document
        .getAnimations()
        .find((a) => a.effect?.pseudoElement === '::view-transition-new(root)');
      return animation ? animation.effect.getTiming().fill : null;
    })()`);
    check('the reveal holds its last frame', fill === 'forwards', String(fill));

    // 7c. The in-app preference outranks the OS, in both directions. This is
    //     the whole point of the setting: Windows reports `reduce` whenever
    //     "Animation effects" is off, which is a machine-wide battery switch,
    //     and a user who wants the reveal should not have to flip it back.
    await cdp.send('Emulation.setEmulatedMedia', {
      features: [{ name: 'prefers-reduced-motion', value: 'reduce' }],
    });
    await cdp.evaluate(`((document.documentElement.dataset.motion = 'full'), true)`);
    await sleep(150);
    await cdp.evaluate(`((window.__vdTransitions = 0), (window.__vdMask = 0), true)`);
    await toggleTheme();
    await sleep(150);
    const forced = await cdp.evaluate(`({
      transitions: window.__vdTransitions,
      masks: window.__vdMask,
      os: matchMedia('(prefers-reduced-motion: reduce)').matches,
    })`);
    check(
      'the in-app preference animates over a reduced-motion OS',
      forced.os === true && forced.transitions === 1 && forced.masks === 1,
      JSON.stringify(forced),
    );

    await sleep(600);
    await cdp.send('Emulation.setEmulatedMedia', {
      features: [{ name: 'prefers-reduced-motion', value: 'no-preference' }],
    });
    await cdp.evaluate(`((document.documentElement.dataset.motion = 'reduced'), true)`);
    await sleep(150);
    await cdp.evaluate(`((window.__vdTransitions = 0), (window.__vdMask = 0), true)`);
    await toggleTheme();
    await sleep(150);
    const suppressed = await cdp.evaluate(`({
      transitions: window.__vdTransitions,
      masks: window.__vdMask,
      os: matchMedia('(prefers-reduced-motion: reduce)').matches,
      theme: document.documentElement.dataset.theme,
    })`);
    check(
      'the in-app preference suppresses motion on a full-motion OS',
      suppressed.os === false && suppressed.transitions === 0 && suppressed.masks === 0,
      JSON.stringify(suppressed),
    );
    // The theme still has to change when the reveal is skipped. A preference
    // that silently swallows the toggle would be worse than no preference.
    check(
      'a suppressed reveal still switches the theme',
      suppressed.theme !== themeBefore,
      JSON.stringify(suppressed),
    );

    await cdp.evaluate(
      `((document.documentElement.dataset.motion = ${JSON.stringify(motionBefore ?? 'system')}), true)`,
    );
  } finally {
    // Put the theme back, then the emulated media, then the patched globals.
    // A loop, not one click: the suite toggles three times, and a check that
    // threw part-way leaves the theme somewhere in between.
    await sleep(600);
    for (let attempt = 0; attempt < 4; attempt += 1) {
      const now = await cdp.evaluate(`document.documentElement.dataset.theme`).catch(() => null);
      if (now === null || now === themeBefore) break;
      await toggleTheme().catch(() => {});
      await sleep(700);
    }
    await cdp.send('Emulation.setEmulatedMedia', { features: [] });
    await cdp.evaluate(`(window.__vdRestore?.(), true)`);
  }

  // 8. Reduced motion is tiered, not binary. Colour and opacity keep their
  //    duration because they aid comprehension and do not move; small and
  //    large transforms halve; an explicit `reduced` drops the transforms
  //    entirely. The old rule collapsed everything to 1ms, which is not
  //    reduced motion — it is no motion, and it is why the interface read as
  //    abrupt rather than as calm.
  const probeDurations = () =>
    cdp.evaluate(`(() => {
      const probe = document.createElement('div');
      probe.style.transition =
        'opacity var(--vd-dur-1) linear, transform var(--vd-fast) linear, grid-template-rows var(--vd-slow) linear';
      document.body.appendChild(probe);
      const value = getComputedStyle(probe).transitionDuration;
      probe.remove();
      return value.split(',').map((part) => Number.parseFloat(part));
    })()`);

  await cdp.send('Emulation.setEmulatedMedia', {
    features: [{ name: 'prefers-reduced-motion', value: 'reduce' }],
  });
  await cdp.evaluate(`((document.documentElement.dataset.motion = 'system'), true)`);
  await sleep(200);
  const tiered = await probeDurations();
  check(
    'a reduced-motion OS keeps colour and halves movement',
    tiered[0] === 0.09 && tiered[1] === 0.07 && tiered[2] === 0.15,
    JSON.stringify(tiered),
  );

  await cdp.evaluate(`((document.documentElement.dataset.motion = 'reduced'), true)`);
  await sleep(200);
  const dropped = await probeDurations();
  check(
    'an explicit reduced preference drops movement and keeps colour',
    dropped[0] === 0.09 && dropped[1] === 0 && dropped[2] === 0,
    JSON.stringify(dropped),
  );

  await cdp.evaluate(`((document.documentElement.dataset.motion = 'full'), true)`);
  await sleep(200);
  const full = await probeDurations();
  check(
    'a full preference restores every duration',
    full[0] === 0.09 && full[1] === 0.14 && full[2] === 0.3,
    JSON.stringify(full),
  );

  await cdp.evaluate(
    `((document.documentElement.dataset.motion = ${JSON.stringify(motionBefore ?? 'system')}), true)`,
  );
  await cdp.send('Emulation.setEmulatedMedia', { features: [] });

  // 9. The preference is a real setting: a control in the Customize drawer that
  //    writes `layout.json` through Rust and comes back applied. An attribute a
  //    test can set is not a feature; a control the user can click is.
  const layoutPath = join(dataDir, 'config', 'layout.json');
  // Restored through the same control that changed it, so the store and the
  // file cannot end up disagreeing after the suite runs.
  const motionWas = await cdp.evaluate(`document.documentElement.dataset.motion ?? 'system'`);
  const pickMotion = (label) => `(() => {
    const heading = [...document.querySelectorAll('span')]
      .find((el) => el.textContent.trim() === 'Motion');
    const group = heading?.parentElement;
    const button = [...(group?.querySelectorAll('button') ?? [])]
      .find((b) => b.textContent.trim() === ${JSON.stringify(label)});
    if (!button) return false;
    button.click();
    return true;
  })()`;

  try {
    await openSection(cdp, 'Customize');
    await sleep(400);

    const clicked = await cdp.evaluate(pickMotion('Full'));
    check('the Customize drawer offers a motion control', clicked, String(clicked));
    await sleep(500);

    const applied = await cdp.evaluate(`document.documentElement.dataset.motion`);
    check('choosing Full applies immediately', applied === 'full', String(applied));

    let onDisk = null;
    try {
      onDisk = JSON.parse(await readFile(layoutPath, 'utf8')).motion ?? null;
    } catch {
      onDisk = null;
    }
    check('the motion preference reaches layout.json', onDisk === 'full', String(onDisk));

    // And back to whatever it was, so the suite leaves the machine as it found
    // it and the store agrees with the file.
    const back = motionWas.charAt(0).toUpperCase() + motionWas.slice(1);
    await cdp.evaluate(pickMotion(back));
    await sleep(500);
    const restored = await cdp.evaluate(`document.documentElement.dataset.motion`);
    check(
      'the motion preference can be set back',
      restored === motionWas,
      `${restored} vs ${motionWas}`,
    );
  } finally {
    // A safety net for the case where a check above threw before the restore.
    // It clicks the same control, so the store and `layout.json` stay in step.
    const back = motionWas.charAt(0).toUpperCase() + motionWas.slice(1);
    await cdp
      .evaluate(`(() => {
        if (document.documentElement.dataset.motion === ${JSON.stringify(motionWas)}) return true;
        return ${pickMotion(back)};
      })()`)
      .catch(() => {});
    await sleep(400);
  }

  return checks;
}

/**
 * The shape vocabulary, and a live number that moves instead of jumping.
 *
 * Phase 6 of the customization plan. Six shape primitives, five motion
 * primitives, and the two things a screenshot cannot prove: that a tween is
 * actually interpolating, and that a changing value does not move the layout
 * around it.
 */
async function shape(cdp, { dataDir }) {
  const checks = [];
  const check = (name, ok, detail) => checks.push({ name, ok: Boolean(ok), detail });

  /** Every declaration of one property, from the app's sheets. */
  const property = (name) =>
    cdp.evaluate(`(() => {
      const out = [];
      const walk = (rules) => {
        for (const rule of rules) {
          if (rule.style && rule.style.getPropertyValue(${JSON.stringify(name)})) {
            out.push(rule.selectorText + ' { ' + rule.style.getPropertyValue(${JSON.stringify(name)}) + ' }');
          }
          if (rule.cssRules) walk(rule.cssRules);
        }
      };
      for (const sheet of document.styleSheets) {
        try { walk(sheet.cssRules); } catch { /* the app has no cross-origin sheet */ }
      }
      return out.join(String.fromCharCode(10));
    })()`);

  /** Every declaration whose selector contains `needle`, from the app's sheets. */
  const declarations = (needle) =>
    cdp.evaluate(`(() => {
      const out = [];
      const walk = (rules) => {
        for (const rule of rules) {
          if (rule.selectorText && rule.selectorText.includes(${JSON.stringify(needle)})) {
            out.push(rule.selectorText + ' { ' + rule.style.cssText + ' }');
          }
          if (rule.cssRules) walk(rule.cssRules);
        }
      };
      for (const sheet of document.styleSheets) {
        try { walk(sheet.cssRules); } catch { /* the app has no cross-origin sheet */ }
      }
      return out.join(String.fromCharCode(10));
    })()`);

  // 1. All six primitives exist. A vocabulary that is half defined is a
  //    vocabulary nobody can use.
  const primitives = {
    squircle: await property('corner-shape'),
    blob: await declarations('.vd-blob'),
    mesh: await declarations('.vd-mesh'),
    grain: await declarations('.vd-grain'),
    gooey: await declarations('.vd-gooey'),
    gloss: await declarations('.vd-gloss'),
  };
  for (const [name, css] of Object.entries(primitives)) {
    check(`the ${name} primitive is defined`, css.length > 0, css.slice(0, 80));
  }

  // 2. The gooey filter has a definition in the document. A `url(#id)` with no
  //    matching filter is a silent no-op, which is the worst kind of shape.
  const filter = await cdp.evaluate(
    `Boolean(document.querySelector('filter#vd-gooey feGaussianBlur'))`,
  );
  check('the gooey filter has a definition', filter);

  // 3. Nothing in the chrome resolves a filter at rest. The gooey primitive is
  //    a real GPU pass, so it is only ever on an element that exists while a
  //    turn is running.
  const filters = await cdp.evaluate(`(() => {
    const hits = [];
    for (const el of document.querySelectorAll('*')) {
      const value = getComputedStyle(el).filter;
      if (value && value.includes('url(')) hits.push(el.className || el.tagName);
    }
    return hits.slice(0, 8);
  })()`);
  check('no element resolves a filter at rest', filters.length === 0, filters.join(', '));

  // 4. The empty state composes three primitives on one surface. This is the
  //    only place a mesh gradient belongs, and it is reached the way a user
  //    reaches it: `/clear` in the composer.
  await waitFor(cdp, `Boolean(document.querySelector('textarea.vd-input'))`, {
    label: 'the composer',
    timeoutMs: 20000,
  });
  const clear = await cdp.evaluate(`(() => {
    const input = document.querySelector('textarea.vd-input');
    if (!input) return false;
    const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value').set;
    setter.call(input, '/clear');
    input.dispatchEvent(new Event('input', { bubbles: true }));
    input.focus();
    return true;
  })()`);
  check('the composer accepts a slash command', clear);
  await sleep(250);
  // Two Enters, because that is what the composer does: the first completes
  // the command from the palette into the draft, the second runs it. A suite
  // that presses once is testing the palette, not the command.
  const pressEnter = () =>
    cdp.evaluate(`(() => {
      const input = document.querySelector('textarea.vd-input');
      input?.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
      return true;
    })()`);
  await pressEnter();
  await sleep(300);
  await pressEnter();

  // A suite that throws loses every check it already made, so the wait is
  // caught and reported as a named failure with the state that explains it.
  let reached = true;
  try {
    await waitFor(cdp, `Boolean(document.querySelector('.vd-mesh'))`, {
      label: 'the empty state after /clear',
      timeoutMs: 15000,
    });
  } catch {
    reached = false;
  }
  if (!reached) {
    const after = await cdp.evaluate(`({
      rows: document.querySelectorAll('.vd-row').length,
      draft: document.querySelector('textarea.vd-input')?.value ?? null,
      toast: document.body.innerText.split(String.fromCharCode(10)).slice(0, 6).join(' | '),
    })`);
    check('the empty state is reachable with /clear', false, JSON.stringify(after));
    return checks;
  }
  check('the empty state is reachable with /clear', true);

  const empty = await cdp.evaluate(`(() => {
    const el = document.querySelector('.vd-mesh');
    if (!el) return null;
    return {
      blob: el.classList.contains('vd-blob'),
      grain: el.classList.contains('vd-grain'),
      radius: getComputedStyle(el).borderRadius,
      mesh: getComputedStyle(el).backgroundImage.includes('radial-gradient'),
    };
  })()`);
  check('the empty state carries the mesh gradient', empty?.mesh === true, JSON.stringify(empty));
  check('the mesh is clipped to a blob', empty?.blob === true, JSON.stringify(empty));
  check('the blob is an eight-value radius', (empty?.radius ?? '').includes('/'), empty?.radius);
  check('the mesh carries grain', empty?.grain === true, JSON.stringify(empty));

  // 5. The springs are generated, not the cubic-bezier fallback, and the soft
  //    one overshoots — which is the whole difference between a spring and an
  //    ease.
  const springs = await cdp.evaluate(`(() => {
    const read = (name) => getComputedStyle(document.documentElement).getPropertyValue(name).trim();
    const parse = (value) => {
      const inner = value.slice(value.indexOf('(') + 1, value.lastIndexOf(')'));
      return inner.split(',').map((part) => Number.parseFloat(part));
    };
    const soft = read('--vd-spring-soft');
    const snappy = read('--vd-spring-snappy');
    return {
      soft,
      snappy,
      softStops: soft.startsWith('linear(') ? parse(soft).length : 0,
      softPeak: soft.startsWith('linear(') ? Math.max(...parse(soft)) : 0,
      snappyStops: snappy.startsWith('linear(') ? parse(snappy).length : 0,
    };
  })()`);
  check(
    'the soft spring is a generated linear() curve',
    springs.softStops > 10,
    `${springs.softStops} stops: ${springs.soft.slice(0, 60)}`,
  );
  check(
    'the snappy spring is a generated linear() curve',
    springs.snappyStops > 10,
    `${springs.snappyStops} stops`,
  );
  check(
    'the soft spring overshoots',
    springs.softPeak > 1.005,
    `peak ${springs.softPeak}`,
  );

  // 6. A live number tweens. The panel binds a clock to a value that jumps by a
  //    thousand each tick, so an interpolating tween produces many distinct
  //    readings inside one tick and a jumping one produces two.
  const restore = await installPanels(dataDir, [
    {
      id: 'shape-tween',
      title: 'Shape Tween',
      icon: 'folder',
      width: 300,
      sources: [{ id: 'clock', type: 'builtin', name: 'clock' }],
      layout: {
        node: 'stack',
        children: [
          {
            kind: 'data',
            label: 'Scaled',
            value: { '@mul': [{ '@path': '/clock/epoch' }, 1000] },
          },
        ],
      },
    },
  ]);

  try {
    await waitFor(
      cdp,
      `[...document.querySelectorAll('button')].some((b) => b.getAttribute('title') === 'Shape Tween')`,
      { label: 'the tween panel to appear in the rail' },
    );
    await openPanel(cdp, 'Shape Tween');
    await waitFor(
      cdp,
      `document.querySelector('.vd-panel-surface .vd-tabular')?.textContent !== '—'`,
      { label: 'the first value' },
    );

    // The panel's own entrance is a 4 px translate. Sampling through it would
    // measure the arrival, not the value, so it is allowed to finish first.
    await sleep(700);

    // A page that is not the active one has its animation frames throttled, and
    // a tween driven by them simply does not advance: this check then reads one
    // value 150 times and reports a broken tween. It happened once in `all` and
    // never alone. Asking for the front is what makes the measurement possible;
    // the frame timings below say whether it was granted, so a failure names a
    // stalled page rather than a stalled animation.
    await cdp.send('Page.bringToFront');

    // Two and a half seconds of wall clock, so at least two clock ticks land
    // inside the window and a tween that never runs cannot pass by sampling a
    // still value. It used to be 150 frames, which is not a duration: 2.5 s on a
    // 60 Hz display and 1.04 s on this machine's 144 Hz one — measured, not
    // guessed, from a failure that read `150 frames in 1039 ms, visible,
    // focused=true`. The window had quietly halved and the check began failing
    // in `all` while passing alone.
    const readings = await cdp.evaluate(`(async () => {
      const seen = new Set();
      const boxes = new Set();
      const el = document.querySelector('.vd-panel-surface .vd-tabular');
      const started = performance.now();
      while (performance.now() - started < 2500) {
        seen.add(el.textContent);
        const box = el.getBoundingClientRect();
        boxes.add([box.x, box.y, box.width, box.height].join(':'));
        await new Promise((r) => requestAnimationFrame(r));
      }
      return {
        values: [...seen],
        boxes: [...boxes],
        framesMs: Math.round(performance.now() - started),
        visibility: document.visibilityState,
        focused: document.hasFocus(),
      };
    })()`);
    check(
      'a live number tweens instead of jumping',
      readings.values.length > 2,
      `${readings.values.length} distinct readings, 150 frames in ${readings.framesMs} ms, ` +
        `${readings.visibility}, focused=${readings.focused}`,
    );
    check(
      'a changing number does not move the layout',
      readings.boxes.length === 1,
      `${readings.boxes.length} distinct boxes`,
    );

    // 7. Both themes render the panel. A shape that only works on one canvas is
    //    half a shape.
    const themeBefore = await cdp.evaluate(`document.documentElement.dataset.theme`);
    const ink = await cdp.evaluate(`(() => {
      const el = document.querySelector('.vd-panel-surface .vd-tabular');
      return { colour: getComputedStyle(el).color, canvas: getComputedStyle(document.body).backgroundColor };
    })()`);
    await clickByTitle(cdp, themeBefore === 'ink' ? 'Switch to paper' : 'Switch to ink');
    await sleep(900);
    const paper = await cdp.evaluate(`(() => {
      const el = document.querySelector('.vd-panel-surface .vd-tabular');
      return { colour: getComputedStyle(el).color, canvas: getComputedStyle(document.body).backgroundColor };
    })()`);
    check(
      'the panel renders in both themes',
      ink.colour !== paper.colour && ink.canvas !== paper.canvas,
      `${ink.colour} vs ${paper.colour}`,
    );
    await clickByTitle(cdp, themeBefore === 'ink' ? 'Switch to ink' : 'Switch to paper');
    await sleep(900);
  } finally {
    await restore();
  }

  return checks;
}

/**
 * Deleting a chat that is not the one on screen.
 *
 * The guard used to read the CLI's lock directory, where a single CLI process
 * keeps a lock for every session it has ever loaded. That made every chat the
 * user had visited undeletable. The check now compares against the session the
 * agent actually has open, so this suite pins both halves: a locked chat that is
 * not open deletes, and the open one is still protected.
 *
 * Nothing real is deleted. The id under test is a lock file this suite writes
 * itself, with no session directory behind it, so `delete_at` has nothing to
 * move and the only thing exercised is the guard. The open chat is checked
 * through the rail rather than by calling delete on it, because a suite that
 * destroys the user's chat when the guard is broken is worse than no suite.
 */
async function chats(cdp, { dataDir, keep }) {
  const checks = [];
  const check = (name, ok, detail) => checks.push({ name, ok: Boolean(ok), detail });

  const activeDir = join(homedir(), '.vibe', 'logs', 'session', 'active');
  const probeId = `vibedesk-live-check-${process.pid}`;
  const lockPath = join(activeDir, `${probeId}.lock`);
  const metaPath = join(activeDir, `${probeId}.lock.json`);
  const statePath = join(dataDir, 'config', 'chats.json');

  let previousState = null;
  try {
    previousState = await readFile(statePath, 'utf8');
  } catch {
    previousState = null;
  }

  try {
    await mkdir(activeDir, { recursive: true });
    await writeFile(lockPath, '');
    await writeFile(metaPath, JSON.stringify({ process_id: process.pid, session_id: probeId }));

    const open = await cdp.evaluate(
      `window.__TAURI_INTERNALS__.invoke('agent_status').then((status) => status.sessionId)`,
    );
    check('a chat is open', typeof open === 'string' && open.length > 0, `open is ${open}`);

    // The bug: a lock file for a chat that was not on screen was enough to
    // refuse the delete.
    const deleted = await cdp.evaluate(
      `window.__TAURI_INTERNALS__.invoke('chats_delete', { sessionId: ${JSON.stringify(probeId)} })
        .then(() => null, (error) => String(error))`,
    );
    check('a locked chat that is not open can be deleted', deleted === null, deleted ?? undefined);

    // The user's flow, at the UI level: open one chat, switch to another, then
    // look at both menus. The rail marks the open chat with an accent bar and
    // disables its delete item, so this pins that the switch actually moved
    // "open" — the precondition the frontend guard reads.
    const rows = await cdp.evaluate(`[...document.querySelectorAll('aside button')]
      .map((button) => {
        const mono = [...button.querySelectorAll('span.font-mono')].map((span) =>
          span.textContent.trim(),
        );
        return mono.length === 1 ? mono[0] : null;
      })
      .filter(Boolean)`);

    if (rows.length < 2) {
      check('the rail lists two chats to switch between', false, `${rows.length} rows`);
    } else {
      const rowFor = (prefix) => `[...document.querySelectorAll('aside button')].find((button) =>
        [...button.querySelectorAll('span.font-mono')].some(
          (span) => span.textContent.trim() === ${JSON.stringify(prefix)},
        ),
      )`;
      const isOpen = (prefix) =>
        cdp.evaluate(`Boolean((${rowFor(prefix)})?.querySelector('span.absolute'))`);
      const openRow = async (prefix) => {
        await cdp.evaluate(`(${rowFor(prefix)}).click(), null`);
        await waitFor(cdp, `Boolean((${rowFor(prefix)})?.querySelector('span.absolute'))`, {
          label: `the switch to ${prefix}`,
        });
      };
      const menuDelete = async (prefix) => {
        await cdp.evaluate(
          `(${rowFor(prefix)}).dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, clientX: 120, clientY: 120 })), null`,
        );
        await waitFor(cdp, `document.querySelectorAll('[role="menuitem"]').length > 0`, {
          label: 'the chat menu',
        });
        const state = await cdp.evaluate(`(() => {
          const entry = [...document.querySelectorAll('[role="menuitem"]')].find((button) =>
            button.textContent.includes('Delete chat'),
          );
          return entry ? entry.disabled : 'no delete item';
        })()`);
        // Wait for the menu to actually go, or the next call reads this one.
        await cdp.evaluate(
          `window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' })), null`,
        );
        await waitFor(cdp, `document.querySelectorAll('[role="menuitem"]').length === 0`, {
          label: 'the chat menu to close',
        });
        return state;
      };

      const [first, second] = rows;
      await openRow(first);
      await openRow(second);
      check('the chat just switched to cannot be deleted', (await menuDelete(second)) === true);
      check('the chat switched away from can be deleted', (await menuDelete(first)) === false);
      check('the switch left exactly one chat open', (await isOpen(first)) === false);
    }
  } finally {
    if (!keep) {
      await rm(lockPath, { force: true });
      await rm(metaPath, { force: true });
      // The probe id was written into the deleted list; put the file back.
      if (previousState === null) await rm(statePath, { force: true });
      else await writeFile(statePath, previousState);
    }
  }

  return checks;
}

/**
 * Bulk archive and bulk delete.
 *
 * Two things are worth pinning. The backend must skip the open chat instead of
 * aborting the whole call — a bulk delete of ten chats that fails because one
 * is on screen is the bug this feature exists to avoid. And the rail must not
 * offer the open chat for selection at all, so that skip is a backstop rather
 * than the only guard.
 *
 * Nothing real is deleted. The probe ids have no session directory, so
 * `delete_at` has nothing to move and only the bookkeeping is exercised. The
 * state file is put back afterwards.
 */
async function bulk(cdp, { dataDir, keep }) {
  const checks = [];
  const check = (name, ok, detail) => checks.push({ name, ok: Boolean(ok), detail });

  const statePath = join(dataDir, 'config', 'chats.json');
  const probeA = `vibedesk-bulk-a-${process.pid}`;
  const probeB = `vibedesk-bulk-b-${process.pid}`;

  let previousState = null;
  try {
    previousState = await readFile(statePath, 'utf8');
  } catch {
    previousState = null;
  }

  const invoke = (command, args) =>
    cdp.evaluate(
      `window.__TAURI_INTERNALS__.invoke(${JSON.stringify(command)}, ${JSON.stringify(args)})
        .then((value) => ({ ok: true, value }), (error) => ({ ok: false, error: String(error) }))`,
    );

  const label = `[...document.querySelectorAll('aside.vd-sidebar .vd-label')].at(-1)?.textContent.trim()`;
  const toggle = `document.querySelector('aside.vd-sidebar svg.lucide-list-checks')?.closest('button')`;
  const rowFor = (id) => `[...document.querySelectorAll('aside.vd-sidebar button')].find((button) =>
    [...button.querySelectorAll('span.font-mono')].some(
      (span) => span.textContent.trim() === ${JSON.stringify(id)},
    ),
  )`;

  try {
    /* ---- the rail ------------------------------------------------ */

    const rows = await cdp.evaluate(`[...document.querySelectorAll('aside.vd-sidebar button')]
      .map((button) => {
        const mono = [...button.querySelectorAll('span.font-mono')].map((span) =>
          span.textContent.trim(),
        );
        return mono.length === 1 ? { id: mono[0], disabled: button.disabled } : null;
      })
      .filter(Boolean)`);

    if (rows.length < 3) {
      check('the rail lists three chats to select between', false, `${rows.length} rows`);
    } else {
      // A previous run may have left the rail in selection mode; the toggle is
      // a toggle, so normalize before asserting anything about entering it.
      for (let attempt = 0; attempt < 3; attempt += 1) {
        if ((await cdp.evaluate(label)) === 'Chats') break;
        await cdp.evaluate(`(${toggle}).click(), null`);
        await sleep(200);
      }

      // Open a chat first, so the guard has something to guard. A freshly
      // launched app has no chat on screen, and then no row should be disabled.
      const first = rows[0].id;
      await cdp.evaluate(`(${rowFor(first)}).click(), null`);
      await waitFor(cdp, `Boolean((${rowFor(first)})?.querySelector('span.absolute'))`, {
        label: 'the chat to open',
      });

      await cdp.evaluate(`(${toggle}).click(), null`);
      await waitFor(cdp, `${label} === '0 selected'`, { label: 'selection mode' });

      const inMode = await cdp.evaluate(`[...document.querySelectorAll('aside.vd-sidebar button')]
        .map((button) => {
          const mono = [...button.querySelectorAll('span.font-mono')].map((span) =>
            span.textContent.trim(),
          );
          return mono.length === 1 ? { id: mono[0], disabled: button.disabled } : null;
        })
        .filter(Boolean)`);

      const openRow = inMode.find((row) => row.disabled);
      const free = inMode.filter((row) => !row.disabled);
      check(
        'the open chat cannot be selected',
        openRow?.id === first,
        `open row is ${openRow?.id}, expected ${first}`,
      );
      check('the other chats can be selected', free.length >= 2, `${free.length} selectable`);

      if (free.length >= 2) {
        const click = (id) => cdp.evaluate(`(${rowFor(id)}).click(), null`);
        await click(free[0].id);
        await waitFor(cdp, `${label} === '1 selected'`, { label: 'one selected' });

        // Shift extends from the anchor and replaces the selection. The anchor
        // is the row just clicked, so the range is exactly these two.
        await cdp.evaluate(
          `(${rowFor(free[1].id)}).dispatchEvent(new MouseEvent('click', { bubbles: true, shiftKey: true })), null`,
        );
        await waitFor(cdp, `${label} === '2 selected'`, { label: 'the shift range' });
        check('shift-click selects the range from the anchor', true);

        await click(free[1].id);
        await waitFor(cdp, `${label} === '1 selected'`, { label: 'one selected again' });
        check('clicking a selected row deselects it', true);

        const bar = await cdp.evaluate(
          `Boolean(document.querySelector('aside.vd-sidebar svg.lucide-archive')?.closest('button'))`,
        );
        check('the bulk bar offers archive', bar === true, String(bar));
      }

      await cdp.evaluate(`(${toggle}).click(), null`);
      await waitFor(cdp, `${label} === 'Chats'`, { label: 'leaving selection mode' });
      check('leaving selection mode restores the header', true);
    }

    /* ---- the commands -------------------------------------------- */

    const archived = await invoke('chats_archive_many', {
      ids: [probeA, probeB, probeA],
      archived: true,
    });
    check('archiving many succeeds', archived.ok, archived.error);
    if (archived.ok) {
      const ids = archived.value.state.archived;
      check(
        'every id is archived once, repeats dropped',
        ids.filter((id) => id === probeA).length === 1 && ids.includes(probeB),
        JSON.stringify(ids),
      );
      check(
        'archiving many reports what it did',
        archived.value.done.length === 2,
        JSON.stringify(archived.value.done),
      );
    }

    const restored = await invoke('chats_archive_many', {
      ids: [probeA, probeB],
      archived: false,
    });
    check(
      'restoring many clears them again',
      restored.ok &&
        !restored.value.state.archived.includes(probeA) &&
        !restored.value.state.archived.includes(probeB),
      restored.ok ? JSON.stringify(restored.value.state.archived) : restored.error,
    );

    const traversal = await invoke('chats_archive_many', {
      ids: ['../../evil'],
      archived: true,
    });
    check(
      'a traversal id is refused',
      !traversal.ok && traversal.error.includes('not a session id'),
      traversal.ok ? 'accepted' : traversal.error,
    );

    const open = await cdp.evaluate(
      `window.__TAURI_INTERNALS__.invoke('agent_status').then((status) => status.sessionId)`,
    );
    check('a chat is open', typeof open === 'string' && open.length > 0, `open is ${open}`);

    const removed = await invoke('chats_delete_many', { ids: [open, probeA] });
    check('deleting many succeeds', removed.ok, removed.error);
    if (removed.ok) {
      check(
        'the open chat is skipped, not fatal',
        removed.value.skipped.includes(open),
        JSON.stringify(removed.value.skipped),
      );
      check(
        'the other chat is still deleted',
        removed.value.done.includes(probeA),
        JSON.stringify(removed.value.done),
      );
      check(
        'the open chat is not marked deleted',
        !removed.value.state.deleted.includes(open),
        JSON.stringify(removed.value.state.deleted),
      );
    }
  } finally {
    if (!keep) {
      if (previousState === null) await rm(statePath, { force: true });
      else await writeFile(statePath, previousState);
    }
  }

  return checks;
}

/**
 * The `widget` block: agent-authored HTML **and scripts**, on its own origin.
 *
 * This is the suite that proves the execution boundary is real rather than
 * described. Three widgets, each proving one thing the design claims:
 *
 * 1. **Scripts run and live data arrives.** The widget subscribes to the panel
 *    model and asks for a taller frame once it has a value. Nothing but a
 *    running script inside the frame can change the frame's height.
 * 2. **A declared capability round-trips.** The widget fires an action from
 *    `actions.json`; the app runs it (the composer takes focus) and acks.
 * 3. **An undeclared capability is refused.** The widget bypasses the SDK and
 *    posts a raw invoke, which is the only way to test the parent's own check
 *    rather than the SDK's.
 *
 * The document's response headers are read over CDP, because the frame is
 * cross-origin and the page cannot see them. That is the assertion that the
 * widget really is served with its own policy instead of inheriting the app's.
 */
async function widget(cdp, { dataDir }) {
  const checks = [];
  const check = (name, ok, detail) => checks.push({ name, ok: Boolean(ok), detail });

  // The widget scheme is `http://widget.localhost` on Windows and Android and
  // `widget://localhost` on macOS and Linux. The app derives it from its own
  // origin, so the suite does too rather than pinning one platform's form.
  const widgetOrigin = await cdp.evaluate(`(() => {
    const origin = location.origin;
    return origin.startsWith('http://tauri.localhost') ||
      origin.startsWith('https://tauri.localhost')
      ? 'http://widget.localhost'
      : 'widget://localhost';
  })()`);

  const documents = [];
  cdp.on('Network.responseReceived', (event) => {
    const url = event.response?.url ?? '';
    if (url.startsWith(`${widgetOrigin}/`)) documents.push(event.response);
  });
  await cdp.send('Network.enable');

  const restoreActions = await installActions(dataDir, [
    {
      id: 'live-check-ping',
      label: 'Ping',
      icon: 'bolt',
      slot: 'rail',
      hint: 'Installed by scripts/live-check.mjs and removed again.',
      run: { kind: 'command', command: 'focus-composer' },
    },
  ]);

  const restorePanels = await installPanels(dataDir, [
    {
      id: 'live-check',
      title: 'Live Check',
      icon: 'folder',
      width: 380,
      sources: [{ id: 'clock', type: 'builtin', name: 'clock' }],
      // A layout tree, not a flat block list: a widget has to compose inside a
      // `tabs` or `split` node, because that is what a real custom panel is.
      layout: {
        node: 'tabs',
        tabs: [
          {
            label: 'Live',
            children: [
              {
                kind: 'widget',
                title: 'Data widget',
                height: 200,
                html: [
                  '<div id="out" style="font:11px ui-monospace">waiting</div>',
                  '<script>',
                  'vibedesk.onData(function (model) {',
                  '  var epoch = model.clock && model.clock.epoch;',
                  "  document.getElementById('out').textContent = 'epoch ' + epoch;",
                  '  vibedesk.resize(epoch ? 300 : 120);',
                  '});',
                  '</script>',
                ].join('\n'),
              },
              {
                kind: 'widget',
                title: 'Action widget',
                height: 200,
                capabilities: [{ kind: 'action', id: 'live-check-ping' }],
                html: [
                  '<button id="go">go</button>',
                  '<script>',
                  "vibedesk.action('live-check-ping')",
                  '  .then(function () { vibedesk.resize(260); })',
                  '  .catch(function () { vibedesk.resize(140); });',
                  '</script>',
                ].join('\n'),
              },
              {
                kind: 'widget',
                title: 'Refused widget',
                height: 200,
                html: [
                  '<script>',
                  "window.addEventListener('message', function (event) {",
                  '  var message = event.data;',
                  "  if (!message || message.type !== 'vibedesk:ack') return;",
                  '  vibedesk.resize(message.ok ? 300 : 180);',
                  '});',
                  // Bypasses `vibedesk.action`, which would refuse this locally.
                  // The parent has to refuse it too, or the SDK is the only
                  // boundary and the frame is not really contained.
                  "parent.postMessage({ type: 'vibedesk:invoke', id: 1, capability: 'action:not-declared', payload: {} }, '*');",
                  '</script>',
                ].join('\n'),
              },
            ],
          },
          { label: 'Other', children: [{ kind: 'text', text: 'nothing here' }] },
        ],
      },
    },
  ]);

  let restoreWidgetFile = null;

  try {
    await waitFor(
      cdp,
      `[...document.querySelectorAll('button')].some((b) => b.getAttribute('title') === 'Live Check')`,
      { label: 'the panel to appear in the rail' },
    );
    await openPanel(cdp, 'Live Check');

    // Every widget asks for its own height, so a frame that never resizes is a
    // frame whose script never ran.
    await waitFor(
      cdp,
      `(() => {
        const frames = [...document.querySelectorAll('iframe')];
        return frames.length === 3 && frames.every((f) => parseFloat(f.style.height) !== 200);
      })()`,
      { label: 'all three widgets to report a height', timeoutMs: 20000 },
    );

    const frames = await cdp.evaluate(`(() => {
      return [...document.querySelectorAll('iframe')].map((frame) => ({
        src: frame.getAttribute('src') ?? '',
        sandbox: frame.getAttribute('sandbox'),
        height: parseFloat(frame.style.height),
        title: frame.getAttribute('title'),
      }));
    })()`);

    check('three widget frames are mounted', frames.length === 3, JSON.stringify(frames));
    check(
      'every widget is served from its own origin',
      frames.every((frame) => frame.src.startsWith('http://widget.localhost/w/')),
      JSON.stringify(frames.map((frame) => frame.src)),
    );
    check(
      'every widget frame is sandboxed to scripts only',
      frames.every((frame) => frame.sandbox === 'allow-scripts'),
      JSON.stringify(frames.map((frame) => frame.sandbox)),
    );
    check(
      'a widget script runs and receives live data',
      frames[0]?.height === 300,
      `height ${frames[0]?.height}`,
    );
    check(
      'a declared capability is dispatched and acked',
      frames[1]?.height === 260,
      `height ${frames[1]?.height}`,
    );
    check(
      'an undeclared capability is refused by the app',
      frames[2]?.height === 180,
      `height ${frames[2]?.height}`,
    );

    // The action really ran: `focus-composer` puts the caret in the composer.
    const focused = await cdp.evaluate(
      `document.activeElement?.className?.includes('vd-input') ?? false`,
    );
    check('the declared action actually ran', focused);

    // The notice is added when the refusal arrives, which is a frame or two after
    // the widget reports its height: the two travel by different routes — the
    // refusal through the app's message handler, the height through the frame's
    // own resize. Reading the body once raced the notice, and under `all` (a panel
    // already mounted, more work in flight) it lost the race twice in five runs.
    // Poll for the sentence; when it never comes, report what the body did say.
    const refusal = 'which the panel does not declare';
    let refused = true;
    try {
      await waitFor(cdp, `document.body.innerText.includes(${JSON.stringify(refusal)})`, {
        label: 'the refusal notice',
        timeoutMs: 5000,
      });
    } catch {
      refused = false;
    }
    // A failure has to say which of two very different things happened: the app
    // never added the notice, or it added it somewhere the user cannot see.
    // `innerText` only carries what is rendered; `innerHTML` carries what is in
    // the tree. Disagreeing means the notice exists behind a closed surface.
    const where = refused
      ? ''
      : await cdp.evaluate(`(() => ({
          inText: document.body.innerText.includes(${JSON.stringify(refusal)}),
          inTree: document.documentElement.innerHTML.includes(${JSON.stringify(refusal)}),
          transcript: document.querySelectorAll('[class*=transcript], [class*=Transcript]').length,
          panels: document.querySelectorAll('[class*=panel]').length,
          tail: document.body.innerText.slice(-160),
        }))()`);
    check('the refusal is reported to the user', refused, JSON.stringify(where));

    // A widget lives inside the layout tree, so switching tabs has to unmount
    // its frame rather than leave a script running off screen.
    await clickByText(cdp, 'Other');
    await sleep(500);
    const hidden = await cdp.evaluate(`document.querySelectorAll('iframe').length`);
    check('switching tabs unmounts the widget frames', hidden === 0, `${hidden} frames`);

    await clickByText(cdp, 'Live');
    await waitFor(cdp, `document.querySelectorAll('iframe').length === 3`, {
      label: 'the widget frames to come back',
    });
    check('switching back remounts them', true);

    // The response headers are the crux: a widget document carries its own
    // policy, which is the only reason it can run scripts at all.
    const served = documents[0];
    const headers = served?.headers ?? {};
    const policy = headers['content-security-policy'] ?? headers['Content-Security-Policy'] ?? '';
    check('the widget document was served', Boolean(served), JSON.stringify(documents.length));
    // The frame has to point at the origin this platform actually serves. A
    // hardcoded Windows origin is a blank frame on macOS and Linux, and a blank
    // frame is indistinguishable from a broken widget.
    check(
      'the widget frame uses this platform\'s scheme',
      (frames[0]?.src ?? '').startsWith(`${widgetOrigin}/w/`),
      `${frames[0]?.src} vs ${widgetOrigin}`,
    );
    check(
      'the widget document carries its own policy',
      policy.includes("script-src 'unsafe-inline'") && policy.includes("connect-src 'none'"),
      policy,
    );
    check(
      'the widget document is not cached',
      (headers['cache-control'] ?? headers['Cache-Control'] ?? '').includes('no-store'),
      JSON.stringify(headers['cache-control']),
    );
    check(
      'the widget document is html',
      (headers['content-type'] ?? headers['Content-Type'] ?? '').includes('text/html'),
      JSON.stringify(headers['content-type']),
    );

    // A widget that is edited reloads: the URL is the content address, so a new
    // html is a new document rather than a stale frame.
    const before = frames[0]?.src;
    await installPanels(dataDir, [
      {
        id: 'live-check',
        title: 'Live Check',
        icon: 'folder',
        width: 380,
        sources: [{ id: 'clock', type: 'builtin', name: 'clock' }],
        blocks: [
          {
            kind: 'widget',
            title: 'Data widget',
            height: 200,
            html: '<script>vibedesk.resize(340);</script>',
          },
        ],
      },
    ]);
    await waitFor(
      cdp,
      `(() => {
        const frame = document.querySelector('iframe');
        return Boolean(frame) && parseFloat(frame.style.height) === 340;
      })()`,
      { label: 'the edited widget to reload', timeoutMs: 20000 },
    );
    const after = await cdp.evaluate(`document.querySelector('iframe')?.getAttribute('src') ?? ''`);
    check('editing a widget changes its address', after !== before, `${before} -> ${after}`);
    check(
      'the edited widget is the one that runs',
      (await cdp.evaluate(`parseFloat(document.querySelector('iframe').style.height)`)) === 340,
    );

    const closed = await cdp.evaluate(`(async () => {
      document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
      await new Promise((r) => setTimeout(r, 400));
      return document.querySelectorAll('iframe').length === 0;
    })()`);
    check('Escape closes the panel and unmounts the frames', closed);

    // ---- Phase 2: the file-backed widget, its own source, and the read budget.
    //
    // Three things the first phase cannot show, because they are about where a
    // widget's html and data come from rather than what it may do:
    //
    // 1. `src` — the html lives in `widgets/<name>.html` and is read into the
    //    block on load. The frame still runs, which is the proof the file was
    //    found and inlined rather than silently dropped.
    // 2. A widget-declared `sources` — the panel declares none, so a clock value
    //    in the model can only have come from the widget's own declaration.
    // 3. The `file` capability — a read succeeds, an absolute path is refused,
    //    and a read storm is stopped.
    restoreWidgetFile = await installWidgetFile(
      dataDir,
      'live-check.html',
      [
        '<div id="out" style="font:11px ui-monospace">waiting</div>',
        '<script>',
        'vibedesk.onData(function (model) {',
        '  var epoch = model.clock && model.clock.epoch;',
        "  document.getElementById('out').textContent = 'src epoch ' + epoch;",
        '  vibedesk.resize(epoch ? 320 : 120);',
        '});',
        '</script>',
      ].join('\n'),
    );

    // A real file in the workspace, so the successful read is a real read. The
    // workspace root is whatever folder the agent has open, so it is asked for
    // rather than assumed.
    const entries = await cdp.evaluate(
      `window.__TAURI_INTERNALS__.invoke('workspace_files', { query: '', limit: 40 })`,
    );
    // A text file, because `workspace_read` decodes UTF-8 and a binary would
    // fail for a reason that has nothing to do with the capability.
    const TEXT = /\.(md|txt|json|js|mjs|cjs|ts|tsx|jsx|css|html|yml|yaml|toml|rs|py|sh|cfg|ini)$/i;
    const files = (entries ?? []).filter((entry) => !entry.is_dir);
    const target = (files.find((entry) => TEXT.test(entry.name)) ?? files[0])?.path ?? null;

    const fileWidget = [
      '<script>',
      '(async function () {',
      '  var bits = 0;',
      '  try {',
      `    var text = await vibedesk.read(${JSON.stringify(target ?? 'package.json')});`,
      "    if (typeof text === 'string') bits |= 1;",
      '  } catch (error) { console.error(error); }',
      '  try {',
      "    await vibedesk.read('C:/Windows/win.ini');",
      '  } catch (error) {',
      "    if (String(error.message).indexOf('relative to the workspace') >= 0) bits |= 2;",
      '  }',
      '  var refused = 0;',
      '  for (var index = 0; index < 25; index += 1) {',
      '    try {',
      `      await vibedesk.read(${JSON.stringify(target ?? 'package.json')});`,
      '    } catch (error) {',
      "      if (String(error.message).indexOf('slow the polling down') >= 0) refused += 1;",
      '    }',
      '  }',
      '  if (refused > 0) bits |= 4;',
      '  vibedesk.resize(200 + bits);',
      '})();',
      '</script>',
    ].join('\n');

    // 300 resize messages in one task. Without coalescing the parent re-renders
    // 300 times; with it, once per frame. The burst is delayed so the observer
    // below is counting only this widget.
    const throttleWidget = [
      '<script>',
      'setTimeout(function () {',
      '  for (var index = 0; index < 300; index += 1) {',
      '    vibedesk.resize(200 + (index % 100));',
      '  }',
      '  vibedesk.resize(333);',
      '}, 3000);',
      '</script>',
    ].join('\n');

    await installPanels(dataDir, [
      {
        id: 'live-check',
        title: 'Live Check',
        icon: 'folder',
        width: 380,
        // No panel-level sources on purpose: the clock below is the widget's.
        layout: {
          node: 'stack',
          children: [
            {
              kind: 'widget',
              title: 'Src widget',
              src: 'widgets/live-check.html',
              height: 200,
              sources: [{ id: 'clock', type: 'builtin', name: 'clock' }],
            },
            {
              kind: 'widget',
              title: 'File widget',
              height: 200,
              capabilities: [{ kind: 'file' }],
              html: fileWidget,
            },
            { kind: 'widget', title: 'Throttle widget', height: 200, html: throttleWidget },
          ],
        },
      },
    ]);

    // The Escape check above closed the panel, so it has to be reopened before
    // the new widgets can mount.
    await openPanel(cdp, 'Live Check');

    await waitFor(
      cdp,
      `(() => {
        const frames = [...document.querySelectorAll('iframe')];
        return frames.length === 3 && parseFloat(frames[0].style.height) === 320;
      })()`,
      { label: 'the src-backed widget to run', timeoutMs: 20000 },
    );
    const srcFrame = await cdp.evaluate(`(() => {
      const frame = document.querySelectorAll('iframe')[0];
      return { src: frame.getAttribute('src') ?? '', height: parseFloat(frame.style.height) };
    })()`);
    check(
      'a widget may point at a file instead of carrying html',
      srcFrame.src.startsWith('http://widget.localhost/w/') && srcFrame.height === 320,
      JSON.stringify(srcFrame),
    );
    check(
      'a widget-declared source reaches the model',
      srcFrame.height === 320,
      `height ${srcFrame.height} — 120 means the clock never arrived`,
    );

    await waitFor(
      cdp,
      `(() => {
        const frame = document.querySelectorAll('iframe')[1];
        return Boolean(frame) && parseFloat(frame.style.height) !== 200;
      })()`,
      { label: 'the file widget to finish its reads', timeoutMs: 30000 },
    );
    const fileBits = await cdp.evaluate(
      `parseFloat(document.querySelectorAll('iframe')[1].style.height) - 200`,
    );
    check(
      'the file capability reads, refuses an escape, and stops a read storm',
      fileBits === 7,
      `bits ${fileBits} (1 read, 2 escape refused, 4 budget) target ${target}`,
    );

    // Count how many times the parent actually re-rendered the frame's height.
    await cdp.evaluate(`(() => {
      window.__vdResize = 0;
      window.__vdResizeObserver = new MutationObserver((records) => {
        for (const record of records) {
          if (record.target instanceof HTMLIFrameElement && record.attributeName === 'style') {
            window.__vdResize += 1;
          }
        }
      });
      window.__vdResizeObserver.observe(document.body, {
        subtree: true,
        attributes: true,
        attributeFilter: ['style'],
      });
      return true;
    })()`);

    await waitFor(
      cdp,
      `(() => {
        const frame = document.querySelectorAll('iframe')[2];
        return Boolean(frame) && parseFloat(frame.style.height) === 333;
      })()`,
      { label: 'the throttled widget to settle', timeoutMs: 30000 },
    );
    const renders = await cdp.evaluate(`(() => {
      window.__vdResizeObserver.disconnect();
      return window.__vdResize;
    })()`);
    check(
      'a resize storm is coalesced to one parent render per frame',
      renders > 0 && renders <= 40,
      `${renders} height changes for 300 messages`,
    );

    // Editing the widget file is a live edit. The config watcher stamps the
    // `widgets/` directory as well as the four config files, so a change to the
    // html reloads the panel and the frame's content address moves — no restart,
    // and panels.json is not touched. Written directly rather than through
    // `installWidgetFile`, whose restore closure would then capture the edit as
    // the thing to put back.
    const beforeSrc = srcFrame.src;
    await writeFile(
      join(dataDir, 'config', 'widgets', 'live-check.html'),
      '<script>vibedesk.resize(410);</script>',
    );
    await waitFor(
      cdp,
      `(() => {
        const frame = document.querySelectorAll('iframe')[0];
        return Boolean(frame) && parseFloat(frame.style.height) === 410;
      })()`,
      { label: 'the edited widget file to reload', timeoutMs: 20000 },
    );
    const afterSrc = await cdp.evaluate(
      `document.querySelectorAll('iframe')[0].getAttribute('src')`,
    );
    check(
      'editing a widget file reloads the panel without a restart',
      afterSrc !== beforeSrc,
      `${beforeSrc} -> ${afterSrc}`,
    );
  } finally {
    if (restoreWidgetFile) await restoreWidgetFile();
    await restorePanels();
    await restoreActions();
  }

  return checks;
}

/**
 * The `chart` block.
 *
 * The block this replaced drew a 38px strip over a flat array: no scale, no
 * ticks, no labels, one series, no way to read a value off it. `AGENTS.md` names
 * that as the shallow surface it was, so this suite is the evidence the
 * replacement is a plot rather than a wider strip. It pins the things a chart
 * has and a sparkline does not:
 *
 * 1. **A y-axis.** Whole-number ticks with gridlines, and an explicit `yMax`
 *    that pins the top of the scale instead of fitting the data.
 * 2. **An x-axis.** Category labels, and epoch milliseconds read as times.
 * 3. **More than one series.** Two named series, a legend, and a hover readout
 *    that names the point under the cursor.
 * 4. **The strip is still available, and still says so.** `sparkline` draws no
 *    axes, which is the honest name for what it is.
 */
async function chart(cdp, { dataDir }) {
  const checks = [];
  const check = (name, ok, detail) => checks.push({ name, ok: Boolean(ok), detail });

  const restorePanels = await installPanels(dataDir, [
    {
      id: 'live-check-chart',
      title: 'Chart Check',
      icon: 'folder',
      width: 420,
      blocks: [
        {
          kind: 'chart',
          label: 'Queue',
          style: 'bar',
          series: [
            { name: 'waiting', values: [4, 9, 6, 12] },
            { name: 'running', values: [2, 3, 5, 4] },
          ],
          x: ['alpha', 'bravo', 'charlie', 'delta'],
          yMin: 0,
          yMax: 20,
          unit: ' jobs',
          height: 180,
        },
        {
          kind: 'chart',
          label: 'Latency',
          style: 'line',
          values: [120, 180, 90, 240],
          x: [1700000000000, 1700000060000, 1700000120000, 1700000180000],
          time: true,
          height: 160,
        },
        { kind: 'chart', label: 'Strip', style: 'sparkline', values: [1, 4, 2, 8, 5] },
        // Stacked, with a threshold. The second series sits on the first, so
        // its rect starts where the first one ends.
        {
          kind: 'chart',
          label: 'Stacked',
          style: 'bar',
          stack: 'stacked',
          series: [
            { name: 'base', values: [4, 6, 2, 8] },
            { name: 'extra', values: [2, 4, 6, 2] },
          ],
          reference: [{ value: 9, label: 'cap', tone: 'warn' }],
          height: 160,
        },
        // Percent stacking normalises each column to 100, so the axis tops out
        // at 100 whatever the values are.
        {
          kind: 'chart',
          label: 'Share',
          style: 'area',
          stack: 'percent',
          series: [
            { name: 'read', values: [1, 3] },
            { name: 'write', values: [3, 1] },
          ],
          height: 160,
        },
        // Four decades. A linear axis would flatten the first three into the
        // baseline, which is the whole reason a log scale exists.
        {
          kind: 'chart',
          label: 'Decades',
          style: 'line',
          scale: 'log',
          values: [1, 10, 100, 1000],
          height: 160,
        },
      ],
    },
  ]);

  try {
    await waitFor(
      cdp,
      `[...document.querySelectorAll('button')].some((b) => b.getAttribute('title') === 'Chart Check')`,
      { label: 'the chart panel to appear in the rail' },
    );
    await openPanel(cdp, 'Chart Check');
    await waitFor(cdp, `document.querySelectorAll('svg[data-vd-chart]').length === 6`, {
      label: 'six charts to render',
    });

    const shapes = await cdp.evaluate(`(() => {
      return [...document.querySelectorAll('svg[data-vd-chart]')].map((svg) => {
        const texts = [...svg.querySelectorAll('text')];
        const lines = [...svg.querySelectorAll('line')];
        return {
          style: svg.getAttribute('data-vd-chart'),
          label: svg.getAttribute('aria-label'),
          width: svg.getAttribute('width'),
          // A y label sits left of the plot area; an x label sits below it.
          yLabels: texts.filter((t) => Number(t.getAttribute('x')) < 38).map((t) => t.textContent),
          xLabels: texts.filter((t) => Number(t.getAttribute('x')) >= 38).map((t) => t.textContent),
          gridlines: lines.filter((l) => l.getAttribute('stroke') === 'var(--vd-line-soft)')
            .length,
          bars: svg.querySelectorAll('rect').length,
          polylines: svg.querySelectorAll('polyline').length,
        };
      });
    })()`);

    const bar = shapes[0];
    const line = shapes[1];
    const spark = shapes[2];

    check('a chart draws an svg with an accessible name', Boolean(bar?.label), bar?.label);
    check(
      'a chart names every series in its accessible name',
      (bar?.label ?? '').includes('waiting') && (bar?.label ?? '').includes('running'),
      bar?.label,
    );
    check(
      'a chart draws a y-axis with ticks',
      (bar?.yLabels.length ?? 0) >= 2,
      JSON.stringify(bar?.yLabels),
    );
    check('a chart draws gridlines', (bar?.gridlines ?? 0) >= 2, `${bar?.gridlines} gridlines`);
    check(
      'an explicit yMax pins the top of the scale',
      (bar?.yLabels ?? []).includes('20'),
      JSON.stringify(bar?.yLabels),
    );
    check(
      'a chart draws x labels',
      (bar?.xLabels ?? []).includes('alpha') && (bar?.xLabels ?? []).includes('delta'),
      JSON.stringify(bar?.xLabels),
    );
    check('a bar chart draws one rect per point per series', bar?.bars === 8, `${bar?.bars} rects`);
    check(
      'a line chart draws one polyline per series',
      line?.polylines === 1,
      `${line?.polylines} polylines`,
    );
    check(
      'epoch milliseconds are drawn as times',
      (line?.xLabels ?? []).some((text) => /^\d{2}:\d{2}$/.test(text)),
      JSON.stringify(line?.xLabels),
    );
    check(
      'a sparkline draws no axes',
      (spark?.yLabels.length ?? 0) === 0 && (spark?.gridlines ?? 0) === 0,
      JSON.stringify({ y: spark?.yLabels, grid: spark?.gridlines }),
    );

    const legend = await cdp.evaluate(
      `document.body.innerText.includes('waiting') && document.body.innerText.includes('running')`,
    );
    check('a multi-series chart carries a legend', legend);

    // The hover readout is the difference between a picture and a chart: it is
    // how a value is read off the plot without guessing at the pixel.
    const tooltip = await cdp.evaluate(`(async () => {
      const svg = document.querySelector('svg[data-vd-chart="bar"]');
      const rect = svg.getBoundingClientRect();
      // The third of four bands, so the label under the cursor is unambiguous.
      const x = rect.left + 38 + ((rect.width - 46) / 4) * 2.5;
      svg.dispatchEvent(
        new PointerEvent('pointermove', { clientX: x, clientY: rect.top + 40, bubbles: true }),
      );
      await new Promise((resolve) => setTimeout(resolve, 120));
      const box = document.querySelector('[data-vd-chart-tooltip]');
      return box ? box.textContent : null;
    })()`);
    check(
      'hovering a chart reads the point out',
      Boolean(tooltip) && tooltip.includes('charlie') && tooltip.includes('6'),
      tooltip,
    );

    // Moving off the plot area clears it. `pointerleave` is not used here: it
    // does not bubble, and React emulates it from `pointerout`, so a synthetic
    // one would test the harness rather than the app. The handler clears on any
    // pointer position outside the plot, which is the same code path.
    const cleared = await cdp.evaluate(`(async () => {
      const svg = document.querySelector('svg[data-vd-chart="bar"]');
      const rect = svg.getBoundingClientRect();
      svg.dispatchEvent(
        new PointerEvent('pointermove', { clientX: rect.left + 2, clientY: rect.top + 40, bubbles: true }),
      );
      await new Promise((resolve) => setTimeout(resolve, 120));
      return document.querySelector('[data-vd-chart-tooltip]') === null;
    })()`);
    check('moving off the plot clears the readout', cleared);

    // The crosshair is the difference between a tooltip and a readout: a dot on
    // the series says which point the number belongs to.
    const dots = await cdp.evaluate(`(async () => {
      const svg = document.querySelector('svg[data-vd-chart="bar"]');
      const rect = svg.getBoundingClientRect();
      const x = rect.left + 38 + ((rect.width - 46) / 4) * 2.5;
      svg.dispatchEvent(
        new PointerEvent('pointermove', { clientX: x, clientY: rect.top + 40, bubbles: true }),
      );
      await new Promise((resolve) => setTimeout(resolve, 120));
      return [...svg.querySelectorAll('[data-vd-chart-dot]')].map((c) => ({
        series: c.getAttribute('data-vd-chart-dot'),
        cx: Number(c.getAttribute('cx')),
        cy: Number(c.getAttribute('cy')),
      }));
    })()`);
    check(
      'the crosshair marks every visible series',
      dots.length === 2 && dots.every((dot) => Number.isFinite(dot.cy)),
      JSON.stringify(dots),
    );

    // Stacking. The second series' band starts where the first one ends, so its
    // rect is above the first one's and its height is its own value, not the
    // total.
    const stacked = await cdp.evaluate(`(() => {
      const svg = document.querySelector('svg[data-vd-chart="bar"][data-vd-chart-stack="stacked"]');
      if (!svg) return null;
      const read = (key) => {
        const rect = svg.querySelector('[data-vd-chart-bar="' + key + '"]');
        if (!rect) return null;
        return {
          y: Number(rect.getAttribute('y')),
          h: Number(rect.getAttribute('height')),
        };
      };
      return {
        base: read('0-0'),
        extra: read('1-0'),
        references: [...svg.querySelectorAll('[data-vd-chart-reference]')].map((g) => ({
          stroke: g.querySelector('line').getAttribute('stroke'),
          label: g.querySelector('text')?.textContent ?? null,
        })),
        label: svg.getAttribute('aria-label'),
      };
    })()`);
    check(
      'a stacked series sits on the one below it',
      stacked !== null &&
        stacked.base !== null &&
        stacked.extra !== null &&
        Math.abs(stacked.extra.y + stacked.extra.h - stacked.base.y) < 1.5,
      JSON.stringify(stacked),
    );
    check(
      'a stacked chart says so in its accessible name',
      (stacked?.label ?? '').includes('stacked stack'),
      stacked?.label,
    );
    check(
      'a reference line is drawn with its label',
      stacked?.references.length === 1 && stacked.references[0].label === 'cap',
      JSON.stringify(stacked?.references),
    );
    check(
      'a reference line takes its tone',
      stacked?.references[0]?.stroke === 'var(--vd-warn)',
      stacked?.references[0]?.stroke,
    );

    // Percent stacking normalises each column, so the axis is 0..100 whatever
    // the values are.
    const percent = await cdp.evaluate(`(() => {
      const svg = document.querySelector('svg[data-vd-chart="area"][data-vd-chart-stack="percent"]');
      if (!svg) return null;
      return {
        yLabels: [...svg.querySelectorAll('text')]
          .filter((t) => Number(t.getAttribute('x')) < 38)
          .map((t) => t.textContent),
        label: svg.getAttribute('aria-label'),
      };
    })()`);
    check(
      'a percent stack tops out at 100',
      (percent?.yLabels ?? []).includes('100'),
      JSON.stringify(percent?.yLabels),
    );
    check(
      'a percent stack says so in its accessible name',
      (percent?.label ?? '').includes('percent stack'),
      percent?.label,
    );

    // A log axis draws decades. On a linear axis 1, 10, 100 and 1000 would put
    // three of the four points on the baseline.
    const logAxis = await cdp.evaluate(`(() => {
      const svg = document.querySelector('svg[data-vd-chart="line"][data-vd-chart-scale="log"]');
      if (!svg) return null;
      return {
        yLabels: [...svg.querySelectorAll('text')]
          .filter((t) => Number(t.getAttribute('x')) < 38)
          .map((t) => t.textContent),
        label: svg.getAttribute('aria-label'),
      };
    })()`);
    check(
      'a log axis draws decades',
      ['1', '10', '100', '1.0k'].every((tick) => (logAxis?.yLabels ?? []).includes(tick)),
      JSON.stringify(logAxis?.yLabels),
    );
    check(
      'a log chart says so in its accessible name',
      (logAxis?.label ?? '').includes('log scale'),
      logAxis?.label,
    );

    // The legend filters. Hiding a series removes its bars and rescales the
    // axis to what is left, which is how one series is read out of four.
    const filtered = await cdp.evaluate(`(async () => {
      const svg = document.querySelector('svg[data-vd-chart="bar"][data-vd-chart-stack="none"]');
      const before = svg.querySelectorAll('rect').length;
      const button = document.querySelector('[data-vd-chart-legend="0"]');
      if (!button) return { before, after: null };
      button.click();
      await new Promise((resolve) => setTimeout(resolve, 200));
      const after = document.querySelector('svg[data-vd-chart="bar"][data-vd-chart-stack="none"]')
        .querySelectorAll('rect').length;
      const pressed = document.querySelector('[data-vd-chart-legend="0"]').getAttribute('aria-pressed');
      return { before, after, pressed };
    })()`);
    check(
      'clicking a legend entry hides that series',
      filtered.before === 8 && filtered.after === 4,
      JSON.stringify(filtered),
    );
    check(
      'a hidden series is marked as unpressed',
      filtered.pressed === 'false',
      String(filtered.pressed),
    );

    const restored = await cdp.evaluate(`(async () => {
      document.querySelector('[data-vd-chart-legend="0"]').click();
      await new Promise((resolve) => setTimeout(resolve, 200));
      return document.querySelector('svg[data-vd-chart="bar"][data-vd-chart-stack="none"]')
        .querySelectorAll('rect').length;
    })()`);
    check('clicking it again brings the series back', restored === 8, String(restored));
  } finally {
    await restorePanels();
  }

  return checks;
}

/**
 * The `diagram` block.
 *
 * A chart plots a series and a widget is a blank canvas. A diagram is the third
 * thing: a graph described in JSON, laid out by an algorithm, with every visible
 * field bound to live data and every node able to fire an action. This suite is
 * the evidence that it is a graph and not a picture of one:
 *
 * 1. **Four layouts, four different pictures.** The same nodes and edges are
 *    drawn by `layered`, `radial`, `tree` and `sequence`, and the four produce
 *    four distinct sets of coordinates. A layout that fell back to a grid would
 *    collapse two of them into one.
 * 2. **Every visible field is an expression.** A node's `tone` is an `@if` over
 *    a live clock path, and the two branches are checked separately: one node
 *    resolves to `danger`, another to `ok`. An edge's label is a `@format` over
 *    the same source, so it reads as a number rather than as a literal.
 * 3. **`when` removes a node and its edges.** A hidden node is not drawn, and
 *    neither is the edge that pointed at it — a dangling arrow would be a lie.
 * 4. **Interaction reaches the agent.** Hovering a node lights its edges and
 *    dims the rest; clicking an actionable node fires the action from
 *    `actions.json`, which here focuses the composer.
 * 5. **It is readable without the picture.** Every edge is also a line of text
 *    in a hidden list, because a `<title>` on a non-focusable group is not
 *    announced.
 */
async function diagram(cdp, { dataDir }) {
  const checks = [];
  const check = (name, ok, detail) => checks.push({ name, ok: Boolean(ok), detail });

  const restoreActions = await installActions(dataDir, [
    {
      id: 'live-check-ping',
      label: 'Ping',
      icon: 'bolt',
      slot: 'rail',
      hint: 'Installed by scripts/live-check.mjs and removed again.',
      run: { kind: 'command', command: 'focus-composer' },
    },
  ]);

  // `ghost` is guarded by a condition that is never true, so it and the edge
  // that points at it must both disappear. `api` and `cache` take opposite
  // branches of the same `@if`, which is what proves the operator evaluates
  // rather than parses.
  const nodes = [
    { id: 'client', label: 'Client', detail: 'browser', tone: 'accent' },
    { id: 'edge', label: 'Edge', detail: { '@path': '/clock/minute' } },
    {
      id: 'api',
      label: 'API',
      tone: { '@if': [{ '@gt': [{ '@path': '/clock/epoch' }, 0] }, 'danger', 'ok'] },
      action: 'live-check-ping',
    },
    {
      id: 'cache',
      label: 'Cache',
      tone: { '@if': [{ '@gt': [{ '@path': '/clock/epoch' }, 99999999999] }, 'danger', 'ok'] },
    },
    { id: 'ghost', label: 'Ghost', when: { '@gt': [{ '@path': '/clock/epoch' }, 99999999999] } },
  ];
  const edges = [
    { id: 'e1', from: 'client', to: 'edge', label: 'GET /' },
    {
      id: 'e2',
      from: 'edge',
      to: 'api',
      label: { '@format': 'number', '@of': { '@path': '/clock/minute' } },
    },
    { id: 'e3', from: 'api', to: 'ghost', label: 'never' },
  ];

  // Five participants with long labels in a 460 px panel: at their natural
  // width they need about 1080 px, so this is the case that used to scroll
  // horizontally and hide the last two participants off the right edge.
  const wide = ['Coordinator', 'Auth Service', 'Ledger', 'Notifier', 'Archive'];
  const wideNodes = wide.map((label, index) => ({
    id: `w${index}`,
    label,
    detail: 'service',
  }));
  const wideEdges = wide.slice(1).map((_, index) => ({
    id: `we${index}`,
    from: `w${index}`,
    to: `w${index + 1}`,
    label: 'call',
  }));

  const restorePanels = await installPanels(dataDir, [
    {
      id: 'live-check-diagram-fit',
      title: 'Diagram Fit',
      icon: 'list',
      width: 460,
      sources: [{ id: 'clock', type: 'builtin', name: 'clock' }],
      blocks: [
        {
          kind: 'diagram',
          label: 'Wide sequence',
          layout: 'sequence',
          nodes: wideNodes,
          edges: wideEdges,
          height: 220,
        },
      ],
    },
    {
      id: 'live-check-diagram',
      title: 'Diagram Check',
      icon: 'folder',
      width: 460,
      sources: [{ id: 'clock', type: 'builtin', name: 'clock' }],
      blocks: [
        { kind: 'diagram', label: 'Layered', layout: 'layered', nodes, edges, height: 200 },
        // Radial is the layout `curved` is for: the ring's interior is empty,
        // so a bow reads better than a chord and still crosses nothing.
        {
          kind: 'diagram',
          label: 'Radial',
          layout: 'radial',
          routing: 'curved',
          nodes,
          edges,
          height: 260,
        },
        { kind: 'diagram', label: 'Tree', layout: 'tree', nodes, edges, height: 200 },
        { kind: 'diagram', label: 'Sequence', layout: 'sequence', nodes, edges, height: 200 },
      ],
    },
  ]);

  try {
    await waitFor(
      cdp,
      `[...document.querySelectorAll('button')].some((b) => b.getAttribute('title') === 'Diagram Check')`,
      { label: 'the diagram panel to appear in the rail' },
    );
    await openPanel(cdp, 'Diagram Check');
    // Wait for a node this suite declared, not just for four svgs: a panel left
    // open by an earlier run has four diagrams too, and asserting against that
    // one would test the wrong graph.
    await waitFor(
      cdp,
      `document.querySelectorAll('svg[data-vd-diagram]').length === 4 &&
       document.querySelector('[data-vd-node="client"]') !== null`,
      { label: 'four diagrams to render' },
    );
    // Nothing is coloured until the clock reaches the panel, so this is also the
    // proof that the block is bound to a live source rather than to literals.
    await waitFor(cdp, `document.querySelector('[data-vd-tone="danger"]') !== null`, {
      label: 'the clock to reach the diagram',
      timeoutMs: 20000,
    });

    const shapes = await cdp.evaluate(`(() => {
      return [...document.querySelectorAll('svg[data-vd-diagram]')].map((svg) => {
        const nodes = [...svg.querySelectorAll('[data-vd-node]')];
        const edges = [...svg.querySelectorAll('[data-vd-edge]')];
        return {
          layout: svg.getAttribute('data-vd-diagram'),
          routing: svg.getAttribute('data-vd-diagram-routing'),
          label: svg.getAttribute('aria-label'),
          nodes: nodes.map((g) => {
            const rect = g.querySelector('rect');
            return {
              id: g.getAttribute('data-vd-node'),
              tone: g.getAttribute('data-vd-tone'),
              role: g.getAttribute('role'),
              tab: g.getAttribute('tabindex'),
              x: Number(rect.getAttribute('x')),
              y: Number(rect.getAttribute('y')),
              w: Number(rect.getAttribute('width')),
              h: Number(rect.getAttribute('height')),
            };
          }),
          edges: edges.map((g) => {
            const text = g.querySelector('text');
            const path = g.querySelector('path').getAttribute('d');
            return {
              id: g.getAttribute('data-vd-edge'),
              stroke: g.querySelector('path').getAttribute('stroke'),
              opacity: g.style.opacity,
              label: text ? text.textContent : null,
              curved: path.includes(' C '),
            };
          }),
          arrows: svg.querySelectorAll('polygon').length,
        };
      });
    })()`);

    const byLayout = Object.fromEntries(shapes.map((shape) => [shape.layout, shape]));
    const node = (layout, id) => byLayout[layout]?.nodes.find((one) => one.id === id);
    const edge = (layout, id) => byLayout[layout]?.edges.find((one) => one.id === id);

    check(
      'four diagrams render, one per layout',
      shapes.length === 4 &&
        ['layered', 'radial', 'tree', 'sequence'].every((name) => Boolean(byLayout[name])),
      shapes.map((shape) => shape.layout).join(', '),
    );
    check(
      'a diagram names itself and its size',
      /4 nodes, 2 edges/.test(byLayout.layered?.label ?? ''),
      byLayout.layered?.label,
    );
    check(
      'every declared node is drawn',
      shapes.every((shape) => shape.nodes.length === 4),
      shapes.map((shape) => shape.nodes.length).join(', '),
    );
    check(
      'every declared edge is drawn',
      shapes.every((shape) => shape.edges.length === 2),
      shapes.map((shape) => shape.edges.length).join(', '),
    );
    check(
      'a node whose when is false is not drawn',
      !shapes.some((shape) => shape.nodes.some((one) => one.id === 'ghost')),
    );
    check(
      'an edge to a hidden node is not drawn',
      !shapes.some((shape) => shape.edges.some((one) => one.id === 'e3')),
    );
    check(
      'a tone expression takes the true branch',
      node('layered', 'api')?.tone === 'danger',
      node('layered', 'api')?.tone,
    );
    check(
      'a tone expression takes the false branch',
      node('layered', 'cache')?.tone === 'ok',
      node('layered', 'cache')?.tone,
    );
    check(
      'a literal tone is honoured',
      node('layered', 'client')?.tone === 'accent',
      node('layered', 'client')?.tone,
    );
    check(
      'an edge label is bound to live data',
      /^\d+$/.test(edge('layered', 'e2')?.label ?? ''),
      edge('layered', 'e2')?.label,
    );
    check(
      'curved routing draws a bezier',
      shapes
        .filter((shape) => shape.routing === 'curved')
        .every((shape) => shape.edges.every((one) => one.curved)),
      JSON.stringify(shapes.map((shape) => [shape.layout, shape.routing])),
    );
    check(
      'orthogonal routing draws elbows',
      shapes
        .filter((shape) => shape.routing === 'orthogonal')
        .every((shape) => shape.edges.every((one) => !one.curved)),
      JSON.stringify(shapes.map((shape) => [shape.layout, shape.routing])),
    );
    check(
      'every edge ends in an arrowhead',
      shapes.every((shape) => shape.arrows === 2),
      shapes.map((shape) => shape.arrows).join(', '),
    );
    check(
      'the four layouts place nodes differently',
      new Set(shapes.map((shape) => JSON.stringify(shape.nodes.map((one) => [one.x, one.y]))))
        .size === 4,
      shapes.map((shape) => shape.layout).join(', '),
    );
    // Two boxes on top of each other is the failure mode a layout algorithm
    // has, and it is invisible in a screenshot of a graph that happens to be
    // sparse. Every pair, every layout.
    const overlaps = shapes.flatMap((shape) => {
      const found = [];
      for (let a = 0; a < shape.nodes.length; a++) {
        for (let b = a + 1; b < shape.nodes.length; b++) {
          const one = shape.nodes[a];
          const two = shape.nodes[b];
          if (
            one.x < two.x + two.w &&
            two.x < one.x + one.w &&
            one.y < two.y + two.h &&
            two.y < one.y + one.h
          ) {
            found.push(`${shape.layout}:${one.id}/${two.id}`);
          }
        }
      }
      return found;
    });
    check('no two nodes overlap in any layout', overlaps.length === 0, overlaps.join(', '));

    // A tidy tree centres a parent over its children. This is the assertion that
    // catches a subtree shift that moves the wrong array: the parent lands one
    // pixel off, and every wider tree lands a whole box off.
    const tree = byLayout.tree;
    const centreOf = (id) => {
      const found = tree.nodes.find((node) => node.id === id);
      return found ? found.x + found.w / 2 : null;
    };
    const skew = ['client->edge', 'edge->api']
      .map((key) => {
        const [from, to] = key.split('->');
        const a = centreOf(from);
        const b = centreOf(to);
        return a === null || b === null ? 0 : Math.abs(a - b);
      })
      .filter((delta) => delta > 1);
    check('a tree centres a parent over its child', skew.length === 0, JSON.stringify(skew));

    check(
      'an actionable node is a keyboard-reachable button',
      node('layered', 'api')?.role === 'button' && node('layered', 'api')?.tab === '0',
      JSON.stringify({ role: node('layered', 'api')?.role, tab: node('layered', 'api')?.tab }),
    );
    check(
      'a node with no action is not a button',
      node('layered', 'client')?.role === 'img' && node('layered', 'client')?.tab === null,
      JSON.stringify({
        role: node('layered', 'client')?.role,
        tab: node('layered', 'client')?.tab,
      }),
    );

    // Hover is the difference between a picture and a graph: it is how a reader
    // follows one node's connections without tracing every line.
    const hovered = await cdp.evaluate(`(async () => {
      const svg = document.querySelector('svg[data-vd-diagram="layered"]');
      const target = svg.querySelector('[data-vd-node="client"]');
      target.dispatchEvent(
        new PointerEvent('pointerover', { bubbles: true, relatedTarget: null }),
      );
      await new Promise((resolve) => setTimeout(resolve, 150));
      return {
        edges: [...svg.querySelectorAll('[data-vd-edge]')].map((g) => ({
          id: g.getAttribute('data-vd-edge'),
          stroke: g.querySelector('path').getAttribute('stroke'),
          opacity: g.style.opacity,
        })),
        nodes: [...svg.querySelectorAll('[data-vd-node]')].map((g) => ({
          id: g.getAttribute('data-vd-node'),
          opacity: g.style.opacity,
        })),
      };
    })()`);

    const lit = hovered.edges.find((one) => one.id === 'e1');
    const dark = hovered.edges.find((one) => one.id === 'e2');
    check(
      'hovering a node lights its edge',
      lit?.stroke === 'var(--vd-accent)',
      JSON.stringify(lit),
    );
    check(
      'hovering a node dims the edges it is not on',
      dark?.opacity === '0.18',
      JSON.stringify(dark),
    );
    check(
      'hovering a node dims the nodes it does not touch',
      hovered.nodes.find((one) => one.id === 'cache')?.opacity === '0.4' &&
        hovered.nodes.find((one) => one.id === 'client')?.opacity === '1',
      JSON.stringify(hovered.nodes),
    );

    const cleared = await cdp.evaluate(`(async () => {
      const svg = document.querySelector('svg[data-vd-diagram="layered"]');
      const target = svg.querySelector('[data-vd-node="client"]');
      target.dispatchEvent(
        new PointerEvent('pointerout', { bubbles: true, relatedTarget: null }),
      );
      await new Promise((resolve) => setTimeout(resolve, 150));
      return [...svg.querySelectorAll('[data-vd-edge]')].every((g) => g.style.opacity === '1');
    })()`);
    check('moving off a node clears the highlight', cleared);

    // The click is the whole point of an action on a node: a diagram that only
    // draws is a screenshot.
    const fired = await cdp.evaluate(`(async () => {
      const svg = document.querySelector('svg[data-vd-diagram="layered"]');
      const target = svg.querySelector('[data-vd-node="api"]');
      target.dispatchEvent(new MouseEvent('click', { bubbles: true }));
      await new Promise((resolve) => setTimeout(resolve, 250));
      const active = document.activeElement;
      return active ? active.className : null;
    })()`);
    check(
      'clicking a node fires its action',
      typeof fired === 'string' && fired.includes('vd-input'),
      fired,
    );

    const listed = await cdp.evaluate(`(() => {
      return [...document.querySelectorAll('ul[aria-label$="connections"]')].map((ul) =>
        [...ul.querySelectorAll('li')].map((li) => li.textContent),
      );
    })()`);
    check(
      'the connections are readable as text',
      listed.length === 4 &&
        listed.every((items) => items.length === 2 && items.every((text) => text.includes(' to '))),
      JSON.stringify(listed[0]),
    );

    // A sequence diagram is the one layout whose width is decided by the panel:
    // participants are columns, and a column past the right edge is a
    // participant the reader cannot see. Five long labels in a 460 px panel is
    // the case that used to scroll.
    await openPanel(cdp, 'Diagram Fit');
    await waitFor(
      cdp,
      `document.querySelector('svg[data-vd-diagram="sequence"][aria-label^="Wide sequence"]') !== null`,
      { label: 'the wide sequence diagram to render' },
    );

    const fit = await cdp.evaluate(`(() => {
      const svg = document.querySelector('svg[data-vd-diagram="sequence"][aria-label^="Wide sequence"]');
      const frame = svg.parentElement;
      const nodes = [...svg.querySelectorAll('[data-vd-node]')].map((g) => {
        const rect = g.querySelector('rect');
        const text = g.querySelector('text');
        return {
          id: g.getAttribute('data-vd-node'),
          x: Number(rect.getAttribute('x')),
          w: Number(rect.getAttribute('width')),
          label: text ? text.textContent : null,
        };
      });
      return {
        svgWidth: Number(svg.getAttribute('width')),
        available: frame.clientWidth,
        scrollWidth: frame.scrollWidth,
        nodes,
      };
    })()`);

    check(
      'a sequence diagram fits its panel',
      fit.svgWidth <= fit.available && fit.scrollWidth <= fit.available + 1,
      JSON.stringify({ svg: fit.svgWidth, available: fit.available, scroll: fit.scrollWidth }),
    );
    check(
      'a sequence label is truncated rather than clipped',
      fit.nodes.some((one) => (one.label ?? '').endsWith('…')),
      JSON.stringify(fit.nodes.map((one) => one.label)),
    );
    check(
      'every participant is inside the panel',
      fit.nodes.every((one) => one.x >= 0 && one.x + one.w <= fit.available),
      JSON.stringify(fit.nodes.map((one) => [one.x, one.w])),
    );
    // The gap absorbs the leftover, so the lifelines span the panel instead of
    // bunching at the left. Two participants at opposite edges of a wide panel
    // would be the other failure, which is why the gap is capped.
    const gaps = fit.nodes.slice(1).map((one, index) => one.x - (fit.nodes[index].x + fit.nodes[index].w));
    check(
      'the participants spread across the panel',
      gaps.length > 0 && gaps.every((gap) => gap >= 9 && gap <= 161),
      JSON.stringify(gaps),
    );
  } finally {
    await restorePanels();
    await restoreActions();
  }

  return checks;
}

/**
 * The actions that reach the agent.
 *
 * `prompt` sends text to a running agent. That was the whole of "control the
 * agent" until `approve`, `reject` and `answer` existed: the agent has two
 * states where it is *stopped and waiting* — a permission request and an
 * elicitation — and neither had a button. This suite pins the three things that
 * can be checked without a model in the loop:
 *
 * 1. The vocabulary is published, so the agent can discover the kinds instead
 *    of guessing at them.
 * 2. The editor offers every kind, so a user can build one.
 * 3. Clicking one with nothing pending says so and does not throw. That is the
 *    guard, and it is the difference between a button and a crash.
 *
 * The agent round trip itself is not exercised here: it needs a live model to
 * decide to call a tool, and a check that only sometimes runs is a check nobody
 * trusts. `approve` calls the same `respond_permission` the approval bar calls,
 * so the path is the one already in daily use.
 */
async function agentActions(cdp, { dataDir }) {
  const checks = [];
  const check = (name, ok, detail) => checks.push({ name, ok: Boolean(ok), detail });

  const manifestPath = join(dataDir, 'config', 'vibedesk.manifest.json');
  let manifest = null;
  try {
    manifest = JSON.parse(await readFile(manifestPath, 'utf8'));
  } catch {
    manifest = null;
  }
  const surface = manifest?.surfaces?.actions ?? null;

  check('the manifest publishes the action surface', surface !== null);
  check(
    'the manifest lists every run kind',
    ['prompt', 'command', 'open-url', 'reveal', 'mode', 'option', 'cancel', 'approve', 'reject', 'answer'].every(
      (kind) => (surface?.runKinds ?? []).includes(kind),
    ),
    JSON.stringify(surface?.runKinds),
  );
  check(
    'the manifest lists the permission scopes',
    (surface?.permissionScopes ?? []).join(',') === 'once,always',
    JSON.stringify(surface?.permissionScopes),
  );
  check(
    'the manifest lists the elicitation answers',
    (surface?.elicitationAnswers ?? []).join(',') === 'accept,decline,cancel',
    JSON.stringify(surface?.elicitationAnswers),
  );

  // Two actions that reach a paused agent, and one that reaches the app, so the
  // strip has something to click in each case.
  const restoreActions = await installActions(dataDir, [
    {
      id: 'live-check-approve',
      label: 'Approve',
      icon: 'check',
      slot: 'composer',
      hint: 'Installed by scripts/live-check.mjs and removed again.',
      run: { kind: 'approve', scope: 'once' },
    },
    {
      id: 'live-check-answer',
      label: 'Decline',
      icon: 'info',
      slot: 'composer',
      hint: 'Installed by scripts/live-check.mjs and removed again.',
      run: { kind: 'answer', action: 'decline' },
    },
  ]);

  try {
    await waitFor(
      cdp,
      `[...document.querySelectorAll('button')].some((b) => b.textContent.trim() === 'Approve')`,
      { label: 'the approve action to appear in the composer strip' },
    );

    // The editor has to offer the kinds, or the vocabulary is unreachable.
    await openSection(cdp, 'Customize');
    await sleep(400);
    const kinds = await cdp.evaluate(`(() => {
      const select = [...document.querySelectorAll('select')]
        .find((s) => [...s.options].some((o) => o.value === 'approve'));
      return select ? [...select.options].map((o) => o.value) : null;
    })()`);
    check(
      'the action editor offers every run kind',
      (kinds ?? []).length === 10 && kinds.includes('approve') && kinds.includes('answer'),
      JSON.stringify(kinds),
    );

    // Close the drawer so the composer strip is reachable again.
    await cdp.evaluate(`(() => {
      const button = [...document.querySelectorAll('button')]
        .find((b) => b.getAttribute('title') === 'Close panel');
      if (button) button.click();
      return true;
    })()`);
    await sleep(400);

    // Nothing is pending, so the guard has to speak rather than throw. A button
    // that silently does nothing is indistinguishable from a broken one.
    const approve = await cdp.evaluate(`(async () => {
      const button = [...document.querySelectorAll('button')]
        .find((b) => b.textContent.trim() === 'Approve');
      if (!button) return { clicked: false };
      button.click();
      await new Promise((resolve) => setTimeout(resolve, 400));
      return {
        clicked: true,
        notices: [...document.querySelectorAll('[data-vd-notice]')].map((n) => n.textContent),
        body: document.body.innerText,
      };
    })()`);
    check(
      'approving with nothing pending says so',
      approve.clicked && (approve.body ?? '').includes('Nothing is waiting for approval'),
      JSON.stringify(approve.notices),
    );

    const answer = await cdp.evaluate(`(async () => {
      const button = [...document.querySelectorAll('button')]
        .find((b) => b.textContent.trim() === 'Decline');
      if (!button) return { clicked: false };
      button.click();
      await new Promise((resolve) => setTimeout(resolve, 400));
      return { clicked: true, body: document.body.innerText };
    })()`);
    check(
      'answering with nothing pending says so',
      answer.clicked && (answer.body ?? '').includes('Nothing is waiting for an answer'),
      String(answer.clicked),
    );
  } finally {
    await restoreActions();
  }

  return checks;
}

/**
 * The settings directory.
 *
 * The rail used to carry one icon per panel — eight of them, all the same
 * weight — so nothing in it read as more important than anything else and the
 * list grew with every surface added. This suite is the evidence that the rail
 * carries one entry now, that the directory lists every section exactly once,
 * that each row opens its own panel, that the rail stays lit while a section is
 * open, and that there is a way back. It also proves the directory is a readout
 * and not just a menu: at least one row carries a live value.
 */
async function settings(cdp) {
  const checks = [];
  const check = (name, ok, detail) => checks.push({ name, ok: Boolean(ok), detail });

  const railTitles = () =>
    cdp.evaluate(
      `[...document.querySelectorAll('nav button')].map((b) => b.getAttribute('title'))`,
    );

  // Start closed, so the first assertion is about the rail and not about
  // whatever the previous suite left open.
  await cdp.evaluate(`(() => {
    const button = [...document.querySelectorAll('nav button')]
      .find((b) => b.getAttribute('title') === 'Settings');
    if (button?.getAttribute('aria-pressed') === 'true') button.click();
    return true;
  })()`);
  await sleep(400);

  const titles = await railTitles();
  check(
    'the rail carries one settings entry',
    titles.filter((title) => title === 'Settings').length === 1,
    JSON.stringify(titles),
  );
  const strays = [
    'Plugins',
    'Configuration',
    'Sessions',
    'Context',
    'Billing',
    'Skills',
    'Customize',
    'Help',
  ].filter((title) => titles.includes(title));
  check('no panel is its own rail icon', strays.length === 0, strays.join(', '));

  await clickByTitle(cdp, 'Settings');
  await waitFor(
    cdp,
    `[...document.querySelectorAll('aside')].some((a) => a.querySelector('h2')?.textContent === 'Settings')`,
    { label: 'the settings directory' },
  );

  const directory = await cdp.evaluate(`(() => {
    const aside = [...document.querySelectorAll('aside')]
      .find((a) => a.querySelector('h2')?.textContent === 'Settings');
    if (!aside) return null;
    // The body, not the whole aside: the header carries a close button and a
    // back button, and counting those as rows is how this suite first failed.
    const body = aside.querySelector(':scope > div');
    const rows = [...body.querySelectorAll('button')];
    return {
      subtitle: aside.querySelector('p')?.textContent ?? '',
      groups: [...aside.querySelectorAll('h3')].map((h) => h.textContent),
      labels: rows.map((b) => b.querySelector('span span')?.textContent ?? ''),
      hints: rows.map((b) => b.querySelectorAll('span span')[1]?.textContent ?? ''),
      badges: rows.map((b) => b.querySelector('.font-mono')?.textContent ?? null),
    };
  })()`);

  check('the directory opened', directory !== null);
  check(
    'every row has a label',
    directory.labels.length > 0 && directory.labels.every((label) => label.length > 0),
    JSON.stringify(directory.labels),
  );
  check(
    'every row says what it is for',
    directory.hints.every((hint) => hint.length > 0),
    JSON.stringify(directory.hints),
  );
  check(
    'no section is listed twice',
    new Set(directory.labels).size === directory.labels.length,
    JSON.stringify(directory.labels),
  );
  check('the sections are grouped', directory.groups.length >= 3, JSON.stringify(directory.groups));
  check(
    'the subtitle counts the sections',
    /^\d+ sections$/.test(directory.subtitle),
    directory.subtitle,
  );
  check(
    'at least one row carries a live value',
    directory.badges.some((badge) => badge !== null),
    JSON.stringify(directory.badges),
  );

  // Every row opens its own panel, the rail stays lit, and the panel offers a
  // way back. The back button is clicked between rows so each one starts from
  // the directory rather than from the previous section.
  const opened = [];
  for (const label of directory.labels) {
    await cdp.evaluate(`(() => {
      const aside = [...document.querySelectorAll('aside')]
        .find((a) => a.querySelector('h2')?.textContent === 'Settings');
      const body = aside?.querySelector(':scope > div');
      const row = [...(body?.querySelectorAll('button') ?? [])]
        .find((b) => b.querySelector('span span')?.textContent === ${JSON.stringify(label)});
      if (!row) return false;
      row.click();
      return true;
    })()`);
    await sleep(350);
    opened.push(
      await cdp.evaluate(`(() => {
        const aside = [...document.querySelectorAll('aside')].find((a) => a.querySelector('h2'));
        const button = [...document.querySelectorAll('nav button')]
          .find((b) => b.getAttribute('title') === 'Settings');
        return {
          title: aside?.querySelector('h2')?.textContent ?? null,
          lit: button?.getAttribute('aria-pressed') === 'true',
          back: Boolean(document.querySelector('aside button[title="Back to settings"]')),
        };
      })()`),
    );
    await cdp.evaluate(`(() => {
      const back = document.querySelector('aside button[title="Back to settings"]');
      if (back) back.click();
      return true;
    })()`);
    await sleep(300);
  }

  check(
    'every section opens a panel',
    opened.every((row) => row.title !== null && row.title !== 'Settings'),
    JSON.stringify(opened.map((row) => row.title)),
  );
  check(
    'every section opens a different panel',
    new Set(opened.map((row) => row.title)).size === opened.length,
    JSON.stringify(opened.map((row) => row.title)),
  );
  check(
    'the rail stays lit inside a section',
    opened.every((row) => row.lit),
    JSON.stringify(opened.filter((row) => !row.lit).map((row) => row.title)),
  );
  check(
    'every section offers a way back',
    opened.every((row) => row.back),
    JSON.stringify(opened.filter((row) => !row.back).map((row) => row.title)),
  );

  // A section opened from somewhere else — the status bar's model chip — is
  // still a section, so it still gets the way back. Without this the hierarchy
  // would only exist when you entered through the directory.
  await cdp.evaluate(`(() => {
    const back = document.querySelector('aside button[title="Back to settings"]');
    if (back) back.click();
    return true;
  })()`);
  await sleep(300);
  await clickByTitle(cdp, 'Active model');
  await sleep(400);
  const fromStatusBar = await cdp.evaluate(`(() => {
    const aside = [...document.querySelectorAll('aside')].find((a) => a.querySelector('h2'));
    return {
      title: aside?.querySelector('h2')?.textContent ?? null,
      back: Boolean(document.querySelector('aside button[title="Back to settings"]')),
    };
  })()`);
  check(
    'a section opened from the status bar is still a section',
    fromStatusBar.title === 'Configuration' && fromStatusBar.back,
    JSON.stringify(fromStatusBar),
  );

  await cdp.evaluate(`(() => {
    const back = document.querySelector('aside button[title="Back to settings"]');
    if (back) back.click();
    return true;
  })()`);
  await sleep(300);
  const returned = await cdp.evaluate(
    `[...document.querySelectorAll('aside')].some((a) => a.querySelector('h2')?.textContent === 'Settings')`,
  );
  check('the way back lands on the directory', returned);

  await cdp.evaluate(`(() => {
    const close = document.querySelector('aside button[title="Close panel"]');
    if (close) close.click();
    return true;
  })()`);
  await sleep(300);
  const closed = await cdp.evaluate(
    `![...document.querySelectorAll('aside')].some((a) => a.querySelector('h2')?.textContent === 'Settings')`,
  );
  check('closing the directory leaves no panel open', closed);

  return checks;
}

/**
 * The keyboard paths.
 *
 * Two surfaces were mouse-only: the context menus, which were opened from
 * `onContextMenu` with no focus management and no arrow keys, and the rail
 * resize handle, which had pointer handlers and no tab stop. Both are reachable
 * now, and this suite is what says so — a menu that opens but cannot be walked
 * is not a keyboard path, and a separator that takes focus but ignores the arrow
 * keys is a tab stop that does nothing.
 */
async function keyboard(cdp, { dataDir, keep }) {
  const checks = [];
  const check = (name, ok, detail) => checks.push({ name, ok: Boolean(ok), detail });

  const layoutPath = join(dataDir, 'config', 'layout.json');
  let previousLayout = null;
  try {
    previousLayout = await readFile(layoutPath, 'utf8');
  } catch {
    previousLayout = null;
  }

  // The first chat row in the rail. Any row will do: the menu is the same menu,
  // and nothing here selects an item, so no chat is opened or deleted.
  const rowFor = `[...document.querySelectorAll('aside button')].find((button) =>
    button.querySelector('span.font-mono'),
  )`;

  try {
    await waitFor(cdp, `Boolean(${rowFor})`, { label: 'a chat row', timeoutMs: 20000 });

    // 1. Shift+F10 opens the menu on the focused row, and the menu takes focus.
    await cdp.evaluate(`(() => {
      const row = ${rowFor};
      row.focus();
      row.dispatchEvent(new KeyboardEvent('keydown', { key: 'F10', shiftKey: true, bubbles: true }));
      return true;
    })()`);

    let menuOpen = true;
    try {
      await waitFor(cdp, `document.querySelectorAll('[role=menuitem]').length > 0`, {
        label: 'the menu from Shift+F10',
        timeoutMs: 5000,
      });
    } catch {
      menuOpen = false;
    }
    check('Shift+F10 opens the menu', menuOpen);

    const focused = await cdp.evaluate(`(() => {
      const active = document.activeElement;
      return {
        role: active?.getAttribute('role') ?? null,
        label: active?.textContent?.trim() ?? null,
      };
    })()`);
    check('the menu takes focus on open', focused.role === 'menuitem', JSON.stringify(focused));

    // 2. The arrow keys walk the menu.
    //
    // Each key waits for focus to actually land somewhere instead of sleeping a
    // fixed 120 ms. The sleep was the same guess `waitFor` exists to replace: it
    // passed alone and failed in `all`, twice in five runs, because the render
    // that moves focus competes with whatever the previous suite left mounted.
    // `settle` returns the label as soon as it changes and gives up after a
    // second, so a menu whose keys genuinely do nothing still fails — with the
    // label it stayed on.
    const walked = await cdp.evaluate(`(async () => {
      const label = () => document.activeElement?.textContent?.trim() ?? null;
      const settle = async (was) => {
        for (let i = 0; i < 20; i += 1) {
          if (label() !== was) return label();
          await new Promise((r) => setTimeout(r, 50));
        }
        return label();
      };
      const first = label();
      document.activeElement?.dispatchEvent(
        new KeyboardEvent('keydown', { key: 'ArrowDown', bubbles: true }),
      );
      const second = await settle(first);
      document.activeElement?.dispatchEvent(
        new KeyboardEvent('keydown', { key: 'ArrowUp', bubbles: true }),
      );
      const back = await settle(second);
      return { first, second, back, count: document.querySelectorAll('[role=menuitem]').length };
    })()`);
    check(
      'ArrowDown moves focus inside the menu',
      walked.first !== null && walked.second !== null && walked.first !== walked.second,
      JSON.stringify(walked),
    );
    check('ArrowUp moves it back', walked.back === walked.first, JSON.stringify(walked));

    // 3. Escape closes the menu and hands focus back to the row it came from.
    await cdp.evaluate(
      `document.activeElement?.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true })), null`,
    );
    let closed = true;
    try {
      await waitFor(cdp, `document.querySelectorAll('[role=menuitem]').length === 0`, {
        label: 'the menu to close',
        timeoutMs: 5000,
      });
    } catch {
      closed = false;
    }
    check('Escape closes the menu', closed);

    const restored = await cdp.evaluate(`(() => ({
      tag: document.activeElement?.tagName ?? null,
      isRow: document.activeElement === ${rowFor},
    }))()`);
    check('focus goes back to the row', restored.isRow === true, JSON.stringify(restored));

    // 4. The resize handle is a real window splitter.
    const handle = await cdp.evaluate(`(() => {
      const element = document.querySelector('[role=separator][aria-label="Resize the workstation rail"]');
      if (!element) return null;
      return {
        tabIndex: element.tabIndex,
        now: Number(element.getAttribute('aria-valuenow')),
        min: Number(element.getAttribute('aria-valuemin')),
        max: Number(element.getAttribute('aria-valuemax')),
      };
    })()`);
    check('the resize handle is focusable', handle?.tabIndex === 0, JSON.stringify(handle));
    check(
      'the resize handle reports its range',
      handle !== null &&
        handle.min === 200 &&
        handle.max === 520 &&
        handle.now >= 200 &&
        handle.now <= 520,
      JSON.stringify(handle),
    );

    const resized = await cdp.evaluate(`(async () => {
      const element = document.querySelector('[role=separator][aria-label="Resize the workstation rail"]');
      const read = () =>
        Number.parseInt(
          getComputedStyle(document.documentElement).getPropertyValue('--vd-sidebar-w'),
          10,
        );
      const before = read();
      element.focus();
      element.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowLeft', bubbles: true }));
      await new Promise((r) => setTimeout(r, 150));
      return { before, after: read(), announced: Number(element.getAttribute('aria-valuenow')) };
    })()`);
    check('an arrow key resizes the rail', resized.after !== resized.before, JSON.stringify(resized));
    check(
      'the announced width follows the key',
      resized.announced === resized.after,
      JSON.stringify(resized),
    );

    // The width is persisted once the key repeat settles, so the file is the
    // proof that the keyboard path goes through the same save as the drag.
    let saved = null;
    try {
      await waitFor(
        cdp,
        `window.__TAURI_INTERNALS__.invoke('layout_get').then((layout) => layout.sidebarWidth === ${resized.after})`,
        { label: 'the width to be saved', timeoutMs: 6000 },
      );
      saved = resized.after;
    } catch {
      saved = null;
    }
    check('the keyboard width is saved', saved === resized.after, `saved ${saved}`);
  } finally {
    if (previousLayout !== null) {
      // Put the rail back where it was through the same command the app uses, so
      // the file and the running window agree. Writing the file alone would
      // leave the next suite measuring a width the app no longer has.
      await cdp
        .evaluate(
          `window.__TAURI_INTERNALS__.invoke('layout_set', { layout: ${previousLayout} }).then((layout) => layout)`,
        )
        .catch(() => null);
    }
  }

  return checks;
}

const SUITES = {
  smoke,
  panels,
  data,
  plugins,
  motion,
  shape,
  chats,
  bulk,
  widget,
  chart,
  diagram,
  agentActions,
  settings,
  commands,
  keyboard,
};

/**
 * Every local slash command, and the keyboard paths around the composer.
 *
 * Phase 5 of the customization plan is a sweep, and a sweep is only worth
 * anything if it is repeatable. Each command is typed the way a user types it
 * — into the composer, completed from the palette, then run — and each one has
 * to produce the surface it promises. A command that silently does nothing is
 * the defect this suite exists to catch.
 *
 * `/quit` is not exercised: it closes the window, which would end the run.
 */
async function commands(cdp) {
  const checks = [];
  const check = (name, ok, detail) => checks.push({ name, ok: Boolean(ok), detail });

  const type = (text) =>
    cdp.evaluate(`(() => {
      const input = document.querySelector('textarea.vd-input');
      if (!input) return false;
      const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value').set;
      setter.call(input, ${JSON.stringify(text)});
      input.dispatchEvent(new Event('input', { bubbles: true }));
      input.focus();
      return true;
    })()`);

  const press = (key, init = '{}') =>
    cdp.evaluate(`(() => {
      const input = document.querySelector('textarea.vd-input');
      input?.dispatchEvent(new KeyboardEvent('keydown', Object.assign({ key: ${JSON.stringify(key)}, bubbles: true }, ${init})));
      return true;
    })()`);

  // The palette completes a command on the first Enter and runs it on the
  // second, so a command is two presses.
  const run = async (command) => {
    await type(command);
    // The palette has to be on screen before Enter means "complete". Pressing it
    // against a list that has not committed sends the raw text down the prompt
    // path instead, which is a different code path and a different result.
    await waitFor(cdp, `Boolean(document.querySelector('[role=listbox]'))`, {
      label: 'the palette',
      timeoutMs: 5000,
    }).catch(() => {});
    await sleep(150);
    await press('Enter');
    await sleep(300);
    await press('Enter');
    await sleep(400);
  };

  const header = () =>
    cdp.evaluate(
      `document.querySelector('.vd-panel-surface header')?.innerText?.split(String.fromCharCode(10))[0] ?? null`,
    );

  const closePanel = async () => {
    await cdp.evaluate(
      `(document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true })), true)`,
    );
    await sleep(400);
  };

  await waitFor(cdp, `Boolean(document.querySelector('textarea.vd-input'))`, {
    label: 'the composer',
    timeoutMs: 20000,
  });

  // 1. Every command that opens a surface opens the surface it names.
  const opens = [
    ['/config', 'Configuration'],
    ['/plugins', 'Plugins'],
    ['/context', 'Context'],
    ['/status', 'Agent status'],
    ['/billing', 'Billing'],
    ['/skills', 'Skills'],
    ['/release-notes', 'Release notes'],
    ['/account', 'Account'],
    ['/help', 'Help'],
    ['/resume', 'Sessions'],
  ];

  for (const [command, title] of opens) {
    await closePanel();
    await run(command);
    let opened = true;
    try {
      await waitFor(
        cdp,
        `document.querySelector('.vd-panel-surface header')?.innerText?.includes(${JSON.stringify(title)}) ?? false`,
        { label: title, timeoutMs: 8000 },
      );
    } catch {
      opened = false;
    }
    check(`${command} opens ${title}`, opened, String(await header()));
    await closePanel();
  }

  // 2. `/theme` switches the theme, and switches it back.
  const themeBefore = await cdp.evaluate(`document.documentElement.dataset.theme`);
  await run('/theme');
  await sleep(900);
  const themeAfter = await cdp.evaluate(`document.documentElement.dataset.theme`);
  check('/theme switches the theme', themeBefore !== themeAfter, `${themeBefore} -> ${themeAfter}`);
  await run('/theme');
  await sleep(900);
  const themeBack = await cdp.evaluate(`document.documentElement.dataset.theme`);
  check('/theme switches it back', themeBack === themeBefore, `${themeAfter} -> ${themeBack}`);

  // 3. `/clear` starts a new conversation.
  await run('/clear');
  let cleared = true;
  try {
    await waitFor(cdp, `Boolean(document.querySelector('.vd-mesh'))`, {
      label: 'the empty state',
      timeoutMs: 15000,
    });
  } catch {
    cleared = false;
  }
  check('/clear starts a new conversation', cleared);

  // 4. The palette itself. A `/` opens it, the arrows move the highlight, and
  //    Escape closes it without touching the draft.
  await type('/');
  await sleep(300);
  const palette = await cdp.evaluate(`(() => {
    const list = document.querySelector('[role=listbox]');
    return {
      open: Boolean(list),
      options: list ? list.querySelectorAll('[role=option]').length : 0,
      active: list ? list.querySelectorAll('[aria-selected=true]').length : 0,
    };
  })()`);
  check('a slash opens the palette', palette.open, JSON.stringify(palette));
  check('the palette lists commands', palette.options > 5, JSON.stringify(palette));
  check('exactly one command is highlighted', palette.active === 1, JSON.stringify(palette));

  const moved = await cdp.evaluate(`(async () => {
    const list = document.querySelector('[role=listbox]');
    const first = list?.querySelector('[role=option][aria-selected=true]')?.textContent ?? null;
    const input = document.querySelector('textarea.vd-input');
    input?.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowDown', bubbles: true }));
    await new Promise((r) => setTimeout(r, 200));
    const second = document.querySelector('[role=listbox] [role=option][aria-selected=true]')?.textContent ?? null;
    return { first, second };
  })()`);
  check(
    'ArrowDown moves the highlight',
    moved.first !== null && moved.second !== null && moved.first !== moved.second,
    JSON.stringify(moved),
  );

  await press('Escape');
  await sleep(250);
  const closed = await cdp.evaluate(`({
    open: Boolean(document.querySelector('[role=listbox]')),
    draft: document.querySelector('textarea.vd-input')?.value ?? null,
  })`);
  check('Escape closes the palette', closed.open === false, JSON.stringify(closed));
  check('Escape leaves the draft alone', closed.draft === '/', JSON.stringify(closed));

  // 5. Shift+Enter is a newline, not a send. The composer is the one place a
  //    stray Enter costs the user a turn.
  await type('line one');
  await sleep(200);
  const newline = await cdp.evaluate(`(() => {
    const input = document.querySelector('textarea.vd-input');
    const before = input.value;
    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', shiftKey: true, bubbles: true }));
    return { before, after: input.value };
  })()`);
  check(
    'Shift+Enter does not send',
    newline.after === newline.before,
    JSON.stringify(newline),
  );

  // 6. A local command that is still running must not eat what the user types
  //    next. `/clear` is a round trip to the app server, so the window between
  //    the command starting and the composer being cleared is real, and a draft
  //    typed inside it used to be deleted.
  await type('/clear');
  await sleep(150);
  await press('Enter');
  await sleep(300);
  await press('Enter');
  await type('typed while the command ran');
  await sleep(2500);
  const kept = await cdp.evaluate(
    `document.querySelector('textarea.vd-input')?.value ?? null`,
  );
  check(
    'a running command does not eat the next draft',
    kept === 'typed while the command ran',
    JSON.stringify(kept),
  );

  // Leave the composer empty so the next suite starts from a clean draft.
  await type('');
  await sleep(200);

  return checks;
}

/* ---------------------------------------------------------------- *
 * CLI
 * ---------------------------------------------------------------- */

function parseArgs(argv) {
  const args = {
    command: argv[0] ?? 'smoke',
    port: DEFAULT_PORT,
    dataDir: DEFAULT_DATA_DIR,
    keep: false,
    json: false,
    rest: [],
  };
  for (let index = 1; index < argv.length; index += 1) {
    const token = argv[index];
    if (token === '--port') args.port = Number(argv[++index]);
    else if (token === '--data-dir') args.dataDir = argv[++index];
    else if (token === '--shot') args.shot = argv[++index];
    else if (token === '--keep') args.keep = true;
    else if (token === '--json') args.json = true;
    else args.rest.push(token);
  }
  return args;
}

function report(name, checks, { json }) {
  if (json) {
    console.log(JSON.stringify({ suite: name, checks }, null, 2));
    return;
  }
  for (const check of checks) {
    console.log(`${check.ok ? 'ok  ' : 'FAIL'}  ${check.name}`);
    if (!check.ok && check.detail) console.log(`        ${check.detail}`);
  }
}

async function main() {
  const args = parseArgs(process.argv.slice(2));

  let cdp;
  try {
    cdp = await Cdp.attach(args.port);
  } catch (error) {
    console.error(`live-check: ${error.message}`);
    console.error(
      'live-check: start the app with VIBEDESK_REMOTE_DEBUGGING=9222 and a debug build',
    );
    process.exit(2);
  }

  try {
    if (args.command === 'probe') {
      const version = await (await fetch(`http://127.0.0.1:${args.port}/json/version`)).json();
      console.log(JSON.stringify(version, null, 2));
      return 0;
    }

    if (args.command === 'eval') {
      const expression = args.rest.join(' ');
      if (!expression) {
        console.error('live-check: eval needs an expression');
        return 1;
      }
      console.log(JSON.stringify(await cdp.evaluate(expression), null, 2));
      return 0;
    }

    if (args.command === 'shot') {
      const path = args.rest[0] ?? 'live-check.png';
      await cdp.send('Page.enable');
      await writeFile(path, await cdp.screenshot());
      console.log(`wrote ${path}`);
      return 0;
    }

    const names = args.command === 'all' ? Object.keys(SUITES) : [args.command];
    if (!names.every((name) => SUITES[name])) {
      console.error(`live-check: unknown suite ${args.command}`);
      console.error(`live-check: known suites are ${Object.keys(SUITES).join(', ')}, all`);
      return 1;
    }

    const { errors, warnings } = await watchConsole(cdp);
    await cdp.send('Page.enable');
    await waitFor(cdp, `(document.querySelector('#root')?.children.length ?? 0) > 0`, {
      label: 'the app to mount',
    });
    await sleep(800);

    let failed = 0;
    let total = 0;
    for (const name of names) {
      let checks = [];
      try {
        checks = await SUITES[name](cdp, {
          dataDir: args.dataDir,
          keep: args.keep,
        });
      } catch (error) {
        // A suite that throws is a failed suite, not a crashed run. The other
        // suites still have something to say, and a stack trace is not a
        // result — a timeout has to read as a named failure like any other.
        //
        // The checks it already made are kept: a suite that fails on its
        // twelfth check has eleven results worth reading, and discarding them
        // turns one timeout into "0/1 checks".
        const partial = Array.isArray(error?.checks) ? error.checks : [];
        checks = [
          ...partial,
          { name: `${name} ran to completion`, ok: false, detail: error.message },
        ];
      }
      report(name, checks, { json: args.json });
      failed += checks.filter((check) => !check.ok).length;
      total += checks.length;
      if (!args.json) console.log('');
    }

    if (!args.json) {
      for (const warning of warnings) console.log(`warn  ${warning}`);
      for (const error of errors) console.log(`ERROR ${error}`);
      console.log(
        `${total - failed}/${total} checks, ${errors.length} console errors, ${warnings.length} warnings`,
      );
    }

    return failed === 0 && errors.length === 0 ? 0 : 1;
  } finally {
    cdp.close();
  }
}

process.exit(await main());
