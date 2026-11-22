#!/usr/bin/env node
/**
 * Real-input UI harness.
 *
 * `live-check.mjs` drives the app by calling `.click()` on elements from inside
 * the page. That is fine for asserting state, and useless for finding out what
 * happens when a person actually uses the thing: it never moves a pointer, never
 * presses a key, never drags past an edge, and never clicks twice in the same
 * frame. Every bug that lives in the gap between "the handler ran" and "the
 * gesture was real" is invisible to it.
 *
 * This harness drives the app through the Chrome DevTools Protocol's input
 * domain instead, so the events arrive the way the window receives them:
 *
 *   Input.dispatchMouseEvent   mouseMoved / mousePressed / mouseReleased / mouseWheel
 *   Input.dispatchKeyEvent     keyDown / keyUp / char, with modifiers
 *   Input.insertText           a paste-sized insertion
 *
 * Chromium synthesises pointer events from those mouse events, so a drag here
 * exercises the same `pointerdown`/`pointermove`/`pointerup` path a trackpad
 * does.
 *
 * Three suites, three kinds of user:
 *
 *   expert       someone who knows the app: every drawer, every control, the
 *                keyboard path, a settings round-trip that has to survive Rust.
 *   novice       someone who does not: is anything discoverable, does every
 *                empty state say what to do, is any control a dead end.
 *   adversarial  someone trying to break it: double and triple clicks, drags
 *                past both edges, Escape mid-gesture, 200k characters, emoji,
 *                right-to-left text, a window the size of a phone.
 *
 * `selftest` runs first in `all` and proves the input layer itself works. A
 * harness that cannot click is a harness whose every failure is a lie, so the
 * primitives are checked against the real window before any suite trusts them.
 *
 *   node scripts/ui-probe.mjs selftest
 *   node scripts/ui-probe.mjs expert
 *   node scripts/ui-probe.mjs novice
 *   node scripts/ui-probe.mjs adversarial
 *   node scripts/ui-probe.mjs all
 *   node scripts/ui-probe.mjs all --json
 *   node scripts/ui-probe.mjs all --shots .shots/probe
 *
 * Exit code 0 when every check passed, 1 when one failed, 2 when the app could
 * not be reached, 3 when the run hit its watchdog.
 *
 * No dependencies. Node 24 has `fetch` and `WebSocket` built in.
 */

import { execFile } from 'node:child_process';
import { mkdir, readdir, stat, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { Cdp } from './cdp-client.mjs';

const DEFAULT_PORT = 9222;



/* ---------------------------------------------------------------- *
 * Real input
 * ---------------------------------------------------------------- */

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/** Modifier bits, as the protocol defines them. */
const ALT = 1;
const CTRL = 2;
const META = 4;
const SHIFT = 8;

/**
 * The keys this harness presses, with the three fields Chromium needs.
 *
 * `text` is what turns a key press into a character. A key without it is a
 * command key, and sending `text` for one of those inserts a stray glyph.
 */
const KEYS = {
  Enter: { code: 'Enter', vk: 13, text: '\r' },
  Escape: { code: 'Escape', vk: 27 },
  Tab: { code: 'Tab', vk: 9 },
  Backspace: { code: 'Backspace', vk: 8 },
  Delete: { code: 'Delete', vk: 46 },
  Space: { code: 'Space', vk: 32, text: ' ' },
  ArrowLeft: { code: 'ArrowLeft', vk: 37 },
  ArrowUp: { code: 'ArrowUp', vk: 38 },
  ArrowRight: { code: 'ArrowRight', vk: 39 },
  ArrowDown: { code: 'ArrowDown', vk: 40 },
  Home: { code: 'Home', vk: 36 },
  End: { code: 'End', vk: 35 },
  PageUp: { code: 'PageUp', vk: 33 },
  PageDown: { code: 'PageDown', vk: 34 },
  a: { code: 'KeyA', vk: 65 },
  c: { code: 'KeyC', vk: 67 },
  v: { code: 'KeyV', vk: 86 },
  z: { code: 'KeyZ', vk: 90 },
};

class Input {
  constructor(cdp) {
    this.cdp = cdp;
  }

  /* ---- pointer ---- */

  async move(x, y) {
    await this.cdp.send('Input.dispatchMouseEvent', {
      type: 'mouseMoved',
      x,
      y,
      button: 'none',
      buttons: 0,
    });
  }

  async down(x, y, { button = 'left', clickCount = 1 } = {}) {
    await this.cdp.send('Input.dispatchMouseEvent', {
      type: 'mousePressed',
      x,
      y,
      button,
      buttons: button === 'left' ? 1 : button === 'right' ? 2 : 4,
      clickCount,
    });
  }

  async up(x, y, { button = 'left', clickCount = 1 } = {}) {
    await this.cdp.send('Input.dispatchMouseEvent', {
      type: 'mouseReleased',
      x,
      y,
      button,
      buttons: 0,
      clickCount,
    });
  }

  /** Move, press, release. The move matters: hover state is part of the UI. */
  async click(x, y, { button = 'left', clickCount = 1 } = {}) {
    await this.move(x, y);
    await this.down(x, y, { button, clickCount });
    await this.up(x, y, { button, clickCount });
  }

  /**
   * A real double click: two press/release pairs, the second with clickCount 2.
   *
   * Sending one pair with clickCount 2 is not a double click — the browser
   * never sees the first click, so `dblclick` never fires and a control that
   * listens for it silently does nothing.
   */
  async doubleClick(x, y) {
    await this.click(x, y, { clickCount: 1 });
    await this.click(x, y, { clickCount: 2 });
  }

  async rightClick(x, y) {
    await this.click(x, y, { button: 'right' });
  }

  /**
   * Press, move in steps, release.
   *
   * The intermediate moves are the point: a drag that jumps straight to its
   * destination never produces a `pointermove`, so a handler that tracks the
   * pointer sees nothing and the element does not move.
   */
  async drag(from, to, { steps = 12, button = 'left' } = {}) {
    await this.move(from.x, from.y);
    await this.down(from.x, from.y, { button });
    for (let step = 1; step <= steps; step += 1) {
      const x = from.x + ((to.x - from.x) * step) / steps;
      const y = from.y + ((to.y - from.y) * step) / steps;
      await this.cdp.send('Input.dispatchMouseEvent', {
        type: 'mouseMoved',
        x,
        y,
        button,
        buttons: 1,
      });
    }
    await this.up(to.x, to.y, { button });
  }

  /** Press and move without releasing, so a test can abandon a gesture. */
  async dragStart(from) {
    await this.move(from.x, from.y);
    await this.down(from.x, from.y);
  }

  async dragTo(to) {
    await this.cdp.send('Input.dispatchMouseEvent', {
      type: 'mouseMoved',
      x: to.x,
      y: to.y,
      button: 'left',
      buttons: 1,
    });
  }

  async dragEnd(at) {
    await this.up(at.x, at.y);
  }

  async wheel(x, y, deltaY, deltaX = 0) {
    await this.cdp.send('Input.dispatchMouseEvent', {
      type: 'mouseWheel',
      x,
      y,
      deltaX,
      deltaY,
    });
  }

  /* ---- keyboard ---- */

  async key(name, { modifiers = 0 } = {}) {
    const spec = KEYS[name] ?? { code: name, vk: 0 };
    const base = {
      key: name,
      code: spec.code,
      windowsVirtualKeyCode: spec.vk,
      nativeVirtualKeyCode: spec.vk,
      modifiers,
    };
    // A modified key is a command, not a character: Ctrl+A must not insert "a".
    const text = modifiers === 0 ? spec.text : undefined;
    await this.cdp.send('Input.dispatchKeyEvent', {
      type: text ? 'keyDown' : 'rawKeyDown',
      ...base,
      ...(text ? { text } : {}),
    });
    await this.cdp.send('Input.dispatchKeyEvent', { type: 'keyUp', ...base });
  }

  /** Insert text the way a paste does: one input event, no key events. */
  async type(text) {
    await this.cdp.send('Input.insertText', { text });
  }

  /**
   * Type character by character, as a person does.
   *
   * Slower, and the only way to exercise a handler that reacts to individual
   * key presses rather than to the resulting value.
   */
  async typeKeys(text) {
    for (const char of text) {
      await this.cdp.send('Input.dispatchKeyEvent', {
        type: 'keyDown',
        key: char,
        text: char,
        unmodifiedText: char,
      });
      await this.cdp.send('Input.dispatchKeyEvent', { type: 'keyUp', key: char });
    }
  }

  async selectAll() {
    await this.key('a', { modifiers: CTRL });
  }

  async clearField() {
    await this.selectAll();
    await this.key('Backspace');
  }
}

/* ---------------------------------------------------------------- *
 * Page helpers
 * ---------------------------------------------------------------- */

/**
 * Poll an expression until it is truthy.
 *
 * A fixed delay is a guess, and a wrong guess reports a slow frame as a broken
 * app. Polling for the condition costs time instead of a false failure.
 */
async function waitFor(cdp, expression, { timeoutMs = 12000, label = expression } = {}) {
  const deadline = Date.now() + timeoutMs;
  let last = null;
  while (Date.now() < deadline) {
    try {
      const value = await cdp.evaluate(expression);
      if (value) return value;
      last = value;
    } catch (error) {
      last = error.message;
    }
    await sleep(120);
  }
  throw new Error(`timed out waiting for ${label} (last: ${JSON.stringify(last)})`);
}

/**
 * Wait until a point actually belongs to the element.
 *
 * This is the single most important primitive here. A drawer that is still
 * entering, or a view transition that is still running, puts something else on
 * top of the target: the click lands on the overlay and the control silently
 * does nothing. A fixed sleep hides that, and hides it differently on a slow
 * frame. Polling the hit test is the only honest way to know the click will
 * reach what it is aimed at.
 */
async function waitClickable(cdp, expression, { timeoutMs = 8000 } = {}) {
  const deadline = Date.now() + timeoutMs;
  let last = null;
  let scrolled = false;
  while (Date.now() < deadline) {
    const state = await cdp.evaluate(`(() => {
      const el = ${expression};
      if (!el) return { ok: false, why: 'missing' };
      const r = el.getBoundingClientRect();
      if (r.width < 1 || r.height < 1) return { ok: false, why: 'zero-size' };
      const cx = r.x + r.width / 2;
      const cy = r.y + r.height / 2;
      if (cx < 0 || cy < 0 || cx > window.innerWidth || cy > window.innerHeight) {
        return { ok: false, why: 'offscreen at ' + Math.round(cx) + ',' + Math.round(cy) };
      }
      const top = document.elementFromPoint(cx, cy);
      if (!top) return { ok: false, why: 'nothing at the point' };
      if (top !== el && !el.contains(top)) {
        return {
          ok: false,
          why: 'covered by ' + top.tagName + '.' + String(top.className).slice(0, 40),
        };
      }
      return { ok: true, cx, cy };
    })()`);
    if (state?.ok) return state;
    last = state;
    // A target inside a scroll container can be scrolled out of view, and then
    // it never becomes clickable no matter how long this waits — the rail grows
    // past the window as chats accumulate, and the workstation row ends up above
    // the fold. A user scrolls to it; so does this. `nearest` leaves an element
    // that is already fully visible exactly where it is, so this is a no-op for
    // every target that was reachable before.
    if (!scrolled && state?.why?.startsWith('offscreen')) {
      scrolled = true;
      await cdp.evaluate(
        `(() => { const el = ${expression}; if (el) el.scrollIntoView({ block: 'nearest', inline: 'nearest' }); return true; })()`,
      );
      await sleep(120);
      continue;
    }
    await sleep(80);
  }
  throw new Error(`the target never became clickable (${last?.why ?? 'timeout'})`);
}

/**
 * Wait until an element stops moving.
 *
 * A drag has to start from where the element is, not from where it was a frame
 * ago. Two identical samples in a row is the cheapest honest definition of
 * "stopped".
 */
async function waitStable(cdp, expression, { timeoutMs = 6000 } = {}) {
  const deadline = Date.now() + timeoutMs;
  let previous = null;
  let stable = 0;
  while (Date.now() < deadline) {
    const rect = await rectOfExpr(cdp, expression);
    if (rect) {
      const same =
        previous &&
        Math.abs(rect.x - previous.x) < 0.5 &&
        Math.abs(rect.y - previous.y) < 0.5 &&
        Math.abs(rect.w - previous.w) < 0.5;
      stable = same ? stable + 1 : 0;
      previous = rect;
      if (stable >= 2) return rect;
    }
    await sleep(80);
  }
  return previous;
}

/**
 * The point a drag must start from: stable *and* on top.
 *
 * A drag needs both. `waitStable` alone let a drag begin on the drawer's
 * transition overlay, which swallowed the pointer-down and left the rail
 * exactly where it was — a failure that read as a broken resize rather than a
 * mistimed one.
 */
async function waitDragStart(cdp, expression, options = {}) {
  try {
    return await waitClickable(cdp, expression, options);
  } catch {
    return null;
  }
}

/** The box of the first element matching a selector, or null. */
function rectOf(cdp, selector) {
  return cdp.evaluate(`(() => {
    const el = document.querySelector(${JSON.stringify(selector)});
    if (!el) return null;
    const r = el.getBoundingClientRect();
    if (r.width < 1 || r.height < 1) return null;
    return { x: r.x, y: r.y, w: r.width, h: r.height, cx: r.x + r.width / 2, cy: r.y + r.height / 2 };
  })()`);
}

/** The box of the element an expression evaluates to, or null. */
function rectOfExpr(cdp, expression) {
  return cdp.evaluate(`(() => {
    const el = ${expression};
    if (!el) return null;
    const r = el.getBoundingClientRect();
    if (r.width < 1 || r.height < 1) return null;
    return { x: r.x, y: r.y, w: r.width, h: r.height, cx: r.x + r.width / 2, cy: r.y + r.height / 2 };
  })()`);
}

/** The open drawer, identified by its close button rather than by being an aside. */
const DRAWER = `[...document.querySelectorAll('aside')].find((a) => a.querySelector('[title="Close panel"]'))`;

/**
 * Bring an element into view before touching it.
 *
 * A drawer grows as panels gain rows, and a field that was reachable last week
 * can end up below the fold. A person scrolls to it; so does this. Without it
 * the click lands on whatever happens to be at those coordinates — or on
 * nothing — and the check fails as though the control were broken.
 */
async function scrollTo(cdp, expression) {
  await cdp.evaluate(`(() => {
    const el = ${expression};
    if (el) el.scrollIntoView({ block: 'center', inline: 'nearest' });
  })()`);
  await sleep(250);
}

const drawerTitle = (cdp) =>
  cdp.evaluate(`(${DRAWER})?.querySelector('h2')?.textContent ?? null`);

const drawerText = (cdp) => cdp.evaluate(`(${DRAWER})?.innerText ?? null`);

const composerValue = (cdp) =>
  cdp.evaluate(`document.querySelector('textarea.vd-input')?.value ?? null`);

/** The rail width, read from the custom property the layout writes. */
const railWidth = (cdp) =>
  cdp.evaluate(
    `Number.parseFloat(getComputedStyle(document.documentElement).getPropertyValue('--vd-sidebar-w')) || 0`,
  );

/** Click the centre of an element found by expression, once it is clickable. */
async function clickExpr(cdp, input, expression, options) {
  const point = await waitClickable(cdp, expression);
  await input.click(point.cx, point.cy, options);
  return point;
}

/** Click the centre of the first element matching a selector. */
async function clickSelector(cdp, input, selector, options) {
  return clickExpr(cdp, input, `document.querySelector(${JSON.stringify(selector)})`, options);
}

/** A rail button, by its title. */
const railButton = (title) =>
  `[...document.querySelectorAll('button')].find((b) => b.getAttribute('title') === ${JSON.stringify(title)})`;

/**
 * Run a slash command the way the composer does it.
 *
 * The palette completes on the first Enter and the command runs on the second.
 * Pressing once tests the palette, not the command.
 */
async function runCommand(cdp, input, command) {
  await clickSelector(cdp, input, 'textarea.vd-input');
  await input.clearField();
  await input.type(command);
  await waitFor(cdp, `Boolean(document.querySelector('[role=listbox]'))`, {
    label: `the palette for ${command}`,
    timeoutMs: 6000,
  });
  await input.key('Enter');
  await sleep(200);
  await input.key('Enter');
  await sleep(500);
}

/** Close whatever drawer is open, with the mouse. */
async function closeDrawer(cdp, input) {
  const target = `${DRAWER}?.querySelector('[title="Close panel"]')`;
  const exists = await cdp.evaluate(`Boolean(${target})`);
  if (!exists) return false;
  await clickExpr(cdp, input, target);
  await waitFor(cdp, `!(${DRAWER})`, { label: 'the drawer to close', timeoutMs: 6000 });
  return true;
}

/** The rail's resize handle, which sits on the sidebar's left edge. */
const SEPARATOR = `document.querySelector('[role="separator"][aria-label="Resize the workstation rail"]')`;

/**
 * Make sure the workstation rail is on screen.
 *
 * The rail can be collapsed, and the collapse is persisted to the layout file,
 * so a suite that assumes it is open fails on a machine where it was closed —
 * the handle is inside the hidden aside and measures 0x0, which reads as "no
 * resize handle" rather than "the rail is closed". Every drag check puts it back
 * first, so the result does not depend on what ran before it.
 */
async function ensureRailVisible(cdp, input) {
  const hidden = await cdp.evaluate(
    `(() => {
      const aside = document.querySelector('aside.vd-sidebar');
      return !aside || getComputedStyle(aside).display === 'none';
    })()`,
  );
  if (!hidden) return false;
  await clickExpr(cdp, input, railButton('Show the workstation rail'));
  await waitFor(
    cdp,
    `(() => {
      const aside = document.querySelector('aside.vd-sidebar');
      return Boolean(aside) && getComputedStyle(aside).display !== 'none';
    })()`,
    { label: 'the workstation rail' },
  );
  return true;
}

/**
 * Put the sidebar back to a width, by dragging.
 *
 * Dragging right narrows the sidebar, so the pointer delta is the width change
 * with the sign flipped. Retried, because a drag that lands during a layout
 * pass can be swallowed.
 */
async function setRailWidth(cdp, input, target) {
  for (let attempt = 0; attempt < 4; attempt += 1) {
    const current = await railWidth(cdp);
    if (Math.abs(current - target) < 1) return current;
    const handle = await waitDragStart(cdp, SEPARATOR);
    if (!handle) return current;
    await input.drag({ x: handle.cx, y: handle.cy }, { x: handle.cx + (current - target), y: handle.cy });
    await sleep(350);
  }
  return railWidth(cdp);
}

/* ---------------------------------------------------------------- *
 * The probe: checks, screenshots, timing
 * ---------------------------------------------------------------- */

class Probe {
  constructor(cdp, { shotDir = null, json = false } = {}) {
    this.cdp = cdp;
    this.input = new Input(cdp);
    this.shotDir = shotDir;
    this.json = json;
    this.checks = [];
    this.shots = [];
  }

  /**
   * Run one check.
   *
   * A check that throws is a failed check, not a failed run: the rest of the
   * suite still has something to say, and a stack trace is not a result. The
   * failure is screenshotted, because the state that produced it is gone the
   * moment the next check runs.
   */
  async check(name, body) {
    const started = Date.now();
    let result;
    try {
      const value = await body();
      result =
        typeof value === 'boolean'
          ? { ok: value }
          : value && typeof value === 'object'
            ? { ok: Boolean(value.ok), detail: value.detail }
            : { ok: Boolean(value) };
    } catch (error) {
      result = { ok: false, detail: error.message };
    }
    const entry = { name, ok: result.ok, detail: result.detail, ms: Date.now() - started };
    this.checks.push(entry);
    if (!result.ok) await this.shot(`fail-${slug(name)}`).catch(() => {});
    if (!this.json) {
      console.log(`${entry.ok ? 'ok  ' : 'FAIL'}  ${name}${entry.ms > 400 ? `  (${entry.ms}ms)` : ''}`);
      if (!entry.ok && entry.detail) console.log(`        ${entry.detail}`);
    }
    return entry.ok;
  }

  async shot(name) {
    if (!this.shotDir) return null;
    await mkdir(this.shotDir, { recursive: true });
    const path = join(this.shotDir, `${name}.png`);
    await writeFile(path, await this.cdp.screenshot());
    this.shots.push(path);
    return path;
  }
}

const slug = (text) =>
  text
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-|-$/g, '')
    .slice(0, 60);

/* ---------------------------------------------------------------- *
 * Suites
 * ---------------------------------------------------------------- */

/**
 * Does the input layer work?
 *
 * Every other suite is worthless if these fail, and a harness that silently
 * clicks nothing reports a clean run. So the primitives are proven against the
 * real window first: a click has to focus, a key has to insert, a drag has to
 * move something.
 */
async function selftest(probe) {
  const { cdp, input } = probe;

  await probe.check('the composer takes focus from a real click', async () => {
    await clickSelector(cdp, input, 'textarea.vd-input');
    const focused = await cdp.evaluate(
      `document.activeElement === document.querySelector('textarea.vd-input')`,
    );
    return { ok: focused, detail: focused ? undefined : 'the click did not focus the textarea' };
  });

  await probe.check('a real key press inserts a character', async () => {
    await input.clearField();
    await input.typeKeys('ab');
    const value = await composerValue(cdp);
    await input.clearField();
    return { ok: value === 'ab', detail: JSON.stringify(value) };
  });

  await probe.check('insertText inserts a whole string', async () => {
    await input.clearField();
    await input.type('hello world');
    const value = await composerValue(cdp);
    await input.clearField();
    return { ok: value === 'hello world', detail: JSON.stringify(value) };
  });

  await probe.check('Ctrl+A then Backspace empties the field', async () => {
    await input.type('something');
    await input.clearField();
    const value = await composerValue(cdp);
    return { ok: value === '', detail: JSON.stringify(value) };
  });

  await probe.check('a real click opens a drawer', async () => {
    await clickExpr(cdp, input, railButton('Settings'));
    await waitFor(cdp, `Boolean(${DRAWER})`, { label: 'the settings drawer' });
    const title = await drawerTitle(cdp);
    return { ok: title === 'Settings', detail: String(title) };
  });

  await probe.check('a real click closes it again', async () => {
    const closed = await closeDrawer(cdp, input);
    return { ok: closed, detail: closed ? undefined : 'no close button was reachable' };
  });

  await probe.check('a real drag moves the rail edge', async () => {
    await ensureRailVisible(cdp, input);
    const handle = await waitDragStart(cdp, SEPARATOR);
    if (!handle) return { ok: false, detail: 'no resize handle' };
    const before = await railWidth(cdp);
    // The handle is the sidebar's left edge, so dragging left widens it.
    await input.drag({ x: handle.cx, y: handle.cy }, { x: handle.cx - 60, y: handle.cy });
    await sleep(400);
    const after = await railWidth(cdp);
    const restored = await setRailWidth(cdp, input, before);
    return {
      ok: after > before && Math.abs(restored - before) < 1,
      detail: `${before} -> ${after}, restored to ${restored}`,
    };
  });

  await probe.check('the wheel scrolls the drawer body', async () => {
    // The Settings directory fits in a full-height window, so a wheel over it
    // has nothing to move and the check would pass or fail on the window size.
    // Shrinking the window first makes the overflow real.
    await cdp.send('Emulation.setDeviceMetricsOverride', {
      width: 900,
      height: 360,
      deviceScaleFactor: 1,
      mobile: false,
    });
    await sleep(500);
    await clickExpr(cdp, input, railButton('Settings'));
    await waitFor(cdp, `Boolean(${DRAWER})`, { label: 'the settings drawer' });
    const body = await waitStable(cdp, `${DRAWER}?.querySelector(':scope > div')`);
    if (!body) return { ok: false, detail: 'no drawer body' };
    const metrics = await cdp.evaluate(`(() => {
      const el = (${DRAWER})?.querySelector(':scope > div');
      return { scrollHeight: el.scrollHeight, clientHeight: el.clientHeight, top: el.scrollTop };
    })()`);
    await input.wheel(body.cx, body.cy, 400);
    await sleep(500);
    const after = await cdp.evaluate(`(${DRAWER})?.querySelector(':scope > div')?.scrollTop ?? 0`);
    await closeDrawer(cdp, input);
    await cdp.send('Emulation.clearDeviceMetricsOverride');
    await sleep(500);
    return {
      ok: metrics.scrollHeight > metrics.clientHeight && after > metrics.top,
      detail: `${JSON.stringify(metrics)} -> scrollTop ${after}`,
    };
  });
}

/** The capability drawers, by the command that opens each one. */
const CAPABILITY_COMMANDS = [
  ['/capabilities', 'Capabilities'],
  ['/compaction', 'Context compaction'],
  ['/terminal', 'Terminal compaction'],
  ['/reading', 'Sparse reading'],
  ['/memory', 'Reflective memory'],
  ['/code-graph', 'Code graph'],
  ['/web', 'Web search and read'],
];

/**
 * Someone who knows the app.
 *
 * Every capability drawer, opened from the keyboard, checked for the things a
 * drawer has to have: a title, sections, controls with names, and no leaked
 * `undefined`. Then a settings round-trip that has to survive the trip through
 * Rust, and a keyboard-only path from the composer to a drawer and back.
 */
async function expert(probe) {
  const { cdp, input } = probe;

  for (const [command, expected] of CAPABILITY_COMMANDS) {
    await probe.check(`${command} opens its drawer`, async () => {
      await runCommand(cdp, input, command);
      await waitFor(cdp, `Boolean(${DRAWER})`, { label: `the ${command} drawer` });
      const title = await drawerTitle(cdp);
      return { ok: title === expected, detail: `title is ${JSON.stringify(title)}` };
    });

    await probe.check(`${command} renders sections and no leaked values`, async () => {
      const text = await drawerText(cdp);
      if (!text) return { ok: false, detail: 'the drawer has no text' };
      const leaked = ['undefined', 'NaN', 'Infinity', '[object Object]'].filter((bad) =>
        text.includes(bad),
      );
      const sections = await cdp.evaluate(
        `(${DRAWER})?.querySelectorAll('section').length ?? 0`,
      );
      return {
        ok: leaked.length === 0 && sections >= 1,
        detail: `sections=${sections} leaked=${leaked.join(',') || 'none'}`,
      };
    });

    await probe.check(`${command} names every control it shows`, async () => {
      // A control's accessible name can come from its own attributes, its text,
      // or a wrapping `<label>`. Checking only the first two reports every
      // labelled numeric field as unnamed, which is a harness bug wearing an
      // accessibility bug's clothes.
      const unnamed = await cdp.evaluate(`(() => {
        const drawer = ${DRAWER};
        if (!drawer) return ['no drawer'];
        const nameOf = (el) => {
          const own =
            el.getAttribute('title') ??
            el.getAttribute('aria-label') ??
            el.getAttribute('placeholder') ??
            el.textContent?.trim() ??
            '';
          if (own) return own;
          const label = el.closest('label');
          if (label) return label.textContent?.trim() ?? '';
          const labelled = el.getAttribute('aria-labelledby');
          if (labelled) return document.getElementById(labelled)?.textContent ?? '';
          return '';
        };
        return [...drawer.querySelectorAll('button, input, select, [role="switch"], [role="tab"]')]
          .filter((el) => nameOf(el).length === 0)
          .map((el) => el.tagName + '.' + String(el.className).slice(0, 30));
      })()`);
      return {
        ok: unnamed.length === 0,
        detail: unnamed.slice(0, 5).join(' | '),
      };
    });

    await probe.check(`${command} closes on Escape`, async () => {
      await input.key('Escape');
      await waitFor(cdp, `!(${DRAWER})`, { label: 'the drawer to close', timeoutMs: 5000 });
      return true;
    });
  }

  await probe.check('a gate survives a round trip through Rust', async () => {
    await runCommand(cdp, input, '/compaction');
    await waitFor(cdp, `Boolean(${DRAWER})`, { label: 'the compaction drawer' });

    const original = await cdp.evaluate(
      `window.__TAURI_INTERNALS__.invoke('capabilities_settings_get', {}).then((s) => s.compaction.minCompressRange)`,
    );

    // Click the field, select what is there, type over it, commit with Enter.
    const fieldExpr = `[...(${DRAWER})?.querySelectorAll('label') ?? []].find((l) => l.textContent.includes('compress at'))?.querySelector('input')`;
    await scrollTo(cdp, fieldExpr);
    const field = await waitClickable(cdp, fieldExpr);
    if (!field) return { ok: false, detail: 'no compress-at field' };
    await input.click(field.cx, field.cy);
    await input.clearField();
    await input.type('4321');
    await input.key('Enter');
    await sleep(600);

    const stored = await cdp.evaluate(
      `window.__TAURI_INTERNALS__.invoke('capabilities_settings_get', {}).then((s) => s.compaction.minCompressRange)`,
    );

    // Restore through the same control, so the file and the store agree.
    await scrollTo(cdp, fieldExpr);
    const again = await waitClickable(cdp, fieldExpr);
    await input.click(again.cx, again.cy);
    await input.clearField();
    await input.type(String(original));
    await input.key('Enter');
    await sleep(600);
    const restored = await cdp.evaluate(
      `window.__TAURI_INTERNALS__.invoke('capabilities_settings_get', {}).then((s) => s.compaction.minCompressRange)`,
    );

    await closeDrawer(cdp, input);
    return {
      ok: stored === 4321 && restored === original,
      detail: `typed 4321 -> ${stored}, restored ${original} -> ${restored}`,
    };
  });

  await probe.check('the keyboard alone reaches a drawer and comes back', async () => {
    await clickSelector(cdp, input, 'textarea.vd-input');
    // Walk the focus ring until something in the rail has focus. Backwards
    // first: the rail sits before the composer in DOM order, so Shift+Tab is
    // the short way round and the one a keyboard user would take. Forwards is
    // the fallback, and it needs a generous bound — a long conversation puts
    // hundreds of focusable rows between the composer and the rail, so a fixed
    // small count fails on a busy session and passes on an empty one.
    const focused = `(() => {
      const el = document.activeElement;
      if (!el) return null;
      return { tag: el.tagName, title: el.getAttribute('title'), role: el.getAttribute('role') };
    })()`;
    let reached = false;
    for (const [modifiers, presses] of [[SHIFT, 60], [0, 200]]) {
      for (let press = 0; press < presses; press += 1) {
        await input.key('Tab', { modifiers });
        const where = await cdp.evaluate(focused);
        if (where?.title === 'Settings') {
          reached = true;
          break;
        }
      }
      if (reached) break;
    }
    if (!reached) return { ok: false, detail: 'Tab never reached the Settings button' };

    await input.key('Enter');
    await waitFor(cdp, `Boolean(${DRAWER})`, { label: 'the drawer from the keyboard' });
    const title = await drawerTitle(cdp);
    await input.key('Escape');
    await waitFor(cdp, `!(${DRAWER})`, { label: 'Escape to close it' });
    // Focus has to land somewhere a keyboard user can carry on from. The button
    // that opened the drawer is the right place; the body is not, because the
    // next Tab would start from the top of the window again.
    const focus = await cdp.evaluate(`(() => {
      const el = document.activeElement;
      return {
        tag: el?.tagName ?? null,
        title: el?.getAttribute?.('title') ?? null,
        isBody: el === document.body,
      };
    })()`);
    return {
      ok: title === 'Settings' && !focus.isBody,
      detail: `title=${title} focus=${JSON.stringify(focus)}`,
    };
  });

  await probe.check('Escape closes a drawer while the composer has focus', async () => {
    // The composer is where focus usually is. A drawer that only answers Escape
    // when focus is elsewhere is a drawer the keyboard cannot dismiss.
    await clickExpr(cdp, input, railButton('Settings'));
    await waitFor(cdp, `Boolean(${DRAWER})`, { label: 'the drawer' });
    await clickSelector(cdp, input, 'textarea.vd-input');
    const focused = await cdp.evaluate(
      `document.activeElement === document.querySelector('textarea.vd-input')`,
    );
    await input.key('Escape');
    await waitFor(cdp, `!(${DRAWER})`, { label: 'Escape to close it', timeoutMs: 5000 });
    return { ok: focused, detail: `composerFocused=${focused}` };
  });

  await probe.check('Escape inside a drawer field leaves the field, not the drawer', async () => {
    // The guard that keeps Escape from closing the panel while a field inside it
    // has focus is deliberate: Escape there means "leave this input".
    await runCommand(cdp, input, '/compaction');
    await waitFor(cdp, `Boolean(${DRAWER})`, { label: 'the compaction drawer' });
    const firstInput = `[...(${DRAWER})?.querySelectorAll('input') ?? []][0]`;
    await scrollTo(cdp, firstInput);
    const field = await waitClickable(cdp, firstInput);
    if (!field) return { ok: false, detail: 'no input in the drawer' };
    await input.click(field.cx, field.cy);
    await input.key('Escape');
    await sleep(400);
    const stillOpen = await cdp.evaluate(`Boolean(${DRAWER})`);
    await closeDrawer(cdp, input);
    return { ok: stillOpen, detail: `drawer still open: ${stillOpen}` };
  });

  await probe.check('the palette filters as you type', async () => {
    await clickSelector(cdp, input, 'textarea.vd-input');
    await input.clearField();
    await input.type('/');
    await waitFor(cdp, `Boolean(document.querySelector('[role=listbox]'))`, {
      label: 'the palette',
    });
    const all = await cdp.evaluate(`document.querySelectorAll('[role=option]').length`);
    await input.type('comp');
    await sleep(300);
    const options = await cdp.evaluate(
      `[...document.querySelectorAll('[role=option]')].map((o) => o.innerText.split('\\n')[0])`,
    );
    await input.clearField();
    await sleep(200);
    // The filter matches descriptions as well as names, so a row whose name
    // lacks the query is not a failure — "high-complexity" and "competing
    // constraints" both match "comp". What has to hold is that typing narrows
    // the list and the command being typed is still in it.
    return {
      ok:
        options.length > 0 &&
        options.length < all &&
        options.some((text) => text.toLowerCase().includes('compaction')),
      detail: `${all} -> ${options.length}: ${JSON.stringify(options.slice(0, 6))}`,
    };
  });
}

/**
 * Someone who does not know the app.
 *
 * The question is not whether the features work — the expert suite answers
 * that. It is whether a person who has never seen this window can find them,
 * and whether every empty state tells them what to do next.
 */
async function novice(probe) {
  const { cdp, input } = probe;

  await probe.check('the composer says what it is for', async () => {
    const placeholder = await cdp.evaluate(
      `document.querySelector('textarea.vd-input')?.getAttribute('placeholder') ?? ''`,
    );
    return { ok: placeholder.length > 8, detail: JSON.stringify(placeholder) };
  });

  await probe.check('every rail button explains itself on hover', async () => {
    const untitled = await cdp.evaluate(`(() => {
      return [...document.querySelectorAll('button[aria-pressed]')]
        .filter((b) => !b.getAttribute('title') && !b.getAttribute('aria-label'))
        .map((b) => b.className.slice(0, 40));
    })()`);
    return { ok: untitled.length === 0, detail: untitled.join(' | ') };
  });

  await probe.check('every palette command explains itself', async () => {
    await clickSelector(cdp, input, 'textarea.vd-input');
    await input.clearField();
    await input.type('/');
    await waitFor(cdp, `Boolean(document.querySelector('[role=listbox]'))`, {
      label: 'the palette',
    });
    const bare = await cdp.evaluate(`(() => {
      return [...document.querySelectorAll('[role=option]')]
        .filter((o) => o.innerText.split('\\n').length < 2)
        .map((o) => o.innerText.slice(0, 30));
    })()`);
    await input.clearField();
    await sleep(200);
    return { ok: bare.length === 0, detail: bare.slice(0, 5).join(' | ') };
  });

  await probe.check('/capabilities lists all six capabilities', async () => {
    await runCommand(cdp, input, '/capabilities');
    await waitFor(cdp, `Boolean(${DRAWER})`, { label: 'the capabilities drawer' });
    const text = ((await drawerText(cdp)) ?? '').toLowerCase();
    const missing = [
      'context compaction',
      'terminal compaction',
      'sparse reading',
      'reflective memory',
      'code graph',
      'web search and read',
    ].filter((name) => !text.includes(name));
    await closeDrawer(cdp, input);
    return { ok: missing.length === 0, detail: `missing ${missing.join(', ')}` };
  });

  await probe.check('a fresh conversation tells you what to do', async () => {
    await runCommand(cdp, input, '/clear');
    await sleep(700);
    const text = await cdp.evaluate(`document.body.innerText`);
    const instructive = /ask|type|start|build|prompt/i.test(text);
    return { ok: instructive, detail: text.slice(0, 160).replace(/\n/g, ' ') };
  });

  await probe.check('clicking the same rail button twice is not a dead end', async () => {
    await clickExpr(cdp, input, railButton('Settings'));
    await waitFor(cdp, `Boolean(${DRAWER})`, { label: 'the drawer' });
    const first = await drawerTitle(cdp);
    await clickExpr(cdp, input, railButton('Settings'));
    await sleep(500);
    const second = await drawerTitle(cdp);
    // Either it closed, or it is still the same drawer. What it must not be is
    // a different drawer, or a blank panel.
    const ok = second === null || second === first;
    if (second !== null) await closeDrawer(cdp, input);
    return { ok, detail: `${first} -> ${second}` };
  });

  await probe.check('no enabled control in a drawer is a dead end', async () => {
    await runCommand(cdp, input, '/compaction');
    await waitFor(cdp, `Boolean(${DRAWER})`, { label: 'the compaction drawer' });
    const dead = await cdp.evaluate(`(() => {
      const drawer = ${DRAWER};
      if (!drawer) return ['no drawer'];
      return [...drawer.querySelectorAll('button')]
        .filter((b) => !b.disabled)
        .filter((b) => {
          const name = b.getAttribute('title') ?? b.textContent?.trim() ?? '';
          return name.length === 0;
        })
        .map((b) => b.className.slice(0, 40));
    })()`);
    await closeDrawer(cdp, input);
    return { ok: dead.length === 0, detail: dead.slice(0, 5).join(' | ') };
  });
}

/**
 * Someone trying to break it.
 *
 * Every gesture here is one a person can produce by accident: a double click
 * where a single was meant, a drag that runs off the edge of the window, an
 * Escape in the middle of a drag, a paste of something enormous. The app has to
 * end in a state it can be used from, not merely avoid throwing.
 */
async function adversarial(probe) {
  const { cdp, input } = probe;

  await probe.check('a triple click on a rail toggle leaves a usable window', async () => {
    const box = await waitClickable(cdp, railButton('Settings'));
    await input.click(box.cx, box.cy);
    await input.click(box.cx, box.cy);
    await input.click(box.cx, box.cy);
    await sleep(700);
    const state = await cdp.evaluate(`({
      drawer: Boolean(${DRAWER}),
      title: (${DRAWER})?.querySelector('h2')?.textContent ?? null,
      rail: document.querySelectorAll('button[aria-pressed]').length,
    })`);
    // Whatever it settled on, the rail has to still be there and the drawer has
    // to be either fully open or fully closed.
    const ok = state.rail >= 2 && (state.drawer === false || state.title === 'Settings');
    if (state.drawer) await closeDrawer(cdp, input);
    return { ok, detail: JSON.stringify(state) };
  });

  await probe.check('a double click on Close panel closes it once', async () => {
    await clickExpr(cdp, input, railButton('Settings'));
    await waitFor(cdp, `Boolean(${DRAWER})`, { label: 'the drawer' });
    const close = await waitClickable(cdp, `${DRAWER}?.querySelector('[title="Close panel"]')`);
    await input.doubleClick(close.cx, close.cy);
    await sleep(600);
    const open = await cdp.evaluate(`Boolean(${DRAWER})`);
    return { ok: open === false, detail: `drawer still open: ${open}` };
  });

  await probe.check('dragging the rail edge past the left limit clamps it', async () => {
    await ensureRailVisible(cdp, input);
    const handle = await waitStable(cdp, SEPARATOR);
    if (!handle) return { ok: false, detail: 'no resize handle' };
    const before = await railWidth(cdp);
    await input.drag({ x: handle.cx, y: handle.cy }, { x: -400, y: handle.cy });
    await sleep(500);
    const after = await railWidth(cdp);
    const overflow = await cdp.evaluate(
      `document.documentElement.scrollWidth <= window.innerWidth + 1`,
    );
    const restored = await setRailWidth(cdp, input, before);
    return {
      ok: after >= 200 && after <= 520 && overflow && Math.abs(restored - before) < 1,
      detail: `${before} -> ${after} (restored ${restored}), noOverflow=${overflow}`,
    };
  });

  await probe.check('dragging the rail edge past the right limit clamps it', async () => {
    await ensureRailVisible(cdp, input);
    const handle = await waitStable(cdp, SEPARATOR);
    if (!handle) return { ok: false, detail: 'no resize handle' };
    const before = await railWidth(cdp);
    await input.drag({ x: handle.cx, y: handle.cy }, { x: 4000, y: handle.cy });
    await sleep(500);
    const after = await railWidth(cdp);
    const overflow = await cdp.evaluate(
      `document.documentElement.scrollWidth <= window.innerWidth + 1`,
    );
    const restored = await setRailWidth(cdp, input, before);
    return {
      ok: after >= 200 && after <= 520 && overflow && Math.abs(restored - before) < 1,
      detail: `${before} -> ${after} (restored ${restored}), noOverflow=${overflow}`,
    };
  });

  await probe.check('Escape in the middle of a drag does not wedge the pointer', async () => {
    await ensureRailVisible(cdp, input);
    const handle = await waitStable(cdp, SEPARATOR);
    if (!handle) return { ok: false, detail: 'no resize handle' };
    const before = await railWidth(cdp);
    await input.dragStart({ x: handle.cx, y: handle.cy });
    await input.dragTo({ x: handle.cx - 40, y: handle.cy });
    await input.key('Escape');
    await input.dragEnd({ x: handle.cx - 40, y: handle.cy });
    await sleep(500);
    // The next gesture has to work. A drag that never released leaves the app
    // tracking a pointer that is no longer down.
    const restored = await setRailWidth(cdp, input, before);
    const usable = await cdp.evaluate(`Boolean(${railButton('Settings')})`);
    return {
      ok: usable && Math.abs(restored - before) < 1,
      detail: `width ${before} -> ${await railWidth(cdp)}, restored ${restored}`,
    };
  });

  await probe.check('Enter on an empty composer sends nothing', async () => {
    await clickSelector(cdp, input, 'textarea.vd-input');
    await input.clearField();
    const before = await cdp.evaluate(`document.querySelectorAll('.vd-row').length`);
    await input.key('Enter');
    await sleep(600);
    const after = await cdp.evaluate(`document.querySelectorAll('.vd-row').length`);
    const value = await composerValue(cdp);
    return { ok: after === before && value === '', detail: `rows ${before} -> ${after}` };
  });

  await probe.check('a 200k-character paste does not break the composer', async () => {
    await clickSelector(cdp, input, 'textarea.vd-input');
    await input.clearField();
    await input.type('x'.repeat(200_000));
    await sleep(700);
    const state = await cdp.evaluate(`(() => {
      const t = document.querySelector('textarea.vd-input');
      const r = t.getBoundingClientRect();
      return {
        length: t.value.length,
        height: Math.round(r.height),
        overflow: document.documentElement.scrollWidth <= window.innerWidth + 1,
        composerFits: r.bottom <= window.innerHeight + 1,
      };
    })()`);
    await input.clearField();
    await sleep(300);
    return {
      ok: state.length === 200_000 && state.overflow && state.composerFits,
      detail: JSON.stringify(state),
    };
  });

  await probe.check('emoji, right-to-left and combining marks survive', async () => {
    await clickSelector(cdp, input, 'textarea.vd-input');
    await input.clearField();
    const mixed = 'مرحبا 🎉 e\u0301 \u200b 日本語 \u{1F468}\u200D\u{1F469}\u200D\u{1F467}';
    await input.type(mixed);
    await sleep(400);
    const value = await composerValue(cdp);
    const overflow = await cdp.evaluate(
      `document.documentElement.scrollWidth <= window.innerWidth + 1`,
    );
    await input.clearField();
    await sleep(200);
    return { ok: value === mixed && overflow, detail: JSON.stringify(value) };
  });

  await probe.check('a right click on the rail does not break it', async () => {
    const box = await waitClickable(cdp, railButton('Settings'));
    await input.rightClick(box.cx, box.cy);
    await sleep(400);
    const state = await cdp.evaluate(`({
      rail: document.querySelectorAll('button[aria-pressed]').length,
      menu: Boolean(document.querySelector('[role=menu]')),
    })`);
    // A context menu is fine. A missing rail is not.
    if (state.menu) await input.key('Escape');
    return { ok: state.rail >= 2, detail: JSON.stringify(state) };
  });

  await probe.check('wheel spam over a drawer does not overflow the window', async () => {
    await clickExpr(cdp, input, railButton('Settings'));
    await waitFor(cdp, `Boolean(${DRAWER})`, { label: 'the drawer' });
    const body = await waitStable(cdp, `${DRAWER}?.querySelector(':scope > div')`);
    if (!body) return { ok: false, detail: 'no drawer body' };
    for (let burst = 0; burst < 12; burst += 1) {
      await input.wheel(body.cx, body.cy, 600);
      await input.wheel(body.cx, body.cy, -600);
    }
    await sleep(500);
    const overflow = await cdp.evaluate(
      `document.documentElement.scrollWidth <= window.innerWidth + 1`,
    );
    await closeDrawer(cdp, input);
    return { ok: overflow, detail: `noOverflow=${overflow}` };
  });

  await probe.check('typing while a drawer is open goes to the composer', async () => {
    await clickExpr(cdp, input, railButton('Settings'));
    await waitFor(cdp, `Boolean(${DRAWER})`, { label: 'the drawer' });
    await clickSelector(cdp, input, 'textarea.vd-input');
    await input.clearField();
    await input.typeKeys('abc');
    const value = await composerValue(cdp);
    const drawerStillOpen = await cdp.evaluate(`Boolean(${DRAWER})`);
    await input.clearField();
    await closeDrawer(cdp, input);
    return {
      ok: value === 'abc' && drawerStillOpen,
      detail: `composer=${JSON.stringify(value)} drawer=${drawerStillOpen}`,
    };
  });

  await probe.check('the smallest window the app allows still fits', async () => {
    // 940x620 is the floor `tauri.conf.json` sets. Testing narrower than that
    // tests a window the user cannot make, and reports a layout bug that is not
    // reachable.
    await cdp.send('Emulation.setDeviceMetricsOverride', {
      width: 940,
      height: 620,
      deviceScaleFactor: 1,
      mobile: false,
    });
    await sleep(700);
    const state = await cdp.evaluate(`({
      overflow: document.documentElement.scrollWidth <= window.innerWidth + 1,
      rail: document.querySelectorAll('button[aria-pressed]').length,
      composer: Boolean(document.querySelector('textarea.vd-input')),
      size: [window.innerWidth, window.innerHeight],
    })`);
    await cdp.send('Emulation.clearDeviceMetricsOverride');
    await sleep(600);
    return {
      ok: state.overflow && state.rail >= 2 && state.composer,
      detail: JSON.stringify(state),
    };
  });

  await probe.check('the window is usable again after all of that', async () => {
    const state = await cdp.evaluate(`({
      overflow: document.documentElement.scrollWidth <= window.innerWidth + 1,
      rail: document.querySelectorAll('button[aria-pressed]').length,
      composer: Boolean(document.querySelector('textarea.vd-input')),
      drawer: Boolean(${DRAWER}),
      size: [window.innerWidth, window.innerHeight],
    })`);
    return {
      ok: state.overflow && state.rail >= 2 && state.composer,
      detail: JSON.stringify(state),
    };
  });
}

/**
 * A real agent doing a real job, driven through the real UI.
 *
 * The other suites prove the chrome works. This one proves the app works: a
 * person picks a folder, types a request, and an agent writes a program. It is
 * the only check that exercises the whole stack at once — the composer, the ACP
 * session, the tool calls, the transcript, and the capability layer that sits
 * between the agent and its model.
 *
 * It is slow and it costs tokens, so it is not part of `all`. Run it by name.
 */
/**
 * Wait for one agent turn to finish, answering any approval that appears.
 *
 * A harness that waits for a prompt nobody clicks reports a timeout, not the
 * thing it was testing. Three quiet polls with the agent idle is the end of the
 * turn; a row count that keeps moving is not.
 */
async function waitForTurn(cdp, input, { timeoutMs = 20 * 60 * 1000 } = {}) {
  const deadline = Date.now() + timeoutMs;
  let approvals = 0;
  let lastRows = 0;
  let idleTicks = 0;
  // The bar is only live while a call is unsettled, which is a window of a few
  // seconds. Sampling it from the poll that is already running is the only way
  // to catch it without a second loop racing the turn.
  let sawLive = false;

  while (Date.now() < deadline) {
    const state = await cdp.evaluate(`(() => {
      const allow = [...document.querySelectorAll('button')].find((b) =>
        /^(allow|approve|accept|yes)$/i.test((b.textContent ?? '').trim()),
      );
      const send = document.querySelector('button[title*="Queue for when this turn ends"]');
      return {
        allow: allow ? allow.textContent.trim() : null,
        busy: Boolean(send),
        rows: document.querySelectorAll('.vd-row').length,
        live: document.querySelectorAll('[data-vd-tool-run="live"]').length,
      };
    })()`);

    if (state.live > 0) sawLive = true;

    if (state.allow) {
      await clickExpr(
        cdp,
        input,
        `[...document.querySelectorAll('button')].find((b) => /^(allow|approve|accept|yes)$/i.test((b.textContent ?? '').trim()))`,
      );
      approvals += 1;
      await sleep(800);
      continue;
    }

    if (state.rows !== lastRows) {
      lastRows = state.rows;
      idleTicks = 0;
    } else if (!state.busy) {
      idleTicks += 1;
      if (idleTicks >= 3) break;
    }
    await sleep(1500);
  }

  const final = await cdp.evaluate(`(() => ({
    rows: document.querySelectorAll('.vd-row').length,
    busy: Boolean(document.querySelector('button[title*="Queue for when this turn ends"]')),
  }))()`);
  return { approvals, rows: final.rows, busy: final.busy, sawLive };
}

async function game(probe, { workspace = null } = {}) {
  const { cdp, input } = probe;
  const folder = workspace ?? 'C:/Users/Santera/AppData/Local/Temp/vd-game/skate';

  await probe.check('the workspace is selectable from the rail', async () => {
    const row = `[...document.querySelectorAll('button')].find((b) => (b.innerText ?? '').includes('skate'))`;
    const exists = await cdp.evaluate(`Boolean(${row})`);
    if (!exists) return { ok: false, detail: 'the skate workstation is not in the rail' };
    await clickExpr(cdp, input, row);
    await sleep(1500);
    // The agent process keeps its own cwd; the session carries the folder. The
    // top bar is the app's own answer to "which folder am I in", so that is what
    // this reads. `agent_status.cwd` is the process, and it never moves.
    const shown = await cdp.evaluate(
      `[...document.querySelectorAll('button')].map((b) => b.getAttribute('title') ?? '').find((t) => t.toLowerCase().includes('skate')) ?? null`,
    );
    return { ok: Boolean(shown), detail: `top bar shows ${shown}` };
  });

  // A suite that only works on an empty folder is a suite that fails the second
  // time it runs. If the game is already there, this verifies it instead of
  // asking for it again.
  let alreadyBuilt = false;
  try {
    const names = await readdir(folder);
    alreadyBuilt = names.some((name) => name.endsWith('.cpp'));
  } catch {
    alreadyBuilt = false;
  }

  const prompt = [
    'Build a C++ skateboard game in this folder.',
    '',
    'Requirements:',
    '- One self-contained .cpp file, no external libraries, terminal-based with ANSI escape codes.',
    '- A skater runs along a scrolling street, jumps over obstacles with the space bar, scores points for distance, and the run ends on a crash.',
    '- Include a Makefile, a README.md explaining the controls, and a small test that checks the collision logic.',
    '- Write the files first, then try to build it with make and tell me exactly what happened.',
  ].join('\n');

  if (!alreadyBuilt) {
    await probe.check('the request is typed and sent with real keys', async () => {
      await clickSelector(cdp, input, 'textarea.vd-input');
      await input.clearField();
      await input.type(prompt);
      const typed = await composerValue(cdp);
      if (typed !== prompt) return { ok: false, detail: 'the composer did not take the text' };
      await input.key('Enter');
      await sleep(1500);
      const sent = await cdp.evaluate(`document.querySelectorAll('.vd-row').length`);
      return { ok: sent > 0, detail: `${sent} transcript rows` };
    });

  // The turn can take minutes. Poll, and answer any approval that appears.
  const turn = await waitForTurn(cdp, input);
  const approvals = turn.approvals;

  await probe.check('the agent finished its turn', async () => {
    return {
      ok: !turn.busy && turn.rows > 0,
      detail: `${turn.rows} rows, busy=${turn.busy}, approvals=${approvals}`,
    };
  });
  }

  await probe.check('the agent wrote the game to disk', async () => {
    let names = [];
    try {
      names = await readdir(folder);
    } catch (error) {
      return { ok: false, detail: `could not read ${folder}: ${error.message}` };
    }
    const cpp = names.filter((name) => name.endsWith('.cpp') || name.endsWith('.cc'));
    const readme = names.filter((name) => /^readme/i.test(name));
    const makefile = names.filter((name) => /^makefile$/i.test(name));
    let bytes = 0;
    for (const name of cpp) {
      try {
        bytes += (await stat(join(folder, name))).size;
      } catch {
        /* a file that vanished between listing and stat is not a failure */
      }
    }
    return {
      ok: cpp.length > 0 && bytes > 2000 && readme.length > 0 && makefile.length > 0,
      detail: `${names.join(', ')} | cpp=${bytes}B`,
    };
  });

  await probe.check("the game's own tests pass", async () => {
    // The strongest thing this suite can say: the program the agent wrote
    // compiles, runs, and its assertions hold. A file on disk proves nothing on
    // its own.
    const candidates = ['test_collision.exe', 'test_collision', 'skate_test.exe'];
    let binary = null;
    for (const name of candidates) {
      try {
        await stat(join(folder, name));
        binary = name;
        break;
      } catch {
        /* try the next name */
      }
    }
    if (!binary) return { ok: false, detail: 'no test binary was built' };
    const result = await new Promise((resolve) => {
      execFile(
        join(folder, binary),
        [],
        { cwd: folder, timeout: 30000, windowsHide: true },
        (error, stdout, stderr) => resolve({ error, stdout, stderr }),
      );
    });
    const output = `${result.stdout ?? ''}${result.stderr ?? ''}`;
    return {
      ok: !result.error && /all checks passed|ok\b/i.test(output),
      detail: result.error ? result.error.message : output.trim().split('\n').slice(-3).join(' | '),
    };
  });

  await probe.check('the capability layer is switched on', async () => {
    const inv = `window.__TAURI_INTERNALS__.invoke`;
    for (const id of ['compaction', 'memory', 'terminal', 'reading']) {
      await cdp.evaluate(`${inv}('capabilities_set_enabled', { id: ${JSON.stringify(id)}, enabled: true })`);
    }
    await sleep(2000);
    const snap = await cdp.evaluate(`${inv}('capabilities_snapshot', {})`);
    const on = snap.filter((entry) => entry.enabled).map((entry) => entry.id);
    return {
      ok: ['compaction', 'memory', 'terminal', 'reading'].every((id) => on.includes(id)),
      detail: on.join(', '),
    };
  });

  await probe.check('a follow-up turn runs through the proxy', async () => {
    // The first turn happened before the layer was on, so it proves nothing
    // about it. This one is carried by the proxy, which is the only way to know
    // the interception is wired to a live session rather than to a test.
    await clickSelector(cdp, input, 'textarea.vd-input');
    await input.clearField();
    await input.type(
      'Add a persistent high-score table to the game, then rebuild and run the tests again.',
    );
    await input.key('Enter');
    await sleep(1500);
    const followUp = await waitForTurn(cdp, input);
    return {
      ok: !followUp.busy && followUp.sawLive,
      detail: `${followUp.rows} rows, approvals=${followUp.approvals}, live bar ${followUp.sawLive ? 'seen' : 'never seen'}`,
    };
  });

  await probe.check('the transcript folds the tool calls into one bar', async () => {
    // The calls are collapsed by default, so the bar is what the transcript
    // shows and the cards only exist once it is opened. Counting cards without
    // opening it would pass on a transcript that rendered nothing.
    const bars = await cdp.evaluate(`document.querySelectorAll('[data-vd-tool-run]').length`);
    if (bars === 0) return { ok: false, detail: 'no tool run in the transcript' };
    const states = await cdp.evaluate(
      `[...document.querySelectorAll('[data-vd-tool-run]')].map((el) => el.dataset.vdToolRun)`,
    );
    // The transcript virtualises off-screen rows, so the first bar in the
    // document can sit thousands of pixels above the viewport and never become
    // clickable. Pick the bar nearest the middle of the window, bring it there,
    // and address it by index so the click and the card count are about the
    // same bar.
    const at = await cdp.evaluate(`(() => {
      const bars = [...document.querySelectorAll('[data-vd-tool-run]')];
      const mid = window.innerHeight / 2;
      let best = 0;
      let distance = Infinity;
      bars.forEach((bar, index) => {
        const r = bar.getBoundingClientRect();
        const d = Math.abs(r.y + r.height / 2 - mid);
        if (d < distance) { distance = d; best = index; }
      });
      bars[best].scrollIntoView({ block: 'center' });
      return best;
    })()`);
    await sleep(400);
    const bar = `document.querySelectorAll('[data-vd-tool-run]')[${at}]`;
    // Scoped to the bar that gets opened. A bar left open by an earlier check
    // would otherwise make the collapsed count pass for the wrong reason.
    const inside = `(() => {
      const body = (${bar}).parentElement.querySelector('.vd-expand');
      return body ? [...body.querySelectorAll('[data-vd-tool]')] : [];
    })()`;
    // A bar that has been opened once keeps its cards mounted at zero height:
    // `.vd-expand` animates `grid-template-rows` from 0fr to 1fr rather than
    // unmounting, so counting nodes reports "open" for a bar the user sees
    // closed. Count the cards that have a box, and close the bar first if an
    // earlier check left it open.
    if (await cdp.evaluate(`(${bar}).getAttribute('aria-expanded') === 'true'`)) {
      await clickExpr(cdp, input, bar);
      await sleep(600);
    }
    const collapsed = await cdp.evaluate(
      `${inside}.filter((el) => el.getBoundingClientRect().height > 0).length`,
    );
    await clickExpr(cdp, input, bar);
    await sleep(600);
    const kinds = await cdp.evaluate(`[...new Set(${inside}.map((el) => el.dataset.vdTool))]`);
    const statuses = await cdp.evaluate(
      `[...new Set(${inside}.map((el) => el.dataset.vdToolStatus))]`,
    );
    const shown = await cdp.evaluate(
      `${inside}.filter((el) => el.getBoundingClientRect().height > 0).length`,
    );
    return {
      ok:
        collapsed === 0 &&
        shown > 0 &&
        kinds.length > 0 &&
        statuses.some((status) => status !== 'pending'),
      detail: `${bars} bars (${states.join(', ')}), ${collapsed} cards visible while collapsed, opened to ${shown} cards in ${kinds.length} kinds (${kinds.join(', ')})`,
    };
  });

  await probe.check('the capability layer recorded the traffic', async () => {
    const snap = await cdp.evaluate(
      `window.__TAURI_INTERNALS__.invoke('capabilities_snapshot', {})`,
    );
    const stats = snap.map((entry) => ({
      id: entry.id,
      on: entry.enabled,
      calls: entry.stats?.calls ?? 0,
      bytesIn: entry.stats?.bytesIn ?? 0,
      bytesOut: entry.stats?.bytesOut ?? 0,
    }));
    const carrying = stats.filter((entry) => entry.on && entry.calls > 0);
    return {
      ok: carrying.length > 0,
      detail: JSON.stringify(stats),
    };
  });

  await probe.check('the compaction panel shows the conversation', async () => {
    await runCommand(cdp, input, '/compaction');
    await waitFor(cdp, `Boolean(${DRAWER})`, { label: 'the compaction drawer' });
    const text = (await drawerText(cdp)) ?? '';
    const hasSession = /[0-9a-f]{16}/.test(text);
    await probe.shot('game-compaction');
    await closeDrawer(cdp, input);
    return { ok: hasSession, detail: text.slice(0, 200).replace(/\n/g, ' ') };
  });

  await probe.check('the capabilities panel shows what was intercepted', async () => {
    await runCommand(cdp, input, '/capabilities');
    await waitFor(cdp, `Boolean(${DRAWER})`, { label: 'the capabilities drawer' });
    const text = (await drawerText(cdp)) ?? '';
    await probe.shot('game-capabilities');
    await closeDrawer(cdp, input);
    return {
      ok: /kept out of context|on\b/i.test(text),
      detail: text.slice(0, 220).replace(/\n/g, ' '),
    };
  });

  await probe.shot('game-transcript');
}

const SUITES = { selftest, expert, novice, adversarial, game };

/* ---------------------------------------------------------------- *
 * CLI
 * ---------------------------------------------------------------- */

function parseArgs(argv) {
  const args = {
    command: argv[0] ?? 'all',
    port: DEFAULT_PORT,
    shotDir: null,
    json: false,
    workspace: null,
  };
  for (let index = 1; index < argv.length; index += 1) {
    const token = argv[index];
    if (token === '--port') args.port = Number(argv[++index]);
    else if (token === '--shots') args.shotDir = argv[++index];
    else if (token === '--workspace') args.workspace = argv[++index];
    else if (token === '--json') args.json = true;
  }
  return args;
}

/** Collect console errors and uncaught exceptions for the whole run. */
async function watchConsole(cdp) {
  const errors = [];
  await cdp.send('Runtime.enable');
  await cdp.send('Log.enable');
  cdp.on('Runtime.consoleAPICalled', (event) => {
    if (event.type !== 'error') return;
    errors.push(
      (event.args ?? []).map((arg) => arg.value ?? arg.description ?? arg.type).join(' '),
    );
  });
  cdp.on('Runtime.exceptionThrown', (event) => {
    const details = event.exceptionDetails;
    errors.push(details?.exception?.description ?? details?.text ?? 'uncaught exception');
  });
  cdp.on('Log.entryAdded', (event) => {
    if (event.entry.level === 'error') errors.push(event.entry.text);
  });
  return errors;
}

async function main() {
  const args = parseArgs(process.argv.slice(2));

  if (!SUITES[args.command] && args.command !== 'all') {
    console.error(`ui-probe: unknown suite ${args.command}`);
    console.error(`ui-probe: known suites are ${Object.keys(SUITES).join(', ')}, all`);
    return 1;
  }

  let cdp;
  try {
    cdp = await Cdp.attach(args.port);
  } catch (error) {
    console.error(`ui-probe: ${error.message}`);
    console.error('ui-probe: start the app with VIBEDESK_REMOTE_DEBUGGING=9222');
    return 2;
  }

  // A run that hangs is indistinguishable from a run that is broken, so the
  // whole thing is bounded. The `game` suite waits on a real agent, which is
  // minutes rather than seconds, so it gets its own budget.
  const budget = args.command === 'game' ? 30 * 60 * 1000 : 10 * 60 * 1000;
  const watchdog = setTimeout(() => {
    console.error('ui-probe: the run exceeded its watchdog and was stopped');
    process.exit(3);
  }, budget);
  watchdog.unref?.();

  try {
    const errors = await watchConsole(cdp);
    await cdp.send('Page.enable');
    await waitFor(cdp, `(document.querySelector('#root')?.children.length ?? 0) > 0`, {
      label: 'the app to mount',
    });
    await sleep(600);

    const names = args.command === 'all' ? Object.keys(SUITES) : [args.command];
    const probe = new Probe(cdp, { shotDir: args.shotDir, json: args.json });

    // The state the harness has to leave behind.
    const before = await cdp.evaluate(`({
      theme: document.documentElement.dataset.theme,
      motion: document.documentElement.dataset.motion,
      rail: getComputedStyle(document.documentElement).getPropertyValue('--vd-sidebar-w'),
    })`);

    for (const name of names) {
      if (!args.json) console.log(`\n# ${name}`);
      try {
        await SUITES[name](probe, { workspace: args.workspace });
      } catch (error) {
        probe.checks.push({ name: `${name} ran to completion`, ok: false, detail: error.message });
        if (!args.json) console.log(`FAIL  ${name} ran to completion\n        ${error.message}`);
      }
    }

    // Put the window back. A harness that leaves the app in a state the next
    // run has to work around is a harness that fails on its second run.
    await cdp
      .evaluate(`(() => {
        const root = document.documentElement;
        if (${JSON.stringify(before.theme ?? null)}) root.dataset.theme = ${JSON.stringify(before.theme ?? '')};
        if (${JSON.stringify(before.motion ?? null)}) root.dataset.motion = ${JSON.stringify(before.motion ?? '')};
        const t = document.querySelector('textarea.vd-input');
        if (t) {
          const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value').set;
          setter.call(t, '');
          t.dispatchEvent(new Event('input', { bubbles: true }));
        }
        return true;
      })()`)
      .catch(() => {});
    await cdp.send('Emulation.clearDeviceMetricsOverride').catch(() => {});

    const failed = probe.checks.filter((check) => !check.ok);
    const total = probe.checks.length;

    if (args.json) {
      console.log(
        JSON.stringify(
          { checks: probe.checks, errors, shots: probe.shots, total, failed: failed.length },
          null,
          2,
        ),
      );
    } else {
      for (const error of errors) console.log(`ERROR ${error}`);
      console.log(
        `\n${total - failed.length}/${total} checks, ${errors.length} console errors, ${probe.shots.length} screenshots`,
      );
    }

    return failed.length === 0 && errors.length === 0 ? 0 : 1;
  } finally {
    clearTimeout(watchdog);
    cdp.close();
  }
}

process.exit(await main());
