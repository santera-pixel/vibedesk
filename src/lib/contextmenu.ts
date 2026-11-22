/**
 * WebView2 shows Chromium's own context menu — Back, Refresh, Save as, Print,
 * More tools — on any right-click that no handler claims. That menu is about a
 * browser, not about this app, and it is the one thing in the window that looks
 * like a web page pretending to be a desktop program.
 *
 * So it is suppressed, with two deliberate exceptions:
 *
 * - Editable fields keep it. Native Copy / Paste / Select all in the composer is
 *   worth more than the consistency of removing it.
 * - Shift + right-click keeps it anywhere, which is the escape hatch for
 *   Inspect Element when the app is running from a release build.
 *
 * The listener runs in the capture phase and only calls `preventDefault`, so a
 * component's own `onContextMenu` still fires afterwards and can open a real
 * menu.
 */
export function installContextMenuGuard(): void {
  document.addEventListener(
    'contextmenu',
    (event) => {
      if (event.shiftKey) return;
      const target = event.target;
      if (
        target instanceof Element &&
        target.closest('input, textarea, [contenteditable="true"]')
      ) {
        return;
      }
      event.preventDefault();
    },
    { capture: true },
  );
}
