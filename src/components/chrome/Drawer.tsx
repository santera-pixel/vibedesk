import { useCallback, useEffect, type ReactNode } from 'react';
import { ChevronLeft, X } from 'lucide-react';
import { isSettingsSection } from '@/lib/settings';
import { useApp, type PanelId } from '@/store/app';

interface DrawerProps {
  title: string;
  subtitle?: string;
  actions?: ReactNode;
  /**
   * Where the back arrow goes.
   *
   * A settings section defaults to the settings directory, because that is the
   * only place it can have come from. A drawer that is one level below some
   * other surface — the capability ledger, for one — says so here, rather than
   * leaving the user to close the panel and retype a command to get back.
   */
  back?: { to: PanelId; label: string };
  children: ReactNode;
}

/**
 * Escape closes a side panel.
 *
 * A side panel is not a modal, so it does not trap focus — but every other
 * dismissible surface in the window answers Escape, and a panel that only
 * closes with the mouse is the one place the keyboard stops working. Four
 * guards keep it from stealing the key: a field inside the panel keeps it
 * (Escape there means "leave this input"), a handler that already claimed it
 * keeps it (the composer's Escape cancels a turn), an open context menu keeps
 * it (that menu closes itself, and closing the panel underneath it as well
 * would be two dismissals for one keypress), and a preview in the proposal bar
 * keeps it.
 */
export function useEscapeToClose(onClose: () => void): void {
  useEffect(() => {
    function onKeyDown(event: KeyboardEvent) {
      if (event.key !== 'Escape' || event.defaultPrevented) return;
      if (document.querySelector('[role="menu"]')) return;
      const target = event.target;
      // Only a field *inside* the panel keeps the key. A field anywhere else —
      // the composer, most of all — is not what this guard is for, and treating
      // it as one meant Escape did nothing whenever the composer had focus,
      // which is where focus usually is.
      if (
        target instanceof Element &&
        target.closest(
          '[data-vd-drawer] input, [data-vd-drawer] textarea, [data-vd-drawer] select, [data-vd-drawer] [contenteditable="true"]',
        )
      ) {
        return;
      }
      onClose();
    }
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [onClose]);
}

export function Drawer({ title, subtitle, actions, back, children }: DrawerProps) {
  const setPanel = useApp((s) => s.setPanel);
  const panel = useApp((s) => s.panel);
  useEscapeToClose(useCallback(() => setPanel(null), [setPanel]));

  // A settings section is one level below the directory, so it says so. Without
  // this the only way back is the rail icon, which is discoverable once and
  // never again.
  const target = back ?? (isSettingsSection(panel) ? { to: 'settings' as const, label: 'Back to settings' } : null);

  return (
    <aside
      data-vd-drawer=""
      className="vd-panel-surface vd-enter flex w-[27rem] shrink-0 flex-col border-l border-[var(--vd-line)] bg-[var(--vd-canvas-raised)]"
    >
      <header className="flex h-11 shrink-0 items-center gap-2 border-b border-[var(--vd-line)] px-3">
        {target && (
          <button
            type="button"
            onClick={() => setPanel(target.to)}
            className="vd-icon-btn -ml-1"
            title={target.label}
          >
            <ChevronLeft size={14} strokeWidth={2} />
          </button>
        )}
        <div className="min-w-0">
          <h2 className="m-0 truncate text-[12.5px] font-semibold text-[var(--vd-text)]">{title}</h2>
          {subtitle && (
            <p className="m-0 truncate text-[10.5px] text-[var(--vd-text-tertiary)]">{subtitle}</p>
          )}
        </div>
        <div className="ml-auto flex items-center gap-1">
          {actions}
          <button
            type="button"
            onClick={() => setPanel(null)}
            className="vd-icon-btn"
            title="Close panel"
          >
            <X size={14} strokeWidth={2} />
          </button>
        </div>
      </header>
      <div className="min-h-0 flex-1 overflow-y-auto">{children}</div>
    </aside>
  );
}

export function Section({
  label,
  children,
  action,
}: {
  label: string;
  children: ReactNode;
  action?: ReactNode;
}) {
  return (
    <section className="border-b border-[var(--vd-line-soft)] px-3 py-3 last:border-b-0">
      <div className="mb-2 flex items-center gap-2">
        <h3 className="vd-label m-0">{label}</h3>
        <span className="h-px flex-1 bg-[var(--vd-line-soft)]" />
        {action}
      </div>
      {children}
    </section>
  );
}

export function Empty({ children }: { children: ReactNode }) {
  return (
    <p className="m-0 rounded-[var(--radius-sm)] border border-dashed border-[var(--vd-line)] px-3 py-4 text-center text-[11.5px] text-[var(--vd-text-tertiary)]">
      {children}
    </p>
  );
}

export function KeyValue({ label, value }: { label: string; value: ReactNode }) {
  return (
    <div className="flex items-baseline gap-3 py-0.5">
      <span className="w-24 shrink-0 text-[11px] text-[var(--vd-text-tertiary)]">{label}</span>
      <span className="min-w-0 flex-1 break-words font-mono text-[11px] text-[var(--vd-text-secondary)]">
        {value}
      </span>
    </div>
  );
}
