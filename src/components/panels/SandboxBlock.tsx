import { useMemo } from 'react';
import { ShieldCheck } from 'lucide-react';

/**
 * Agent- or plugin-authored HTML, rendered in an opaque-origin iframe.
 *
 * The block is the only one whose content is markup rather than data, so it is
 * the only one that needs a boundary. There are three, and they are independent
 * on purpose — any one alone would stop the obvious attacks, and the point is
 * that a mistake in one does not open the door:
 *
 * 1. **The sandbox attribute.** Empty, which is the strictest value: no scripts,
 *    no forms, no popups, no top navigation, no same-origin. The document gets an
 *    opaque origin, so it cannot reach `parent`, cannot read the app's cookies or
 *    storage, and cannot navigate the window it sits in.
 * 2. **A CSP injected into the document.** `default-src 'none'` means no fetch,
 *    no XHR, no WebSocket, no external script, font or image. Only inline style
 *    and `data:` images.
 * 3. **Rust validation.** `panels.rs` refuses the common mistakes at write time,
 *    so the agent gets a message in its receipt instead of a blank box.
 *
 * There are no scripts, and that is a platform fact rather than a choice made
 * here. A `srcdoc`, `blob:` and `data:` document all inherit the *parent's* CSP
 * in Chromium — verified against this build, all three — and the app's policy is
 * `script-src 'self'`. A frame cannot be given a looser policy than the app
 * without loosening the app, and `'unsafe-inline'` on the whole app to enable a
 * widget is not a trade worth making. What is left — styled cards, tables, SVG,
 * CSS animation — is what a panel needs. A widget that has to *do* something
 * uses an `actions` block, which is declarative and already validated.
 */

/** The policy injected into every sandbox document. */
const CSP = [
  "default-src 'none'",
  "style-src 'unsafe-inline'",
  "script-src 'none'",
  'img-src data:',
  'font-src data:',
  "connect-src 'none'",
  "form-action 'none'",
  "base-uri 'none'",
].join('; ');

/** Height bounds, mirroring `SANDBOX_HEIGHT` in `panels.rs`. */
const MIN_HEIGHT = 80;
const MAX_HEIGHT = 480;

function token(name: string, fallback: string): string {
  const value = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  return value || fallback;
}

/**
 * Wrap the block's markup in a document that carries the policy and enough base
 * styling to look like it belongs in the panel. The palette is read from the
 * live theme, so a widget does not have to know which theme is active.
 */
function documentFor(html: string): string {
  const text = token('--vd-text', '#e8e2da');
  const muted = token('--vd-text-secondary', '#a8a29a');
  const accent = token('--vd-accent', '#ff7a1a');
  const surface = token('--vd-canvas-raised', '#141210');
  const line = token('--vd-line', '#2a2622');
  return [
    '<!doctype html><html><head><meta charset="utf-8">',
    `<meta http-equiv="Content-Security-Policy" content="${CSP}">`,
    '<style>',
    `:root{color-scheme:dark;--vd-text:${text};--vd-text-secondary:${muted};--vd-accent:${accent};--vd-line:${line}}`,
    `html,body{margin:0;padding:0;background:${surface};color:${text};`,
    'font:12px/1.55 ui-sans-serif,system-ui,-apple-system,"Segoe UI",sans-serif;',
    '-webkit-font-smoothing:antialiased}',
    '*,*::before,*::after{box-sizing:border-box}',
    'a{color:var(--vd-accent)}',
    'table{border-collapse:collapse;width:100%}',
    'th,td{text-align:left;padding:3px 6px;border-bottom:1px solid var(--vd-line)}',
    '</style></head><body>',
    html,
    '</body></html>',
  ].join('');
}

export function SandboxBlock({
  html,
  title,
  height,
}: {
  html: string;
  title: string | null;
  height: number;
}) {
  const box = Math.min(MAX_HEIGHT, Math.max(MIN_HEIGHT, height || 160));

  /**
   * The document, as a `data:` URL. Rebuilt when the markup changes, which is
   * what makes a hot-reloaded panel show the new widget.
   */
  const src = useMemo(
    () => `data:text/html;charset=utf-8,${encodeURIComponent(documentFor(html))}`,
    [html],
  );

  return (
    <div>
      <div className="mb-1.5 flex items-center gap-1.5">
        <ShieldCheck size={11} strokeWidth={2} className="shrink-0 text-[var(--vd-text-tertiary)]" />
        <span className="min-w-0 flex-1 truncate text-[11px] font-medium text-[var(--vd-text-secondary)]">
          {title ?? 'Sandboxed widget'}
        </span>
        <span className="vd-chip !h-[18px] !text-[9.5px]">isolated</span>
      </div>

      <div className="overflow-hidden rounded-[var(--radius-sm)] border border-[var(--vd-line)]">
        <iframe
          src={src}
          title={title ?? 'Sandboxed widget'}
          sandbox=""
          referrerPolicy="no-referrer"
          className="block w-full border-0 bg-[var(--vd-canvas-raised)]"
          style={{ height: box }}
        />
      </div>

      <p className="m-0 mt-1 text-[10px] text-[var(--vd-text-tertiary)]">
        no network · no scripts · no access to VibeDesk
      </p>
    </div>
  );
}
