import { useCallback, useEffect, useRef, useState } from 'react';
import { customize } from '@/lib/bridge';
import { applyLayout, defaultLayout, type LayoutConfig } from '@/lib/customize';
import { useApp } from '@/store/app';

/** Must match the clamp in `src-tauri/src/layout.rs`. */
const MIN = 200;
const MAX = 520;

/** One arrow press, and one with Shift held. */
const STEP = 16;
const BIG_STEP = 48;

/**
 * Drag strip on the inner edge of the workstation rail.
 *
 * During a drag the width is written straight to the CSS custom property on
 * `<html>` — no React state, no re-render, so the rail tracks the pointer at
 * frame rate. Only on release is the value persisted, and the value that comes
 * back from Rust is what gets applied, so a clamped width is what the user ends
 * up seeing rather than what they asked for.
 *
 * It is also a window splitter: focusable, arrow keys move the edge, Home and
 * End jump to the limits, and the width is persisted once the key repeat
 * settles rather than once per press.
 */
export function ResizeHandle() {
  const layout = useApp((s) => s.layoutConfig);
  const setLayoutConfig = useApp((s) => s.setLayoutConfig);
  const notify = useApp((s) => s.notify);

  const [dragging, setDragging] = useState(false);
  const [keyWidth, setKeyWidth] = useState<number | null>(null);
  const drag = useRef<{ startX: number; startWidth: number; side: 'left' | 'right' } | null>(null);
  const saveTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const commit = useCallback(
    async (width: number) => {
      const base = layout ?? defaultLayout();
      const next: LayoutConfig = { ...base, sidebarWidth: width };
      try {
        const saved = await customize.setLayout(next);
        setLayoutConfig(saved);
        applyLayout(saved);
      } catch (error) {
        // Put the rail back where it was rather than leaving it at a width the
        // backend never accepted.
        applyLayout(base);
        notify(`Could not save the layout — ${String(error)}`, 'danger');
      } finally {
        setKeyWidth(null);
      }
    },
    [layout, notify, setLayoutConfig],
  );

  const onPointerDown = useCallback(
    (event: React.PointerEvent<HTMLDivElement>) => {
      const current = layout ?? defaultLayout();
      event.preventDefault();
      (event.target as HTMLElement).setPointerCapture(event.pointerId);
      drag.current = {
        startX: event.clientX,
        startWidth: current.sidebarWidth,
        side: current.sidebarSide,
      };
      setDragging(true);
    },
    [layout],
  );

  const onPointerMove = useCallback((event: React.PointerEvent<HTMLDivElement>) => {
    const state = drag.current;
    if (!state) return;

    // A right-hand rail grows as the pointer moves left, hence the sign flip.
    const delta = state.side === 'right' ? state.startX - event.clientX : event.clientX - state.startX;
    const next = Math.min(MAX, Math.max(MIN, Math.round(state.startWidth + delta)));
    document.documentElement.style.setProperty('--vd-sidebar-w', `${next}px`);
  }, []);

  const finish = useCallback(
    (event: React.PointerEvent<HTMLDivElement>) => {
      if (!drag.current) return;
      drag.current = null;
      setDragging(false);
      (event.target as HTMLElement).releasePointerCapture?.(event.pointerId);

      const width = Number.parseInt(
        getComputedStyle(document.documentElement).getPropertyValue('--vd-sidebar-w'),
        10,
      );
      const base = layout ?? defaultLayout();
      void commit(Number.isFinite(width) ? width : base.sidebarWidth);
    },
    [commit, layout],
  );

  const onKeyDown = useCallback(
    (event: React.KeyboardEvent<HTMLDivElement>) => {
      const base = layout ?? defaultLayout();
      const step = event.shiftKey ? BIG_STEP : STEP;
      // The edge moves away from the rail, so the key that grows it depends on
      // which side the rail is on.
      const grow = base.sidebarSide === 'right' ? 'ArrowLeft' : 'ArrowRight';
      const shrink = base.sidebarSide === 'right' ? 'ArrowRight' : 'ArrowLeft';

      let wanted: number | null = null;
      if (event.key === grow) wanted = base.sidebarWidth + step;
      else if (event.key === shrink) wanted = base.sidebarWidth - step;
      else if (event.key === 'Home') wanted = MIN;
      else if (event.key === 'End') wanted = MAX;
      if (wanted === null) return;

      event.preventDefault();
      const next = Math.min(MAX, Math.max(MIN, Math.round(wanted)));
      document.documentElement.style.setProperty('--vd-sidebar-w', `${next}px`);
      setKeyWidth(next);

      // Holding an arrow key repeats; one write per press would be one round
      // trip per press.
      if (saveTimer.current) clearTimeout(saveTimer.current);
      saveTimer.current = setTimeout(() => void commit(next), 350);
    },
    [commit, layout],
  );

  // A drag that ends outside the strip still has to commit.
  useEffect(() => {
    if (!dragging) return;
    const cancel = () => {
      drag.current = null;
      setDragging(false);
      applyLayout(useApp.getState().layoutConfig);
    };
    window.addEventListener('pointercancel', cancel);
    return () => window.removeEventListener('pointercancel', cancel);
  }, [dragging]);

  useEffect(
    () => () => {
      if (saveTimer.current) clearTimeout(saveTimer.current);
    },
    [],
  );

  const width = keyWidth ?? layout?.sidebarWidth ?? defaultLayout().sidebarWidth;

  return (
    <div
      role="separator"
      aria-orientation="vertical"
      aria-label="Resize the workstation rail"
      aria-valuenow={width}
      aria-valuemin={MIN}
      aria-valuemax={MAX}
      tabIndex={0}
      data-dragging={dragging}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={finish}
      onKeyDown={onKeyDown}
      className="vd-resize"
    />
  );
}
