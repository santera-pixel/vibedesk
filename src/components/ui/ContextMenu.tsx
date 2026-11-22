import { useEffect, useRef, type KeyboardEvent as ReactKeyboardEvent, type MouseEvent, type ReactNode } from 'react';
import { createPortal } from 'react-dom';
import type { LucideIcon } from 'lucide-react';
import { cn } from '@/lib/utils';

export type MenuAnchor = { x: number; y: number };

const MENU_WIDTH = 208;
const ROW_HEIGHT = 26;
const EDGE = 8;

/**
 * Clamp a click point so a menu of `rows` rows stays inside the window. The
 * height is estimated rather than measured because the menu does not exist yet
 * when the anchor is computed.
 */
export function anchorAt(event: MouseEvent, rows: number): MenuAnchor {
  return {
    x: Math.max(EDGE, Math.min(event.clientX, window.innerWidth - MENU_WIDTH - EDGE)),
    y: Math.max(EDGE, Math.min(event.clientY, window.innerHeight - rows * ROW_HEIGHT - EDGE)),
  };
}

/**
 * The keyboard equivalent of a right-click. Shift+F10 is the Windows
 * convention and the Menu key is the one on the keyboard, so both open the menu
 * anchored to the bottom-left of the row that has focus.
 *
 * Returns `null` for every other key, so a trigger can call this from its own
 * `onKeyDown` without swallowing the keys it already handles.
 */
export function keyboardAnchor(
  event: ReactKeyboardEvent<HTMLElement>,
  rows: number,
): MenuAnchor | null {
  const isMenuKey = event.key === 'ContextMenu' || (event.key === 'F10' && event.shiftKey);
  if (!isMenuKey) return null;
  event.preventDefault();
  const rect = event.currentTarget.getBoundingClientRect();
  return {
    x: Math.max(EDGE, Math.min(rect.left + 12, window.innerWidth - MENU_WIDTH - EDGE)),
    y: Math.max(EDGE, Math.min(rect.bottom - 4, window.innerHeight - rows * ROW_HEIGHT - EDGE)),
  };
}

/**
 * A right-click menu rendered into `document.body` so it is never clipped by an
 * `overflow: hidden` ancestor. Closes on the next click anywhere, on Escape, on
 * Tab and on resize.
 *
 * It is a real menu, not a mouse-only popup: it takes focus on open, moves it
 * with the arrow keys, Home and End, and hands it back to whatever had it when
 * it closes. A menu that can only be opened with a mouse is a menu half the
 * users cannot reach.
 */
export function ContextMenu({
  anchor,
  onClose,
  children,
}: {
  anchor: MenuAnchor;
  onClose: () => void;
  children: ReactNode;
}) {
  const menuRef = useRef<HTMLDivElement>(null);
  const restoreRef = useRef<HTMLElement | null>(null);

  useEffect(() => {
    const previous = document.activeElement;
    restoreRef.current = previous instanceof HTMLElement ? previous : null;

    const items = () =>
      [...(menuRef.current?.querySelectorAll<HTMLElement>('[role=menuitem]:not([disabled])') ?? [])];

    items()[0]?.focus();

    const close = () => onClose();

    const onKey = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        event.preventDefault();
        onClose();
        return;
      }
      if (event.key === 'Tab') {
        // A menu is not a tab stop. Tab leaves it, the way it leaves a native
        // menu, rather than walking into the page behind it.
        onClose();
        return;
      }
      const list = items();
      if (list.length === 0) return;
      const current = list.indexOf(document.activeElement as HTMLElement);
      let next = -1;
      if (event.key === 'ArrowDown') next = current < 0 ? 0 : (current + 1) % list.length;
      else if (event.key === 'ArrowUp') next = current < 0 ? list.length - 1 : (current - 1 + list.length) % list.length;
      else if (event.key === 'Home') next = 0;
      else if (event.key === 'End') next = list.length - 1;
      if (next < 0) return;
      event.preventDefault();
      list[next]?.focus();
    };

    window.addEventListener('click', close);
    window.addEventListener('resize', close);
    window.addEventListener('keydown', onKey);
    return () => {
      window.removeEventListener('click', close);
      window.removeEventListener('resize', close);
      window.removeEventListener('keydown', onKey);
      // Focus goes back where it came from, so a keyboard user is not dropped at
      // the top of the document after dismissing a menu.
      restoreRef.current?.focus();
    };
  }, [onClose]);

  return createPortal(
    <div
      ref={menuRef}
      role="menu"
      aria-orientation="vertical"
      tabIndex={-1}
      className="vd-menu"
      style={{ left: anchor.x, top: anchor.y, width: MENU_WIDTH }}
      onContextMenu={(event) => event.preventDefault()}
    >
      {children}
    </div>,
    document.body,
  );
}

export function MenuItem({
  icon: Icon,
  label,
  shortcut,
  danger,
  disabled,
  onSelect,
}: {
  icon: LucideIcon;
  label: string;
  shortcut?: string;
  danger?: boolean;
  disabled?: boolean;
  onSelect: () => void;
}) {
  return (
    <button
      type="button"
      role="menuitem"
      tabIndex={-1}
      disabled={disabled}
      onClick={onSelect}
      className={cn('vd-menu-item', danger && 'vd-menu-item-danger')}
    >
      <Icon size={11} strokeWidth={2} className="shrink-0 opacity-70" />
      <span className="min-w-0 flex-1 truncate">{label}</span>
      {shortcut && <span className="vd-menu-shortcut">{shortcut}</span>}
    </button>
  );
}

export function MenuSeparator() {
  return <div className="vd-menu-sep" role="separator" />;
}
