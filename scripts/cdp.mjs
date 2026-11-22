#!/usr/bin/env node
/**
 * Drive the running VibeDesk window the way a user does.
 *
 * The app is a Tauri window, so the only honest way to exercise it is through
 * the webview it actually renders. WebView2 exposes a DevTools endpoint when it
 * is started with `--remote-debugging-port`, and this script speaks that
 * protocol: real mouse events at real coordinates, real key events, DOM reads,
 * and screenshots of what the user would see.
 *
 * Usage:
 *   node scripts/cdp.mjs targets
 *   node scripts/cdp.mjs eval "<expression>"
 *   node scripts/cdp.mjs click "<css selector>" [--index N]
 *   node scripts/cdp.mjs clickxy <x> <y>
 *   node scripts/cdp.mjs type "<text>"
 *   node scripts/cdp.mjs key <Enter|Escape|Tab|Backspace|a>
 *   node scripts/cdp.mjs shot <path.png>
 *   node scripts/cdp.mjs wait "<css selector>" [--timeout ms]
 *   node scripts/cdp.mjs text "<css selector>"
 */

const PORT = process.env.VIBEDESK_CDP_PORT ?? '9222';

async function pageTarget() {
  const res = await fetch(`http://127.0.0.1:${PORT}/json/list`);
  const list = await res.json();
  // `VIBEDESK_CDP_MATCH` picks a target by URL substring. The app is not the only
  // page in the browser once a second one is opened — a game served on
  // localhost, a widget origin — and driving the wrong one silently does nothing.
  const match = process.env.VIBEDESK_CDP_MATCH;
  const pages = list.filter((t) => t.type === 'page' && !t.url.startsWith('devtools://'));
  const page = match ? pages.find((t) => t.url.includes(match)) : pages[0];
  if (!page) {
    throw new Error(
      match
        ? `no page target matching "${match}"; open: ${pages.map((t) => t.url).join(', ')}`
        : 'no page target; is the app running with --remote-debugging-port?',
    );
  }
  return page;
}

class Cdp {
  constructor(ws) {
    this.ws = ws;
    this.id = 0;
    this.pending = new Map();
    ws.addEventListener('message', (event) => {
      const msg = JSON.parse(event.data);
      if (msg.id && this.pending.has(msg.id)) {
        const { resolve, reject } = this.pending.get(msg.id);
        this.pending.delete(msg.id);
        if (msg.error) reject(new Error(`${msg.error.message} (${msg.error.code})`));
        else resolve(msg.result);
      }
    });
  }

  static async open() {
    const target = await pageTarget();
    const ws = new WebSocket(target.webSocketDebuggerUrl);
    await new Promise((resolve, reject) => {
      ws.addEventListener('open', resolve, { once: true });
      ws.addEventListener('error', () => reject(new Error('cdp socket failed')), { once: true });
    });
    return new Cdp(ws);
  }

  send(method, params = {}) {
    const id = ++this.id;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.ws.send(JSON.stringify({ id, method, params }));
      setTimeout(() => {
        if (this.pending.has(id)) {
          this.pending.delete(id);
          reject(new Error(`${method} timed out`));
        }
      }, 30_000);
    });
  }

  async eval(expression) {
    const result = await this.send('Runtime.evaluate', {
      expression,
      awaitPromise: true,
      returnByValue: true,
      userGesture: true,
    });
    if (result.exceptionDetails) {
      const text =
        result.exceptionDetails.exception?.description ??
        result.exceptionDetails.text ??
        'evaluation failed';
      throw new Error(text);
    }
    return result.result.value;
  }

  /** The centre of an element, in CSS pixels, or null when it is not there. */
  async centre(selector, index = 0) {
    return this.eval(`(() => {
      const nodes = document.querySelectorAll(${JSON.stringify(selector)});
      const el = nodes[${index}];
      if (!el) return null;
      el.scrollIntoView({ block: 'center', inline: 'center' });
      const r = el.getBoundingClientRect();
      if (r.width === 0 || r.height === 0) return null;
      return { x: r.left + r.width / 2, y: r.top + r.height / 2, w: r.width, h: r.height, n: nodes.length };
    })()`);
  }

  async mouse(type, x, y, extra = {}) {
    await this.send('Input.dispatchMouseEvent', {
      type,
      x,
      y,
      button: 'left',
      buttons: type === 'mousePressed' ? 1 : 0,
      clickCount: 1,
      ...extra,
    });
  }

  async clickAt(x, y) {
    await this.mouse('mouseMoved', x, y, { button: 'none', buttons: 0 });
    await sleep(40);
    await this.mouse('mousePressed', x, y);
    await sleep(60);
    await this.mouse('mouseReleased', x, y);
  }

  async click(selector, index = 0) {
    const at = await this.centre(selector, index);
    if (!at) throw new Error(`not clickable: ${selector}[${index}]`);
    await this.clickAt(at.x, at.y);
    return at;
  }

  async type(text) {
    for (const ch of text) {
      await this.send('Input.dispatchKeyEvent', { type: 'keyDown', text: ch, key: ch });
      await this.send('Input.dispatchKeyEvent', { type: 'keyUp', key: ch });
      await sleep(12);
    }
  }

  async key(name) {
    const map = {
      Enter: { key: 'Enter', code: 'Enter', windowsVirtualKeyCode: 13, text: '\r' },
      Escape: { key: 'Escape', code: 'Escape', windowsVirtualKeyCode: 27 },
      Tab: { key: 'Tab', code: 'Tab', windowsVirtualKeyCode: 9 },
      Backspace: { key: 'Backspace', code: 'Backspace', windowsVirtualKeyCode: 8 },
      ArrowDown: { key: 'ArrowDown', code: 'ArrowDown', windowsVirtualKeyCode: 40 },
      ArrowUp: { key: 'ArrowUp', code: 'ArrowUp', windowsVirtualKeyCode: 38 },
    };
    const spec = map[name] ?? {
      key: name,
      code: `Key${name.toUpperCase()}`,
      windowsVirtualKeyCode: name.toUpperCase().charCodeAt(0),
      text: name,
    };
    await this.send('Input.dispatchKeyEvent', { type: 'keyDown', ...spec });
    await this.send('Input.dispatchKeyEvent', { type: 'keyUp', ...spec });
  }

  async shot(path) {
    const { data } = await this.send('Page.captureScreenshot', { format: 'png' });
    const { writeFile } = await import('node:fs/promises');
    await writeFile(path, Buffer.from(data, 'base64'));
    return path;
  }

  async waitFor(selector, timeout = 8000) {
    const started = Date.now();
    while (Date.now() - started < timeout) {
      if (await this.eval(`!!document.querySelector(${JSON.stringify(selector)})`)) return true;
      await sleep(120);
    }
    return false;
  }
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function main() {
  const [command, ...rest] = process.argv.slice(2);
  if (!command) {
    console.error('usage: cdp.mjs <targets|eval|click|clickxy|type|key|shot|wait|text> ...');
    process.exit(2);
  }

  if (command === 'targets') {
    const res = await fetch(`http://127.0.0.1:${PORT}/json/list`);
    console.log(JSON.stringify(await res.json(), null, 2));
    return;
  }

  const cdp = await Cdp.open();
  try {
    switch (command) {
      case 'eval': {
        const value = await cdp.eval(rest.join(' '));
        console.log(typeof value === 'string' ? value : JSON.stringify(value, null, 2));
        break;
      }
      case 'click': {
        const index = rest.includes('--index') ? Number(rest[rest.indexOf('--index') + 1]) : 0;
        const selector = rest.filter((a, i) => a !== '--index' && rest[i - 1] !== '--index').join(' ');
        const at = await cdp.click(selector, index);
        console.log(`clicked ${selector}[${index}] at ${Math.round(at.x)},${Math.round(at.y)}`);
        break;
      }
      case 'clickxy': {
        await cdp.clickAt(Number(rest[0]), Number(rest[1]));
        console.log(`clicked ${rest[0]},${rest[1]}`);
        break;
      }
      case 'type': {
        await cdp.type(rest.join(' '));
        console.log('typed');
        break;
      }
      case 'key': {
        await cdp.key(rest[0]);
        console.log(`key ${rest[0]}`);
        break;
      }
      case 'shot': {
        console.log(await cdp.shot(rest[0]));
        break;
      }
      case 'wait': {
        const timeout = rest.includes('--timeout') ? Number(rest[rest.indexOf('--timeout') + 1]) : 8000;
        const selector = rest.filter((a, i) => a !== '--timeout' && rest[i - 1] !== '--timeout').join(' ');
        const found = await cdp.waitFor(selector, timeout);
        console.log(found ? `found ${selector}` : `missing ${selector}`);
        process.exitCode = found ? 0 : 1;
        break;
      }
      case 'text': {
        const value = await cdp.eval(
          `(document.querySelector(${JSON.stringify(rest.join(' '))})?.innerText ?? '').trim()`,
        );
        console.log(value);
        break;
      }
      default:
        console.error(`unknown command ${command}`);
        process.exitCode = 2;
    }
  } finally {
    cdp.ws.close();
  }
}

main().catch((error) => {
  console.error(error.message);
  process.exit(1);
});
