/**
 * The one CDP client the live harnesses share.
 *
 * `live-check.mjs` and `ui-probe.mjs` each carried their own copy of this class.
 * The copies had drifted: one guarded against a call made after the socket
 * closed and the other did not, so the same failure hung one harness and failed
 * fast in the other. One copy, one behaviour.
 *
 * This talks to the WebView2 debug port, which only exists in a debug build with
 * `VIBEDESK_REMOTE_DEBUGGING` set. The app's own channeler reaches the same
 * protocol in process, with no port and no token to hand out — but a Node script
 * cannot use it, because the bearer token is derived from the app's process
 * state and never written anywhere. So the harnesses keep the port route and
 * share this client.
 */

/** How long to wait for a page target to appear. */
const ATTACH_TIMEOUT_MS = 15_000;

/** How long a single protocol call may take. */
const CALL_TIMEOUT_MS = 30_000;

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

export class Cdp {
  #ws;
  #next = 1;
  #pending = new Map();
  #listeners = new Map();
  #closed = false;

  constructor(ws) {
    this.#ws = ws;
    ws.onmessage = (event) => this.#receive(JSON.parse(event.data));
    // A window that goes away mid-run has to fail the call in flight. Leaving
    // the promise pending hangs the whole run, and a harness that hangs looks
    // exactly like a harness that is broken.
    ws.onclose = () => this.#fail(new Error('the app closed the connection'));
    ws.onerror = () => this.#fail(new Error('the app dropped the connection'));
  }

  #fail(error) {
    this.#closed = true;
    for (const entry of this.#pending.values()) entry.reject(error);
    this.#pending.clear();
  }

  static async attach(port, { timeoutMs = ATTACH_TIMEOUT_MS } = {}) {
    const deadline = Date.now() + timeoutMs;
    let lastError = 'no target';

    while (Date.now() < deadline) {
      try {
        const response = await fetch(`http://127.0.0.1:${port}/json/list`);
        const targets = await response.json();
        // The webview is the only page target; anything else is a worker.
        const page = targets.find((target) => target.type === 'page');
        if (page?.webSocketDebuggerUrl) {
          const ws = new WebSocket(page.webSocketDebuggerUrl);
          await new Promise((resolve, reject) => {
            ws.onopen = resolve;
            ws.onerror = () => reject(new Error('websocket refused'));
          });
          return new Cdp(ws);
        }
        lastError = 'no page target';
      } catch (error) {
        lastError = error.message;
      }
      await sleep(250);
    }

    throw new Error(`could not reach CDP on 127.0.0.1:${port} (${lastError})`);
  }

  #receive(message) {
    if (message.id !== undefined) {
      const entry = this.#pending.get(message.id);
      if (!entry) return;
      this.#pending.delete(message.id);
      if (message.error) entry.reject(new Error(message.error.message));
      else entry.resolve(message.result);
      return;
    }
    const handlers = this.#listeners.get(message.method);
    if (handlers) for (const handler of handlers) handler(message.params);
  }

  send(method, params = {}, { timeoutMs = CALL_TIMEOUT_MS } = {}) {
    if (this.#closed) {
      return Promise.reject(new Error('the connection is closed'));
    }
    const id = this.#next++;
    return new Promise((resolve, reject) => {
      // A dead page leaves the socket open and the call unanswered, so the
      // close handler alone is not enough to keep a run from hanging.
      const timer = setTimeout(() => {
        this.#pending.delete(id);
        reject(new Error(`${method} did not answer within ${timeoutMs}ms`));
      }, timeoutMs);
      this.#pending.set(id, {
        resolve: (value) => {
          clearTimeout(timer);
          resolve(value);
        },
        reject: (error) => {
          clearTimeout(timer);
          reject(error);
        },
      });
      this.#ws.send(JSON.stringify({ id, method, params }));
    });
  }

  on(method, handler) {
    const handlers = this.#listeners.get(method) ?? [];
    handlers.push(handler);
    this.#listeners.set(method, handlers);
  }

  close() {
    this.#ws.close();
  }

  /** Evaluate an expression in the page and return its value. */
  async evaluate(expression) {
    const result = await this.send('Runtime.evaluate', {
      expression,
      returnByValue: true,
      awaitPromise: true,
      userGesture: true,
    });
    if (result.exceptionDetails) {
      const text =
        result.exceptionDetails.exception?.description ??
        result.exceptionDetails.text ??
        'evaluation threw';
      throw new Error(text);
    }
    return result.result.value;
  }

  async screenshot() {
    const { data } = await this.send('Page.captureScreenshot', { format: 'png' });
    return Buffer.from(data, 'base64');
  }
}
