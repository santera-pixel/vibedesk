import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { Cpu, ShieldCheck } from 'lucide-react';
import {
  MAX_WIDGET_PROMPT,
  WIDGET_HEIGHT,
  capabilityName,
  widgetUrl,
  type WidgetCapability,
} from '@/lib/customize';
import { errorMessage, sendPrompt } from '@/lib/actions';
import { workspace } from '@/lib/bridge';
import { runAction } from '@/components/chrome/ActionStrip';
import { useApp } from '@/store/app';
import type { DataModel } from '@/lib/data';

/**
 * Agent- or plugin-authored HTML **and scripts**, in a sandboxed frame on its
 * own origin.
 *
 * This is the block a `sandbox` cannot be. The document is served from the
 * `widget` scheme, which is a different origin from the app, so it carries its
 * own `Content-Security-Policy` instead of inheriting `script-src 'self'`. The
 * app's own policy is untouched; only `frame-src` grew by one origin. See
 * `src-tauri/src/widget.rs` for the server half and for why the platform
 * constraint that stops a `sandbox` block does not apply here.
 *
 * The boundary has three layers, and they are independent on purpose:
 *
 * 1. **The frame.** `sandbox="allow-scripts"` and nothing else. No
 *    `allow-same-origin`, so the document's origin is opaque: no cookies, no
 *    storage, no `parent` access, no navigation of the window it sits in.
 * 2. **The document's own policy.** `default-src 'none'` with
 *    `script-src 'unsafe-inline'` — its own scripts run, the network does not
 *    exist, and no external script, font, image or frame can load.
 * 3. **This bridge.** The only route back into the app, and it is a closed set:
 *    a widget may send a prompt or fire an action **its block declared**, and
 *    nothing else. An undeclared request is refused and the refusal is reported
 *    back to the widget so its author sees it instead of a silent no-op.
 *
 * What the widget receives is the panel's live data model and the live theme
 * tokens, pushed on every change. That is what makes a widget a live surface
 * rather than a picture: it can draw a graph over a source that ticks, and it
 * can drive the agent from a control it drew itself.
 */

/** The theme tokens a widget is given, so it does not have to know the theme. */
const THEME_TOKENS = [
  '--vd-canvas',
  '--vd-canvas-raised',
  '--vd-surface',
  '--vd-surface-hover',
  '--vd-surface-sunken',
  '--vd-line',
  '--vd-line-soft',
  '--vd-line-strong',
  '--vd-text',
  '--vd-text-secondary',
  '--vd-text-tertiary',
  '--vd-accent',
  '--vd-accent-strong',
  '--vd-accent-wash',
  '--vd-warn',
  '--vd-danger',
  '--vd-info',
] as const;

function themeTokens(): Record<string, string> {
  const style = getComputedStyle(document.documentElement);
  const tokens: Record<string, string> = {};
  for (const name of THEME_TOKENS) {
    const value = style.getPropertyValue(name).trim();
    if (value) tokens[name] = value;
  }
  return tokens;
}

/** What a widget may send back. Anything else is ignored. */
interface InvokeMessage {
  type: 'vibedesk:invoke';
  id: number;
  capability: string;
  payload?: { text?: unknown; path?: unknown };
}

interface ResizeMessage {
  type: 'vibedesk:resize';
  height: number;
}

/**
 * How many file reads a widget may make in a window, and how long the window is.
 *
 * A widget that polls a file in a loop is the case this exists for: without a
 * budget it would read the disk as fast as the event loop allows and the user
 * would see the app stall with nothing on screen explaining why. The refusal is
 * reported back to the widget, so its author sees it.
 */
const FILE_READS = 20;
const FILE_WINDOW_MS = 10_000;

/**
 * A path a widget may ask for: relative, inside the workspace, no traversal.
 *
 * The backend resolves against the workspace root but does not refuse an
 * absolute path or a `..` segment, so the check has to be here. A widget is
 * agent-authored HTML; treating its path as trusted would make the `file`
 * capability a read-anything primitive.
 */
function safeRelativePath(path: string): string | null {
  const trimmed = path.trim();
  if (!trimmed || trimmed.length > 400) return null;
  if (trimmed.startsWith('/') || trimmed.startsWith('\\')) return null;
  if (/^[a-zA-Z]:/.test(trimmed)) return null;
  if (trimmed.includes('\0')) return null;
  const parts = trimmed.split(/[\\/]+/);
  if (parts.some((part) => part === '..')) return null;
  return trimmed;
}

export function WidgetBlock({
  html,
  title,
  height,
  capabilities,
  model,
}: {
  html: string;
  title: string | null;
  height: number;
  capabilities: WidgetCapability[];
  model: DataModel;
}) {
  const frame = useRef<HTMLIFrameElement | null>(null);
  const loaded = useRef(false);
  const theme = useApp((state) => state.theme);
  const actionsConfig = useApp((state) => state.actionsConfig);
  const addNotice = useApp((state) => state.addNotice);

  const [box, setBox] = useState(() =>
    Math.min(WIDGET_HEIGHT[1], Math.max(WIDGET_HEIGHT[0], height || 240)),
  );

  // The height the frame is actually at, so a resize message that changes
  // nothing costs nothing. A widget that re-measures on every animation frame
  // sends the same number sixty times a second; without this the parent would
  // re-render sixty times a second to arrive at the height it already had.
  const boxRef = useRef(box);
  boxRef.current = box;
  const pendingHeight = useRef<number | null>(null);
  const resizeFrame = useRef<number | null>(null);

  // Read budget. A widget polling a file in a loop is the case this defends
  // against; the window slides rather than resetting, so a widget cannot burst
  // at the boundary.
  const reads = useRef<number[]>([]);

  const src = useMemo(() => widgetUrl(html), [html]);
  const declared = useMemo(() => capabilities.map(capabilityName), [capabilities]);

  const post = useCallback((message: unknown) => {
    frame.current?.contentWindow?.postMessage(message, '*');
  }, []);

  // The handshake. Sent on load rather than on mount because the frame's own
  // bridge has to be listening, and it is in the document's head — so by the
  // time `load` fires it is. Everything the widget needs to start is in here:
  // what it may do, what the data is now, and what the theme is now.
  const onLoad = useCallback(() => {
    loaded.current = true;
    post({
      type: 'vibedesk:init',
      capabilities: declared,
      data: model,
      theme: themeTokens(),
    });
  }, [declared, model, post]);

  // Live data. Every tick of every source in this panel, and every change to a
  // built-in, arrives here — the same model the declarative blocks read.
  useEffect(() => {
    if (!loaded.current) return;
    post({ type: 'vibedesk:data', data: model });
  }, [model, post]);

  useEffect(() => {
    if (!loaded.current) return;
    post({ type: 'vibedesk:theme', theme: themeTokens() });
    // `theme` is the switch; the tokens are read from the live stylesheet, which
    // changes when the theme does.
  }, [theme, post]);

  useEffect(() => {
    const actions = actionsConfig?.actions ?? [];

    async function handle(message: InvokeMessage) {
      const id = message.id;
      const capability = String(message.capability ?? '');

      const reply = (ok: boolean, error?: string, result?: unknown) =>
        post({ type: 'vibedesk:ack', id, ok, error, result });

      if (!declared.includes(capability)) {
        // Not a user-facing refusal: the panel's author asked for something the
        // panel did not declare, which is a bug in the widget. The widget is
        // told, and the user is told once, because a widget that loops on a
        // refused call would otherwise fill the notice stack.
        reply(false, `this widget did not declare the \`${capability}\` capability`);
        addNotice(
          `A widget in this panel asked for \`${capability}\`, which the panel does not declare.`,
          'warn',
        );
        return;
      }

      if (capability === 'prompt') {
        const text = typeof message.payload?.text === 'string' ? message.payload.text.trim() : '';
        if (!text) {
          reply(false, 'prompt text is empty');
          return;
        }
        if (text.length > MAX_WIDGET_PROMPT) {
          reply(false, `prompt text is longer than ${MAX_WIDGET_PROMPT} characters`);
          return;
        }
        // `sendPrompt` resolves when the *turn* ends, which for a real prompt is
        // minutes. A widget's button cannot wait for that, so the ack means
        // "the app accepted and dispatched this", not "the agent answered".
        // The one case where nothing would be dispatched is checked here, so a
        // widget is never told `ok` for a prompt that went nowhere.
        const state = useApp.getState();
        if (!state.sessionId && !(state.cwd ?? state.environment?.vibeHome)) {
          reply(false, 'no working folder is open, so there is no session to send to');
          return;
        }
        void sendPrompt(text).catch((error: unknown) => {
          addNotice(errorMessage(error), 'error');
        });
        reply(true, undefined, { dispatched: true });
        return;
      }

      if (capability.startsWith('action:')) {
        const actionId = capability.slice('action:'.length);
        const action = actions.find((entry) => entry.id === actionId);
        if (!action) {
          reply(false, `no action \`${actionId}\` is declared in actions.json`);
          return;
        }
        // An action whose `run` is a prompt resolves when the *turn* ends, which
        // is minutes. Same rule as the `prompt` capability: the ack means the
        // app dispatched it. Every other run kind finishes in a tick, so it is
        // awaited and a failure is reported back.
        if (action.run.kind === 'prompt') {
          void runAction(action);
          reply(true, undefined, { dispatched: true });
          return;
        }
        await runAction(action);
        reply(true);
        return;
      }

      if (capability === 'file') {
        const raw = typeof message.payload?.path === 'string' ? message.payload.path : '';
        const path = safeRelativePath(raw);
        if (!path) {
          reply(false, 'a file path must be relative to the workspace and may not climb out of it');
          return;
        }
        const now = Date.now();
        reads.current = reads.current.filter((at) => now - at < FILE_WINDOW_MS);
        if (reads.current.length >= FILE_READS) {
          reply(
            false,
            `this widget has read ${FILE_READS} files in ${FILE_WINDOW_MS / 1000}s; slow the polling down`,
          );
          return;
        }
        reads.current.push(now);
        try {
          const text = await workspace.read(path);
          reply(true, undefined, text);
        } catch (error) {
          reply(false, errorMessage(error));
        }
        return;
      }

      reply(false, `\`${capability}\` is not a capability this build knows`);
    }

    function onMessage(event: MessageEvent) {
      // The frame is cross-origin, so `event.origin` is `"null"` and proves
      // nothing. The source window is the check that matters: a nested frame
      // inside the widget, or another widget on the page, cannot speak for this
      // one.
      if (!frame.current || event.source !== frame.current.contentWindow) return;
      const message = event.data as InvokeMessage | ResizeMessage | null;
      if (!message || typeof message !== 'object') return;

      if (message.type === 'vibedesk:resize') {
        const value = Number(message.height);
        if (!Number.isFinite(value)) return;
        const next = Math.min(WIDGET_HEIGHT[1], Math.max(WIDGET_HEIGHT[0], Math.round(value)));
        if (next === boxRef.current) return;
        // Coalesced to one parent render per frame. A widget that measures
        // itself in a loop would otherwise re-render the whole panel on every
        // message, and the panel is the expensive part.
        pendingHeight.current = next;
        if (resizeFrame.current !== null) return;
        resizeFrame.current = requestAnimationFrame(() => {
          resizeFrame.current = null;
          const queued = pendingHeight.current;
          pendingHeight.current = null;
          if (queued === null || queued === boxRef.current) return;
          boxRef.current = queued;
          setBox(queued);
        });
        return;
      }

      if (message.type === 'vibedesk:invoke') {
        void handle(message);
      }
    }

    window.addEventListener('message', onMessage);
    return () => {
      window.removeEventListener('message', onMessage);
      if (resizeFrame.current !== null) {
        cancelAnimationFrame(resizeFrame.current);
        resizeFrame.current = null;
      }
    };
  }, [actionsConfig, addNotice, declared, post]);

  return (
    <div>
      <div className="mb-1.5 flex items-center gap-1.5">
        <Cpu size={11} strokeWidth={2} className="shrink-0 text-[var(--vd-text-tertiary)]" />
        <span className="min-w-0 flex-1 truncate text-[11px] font-medium text-[var(--vd-text-secondary)]">
          {title ?? 'Widget'}
        </span>
        {declared.length > 0 ? (
          <span
            className="vd-chip !h-[18px] !text-[9.5px]"
            title={`This widget may: ${declared.join(', ')}`}
          >
            {declared.length === 1 ? declared[0] : `${declared.length} capabilities`}
          </span>
        ) : (
          <span className="vd-chip !h-[18px] !text-[9.5px]" title="This widget cannot act on the app">
            read-only
          </span>
        )}
      </div>

      <div className="overflow-hidden rounded-[var(--radius-sm)] border border-[var(--vd-line)]">
        <iframe
          ref={frame}
          src={src}
          title={title ?? 'Widget'}
          // `allow-scripts` and nothing else. Without `allow-same-origin` the
          // document's origin is opaque, so it cannot read the app's storage,
          // cookies or DOM, and cannot navigate this window.
          sandbox="allow-scripts"
          referrerPolicy="no-referrer"
          onLoad={onLoad}
          className="block w-full border-0 bg-[var(--vd-canvas-raised)]"
          style={{ height: box }}
        />
      </div>

      <p className="m-0 mt-1 flex items-center gap-1 text-[10px] text-[var(--vd-text-tertiary)]">
        <ShieldCheck size={9} strokeWidth={2} className="shrink-0" />
        own origin · no network · no access to VibeDesk
        {declared.length > 0 && ` · may ${declared.join(', ')}`}
      </p>
    </div>
  );
}
