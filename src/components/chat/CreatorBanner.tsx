import { Hammer, X } from 'lucide-react';
import { toggleCreatorMode } from '@/lib/actions';
import { useApp } from '@/store/app';

/**
 * The mode indicator.
 *
 * Creator mode changes what the agent is told it is doing, which is invisible
 * from the transcript: without a standing marker the user cannot tell whether
 * the agent is describing VibeDesk or building it. It is deliberately not
 * dismissible — a mode you can hide is a mode you forget you are in — and the
 * exit is a real button rather than a second click on the chip, so turning it
 * off is never an accident.
 */
export function CreatorBanner() {
  const creatorMode = useApp((s) => s.creatorMode);
  const setPanel = useApp((s) => s.setPanel);
  if (!creatorMode) return null;

  return (
    <div className="mx-auto mb-2 w-full max-w-[var(--vd-composer-max)] px-6">
      <div className="flex items-center gap-2 rounded-[var(--radius-sm)] border border-[color-mix(in_oklab,var(--vd-accent)_28%,transparent)] bg-[var(--vd-accent-wash)] px-2.5 py-1.5">
        <Hammer size={11} strokeWidth={2.2} className="shrink-0 text-[var(--vd-accent)]" />
        <span className="shrink-0 whitespace-nowrap text-[11.5px] font-medium text-[var(--vd-text)]">
          Creator mode
        </span>
        <span className="min-w-0 truncate text-[11px] text-[var(--vd-text-secondary)]">
          changes are proposed, not applied
        </span>
        <button
          type="button"
          onClick={() => setPanel('customize')}
          className="ml-auto shrink-0 rounded-[var(--radius-xs)] px-1.5 py-0.5 text-[10.5px] font-medium text-[var(--vd-accent)] hover:bg-[color-mix(in_oklab,var(--vd-accent)_14%,transparent)]"
        >
          Customize
        </button>
        <button
          type="button"
          onClick={() => void toggleCreatorMode()}
          className="vd-icon-btn !h-[18px] !w-[18px] shrink-0"
          title="Turn creator mode off"
        >
          <X size={11} strokeWidth={2.4} />
        </button>
      </div>
    </div>
  );
}
