import { useState } from 'react';
import {
  ArrowDownToLine,
  FolderOpen,
  Hammer,
  Moon,
  Plug,
  RefreshCw,
  RotateCcw,
  Sun,
} from 'lucide-react';
import { open } from '@tauri-apps/plugin-dialog';
import { cn, basename, chatTitle, truncateMiddle } from '@/lib/utils';
import {
  errorMessage,
  pinWorkstation,
  reloadPlugins,
  reloadSession,
  toggleCreatorMode,
} from '@/lib/actions';
import { revealTheme } from '@/lib/reveal';
import { useApp } from '@/store/app';

export function TitleBar() {
  const cwd = useApp((s) => s.cwd);
  const title = useApp((s) => s.title);
  const theme = useApp((s) => s.theme);
  const creatorMode = useApp((s) => s.creatorMode);
  const setPanel = useApp((s) => s.setPanel);
  const update = useApp((s) => s.update);
  const status = useApp((s) => s.status);
  const notify = useApp((s) => s.notify);
  const addNotice = useApp((s) => s.addNotice);

  const [reloading, setReloading] = useState<'session' | 'plugins' | null>(null);

  async function pickFolder() {
    try {
      const picked = await open({ directory: true, multiple: false, title: 'Open a folder' });
      if (typeof picked !== 'string') return;
      // Pinning keeps the folder in the rail, so its chats stay reachable.
      await pinWorkstation(picked);
      notify(`Opened ${basename(picked)}`, 'success');
    } catch (error) {
      addNotice(errorMessage(error), 'error');
    }
  }

  async function onReloadSession() {
    setReloading('session');
    try {
      await reloadSession();
    } finally {
      setReloading(null);
    }
  }

  async function onReloadPlugins() {
    setReloading('plugins');
    try {
      await reloadPlugins();
    } finally {
      setReloading(null);
    }
  }

  return (
    <header className="vd-sheen flex h-11 shrink-0 items-center gap-3 border-b border-[var(--vd-line)] bg-[var(--vd-canvas-raised)] px-3">
      <div className="flex shrink-0 items-center gap-2">
        <span className="vd-wordmark text-[var(--vd-text)]">VibeDesk</span>
        <span className="vd-chip !h-[18px] !text-[9.5px]">acp</span>
      </div>

      <span className="h-4 w-px shrink-0 bg-[var(--vd-line)]" />

      <button
        type="button"
        onClick={pickFolder}
        className="group flex min-w-0 items-center gap-1.5 rounded-[var(--radius-sm)] px-1.5 py-1 text-left hover:bg-[var(--vd-surface-hover)]"
        title={cwd ?? 'Choose a working folder'}
      >
        <FolderOpen
          size={12}
          strokeWidth={1.9}
          className="shrink-0 text-[var(--vd-text-tertiary)]"
        />
        <span className="truncate font-mono text-[11.5px] text-[var(--vd-text-secondary)] group-hover:text-[var(--vd-text)]">
          {cwd ? truncateMiddle(cwd, 52) : 'choose a folder'}
        </span>
      </button>

      {title && (
        <>
          <span className="text-[var(--vd-line-strong)]">·</span>
          <span className="min-w-0 truncate text-[11.5px] text-[var(--vd-text-tertiary)]">
            {chatTitle(title)}
          </span>
        </>
      )}

      <div className="ml-auto flex shrink-0 items-center gap-1">
        {update?.updateAvailable && (
          <button
            type="button"
            onClick={() => setPanel('status')}
            className="vd-chip vd-chip-accent !h-[22px] hover:brightness-110"
            title={`Vibe ${update.latestVersion} is available`}
          >
            <ArrowDownToLine size={10} strokeWidth={2.4} />
            update {update.latestVersion}
          </button>
        )}

        <button
          type="button"
          onClick={onReloadPlugins}
          disabled={reloading !== null}
          className="vd-btn vd-btn-ghost !h-[26px] shrink-0"
          title="Reload plugins — re-discover from ~/.vibe/plugins and ./.vibe/plugins, then re-pin the session"
        >
          <Plug size={12} strokeWidth={1.9} className={cn(reloading === 'plugins' && 'vd-spin')} />
          {/* The labels are the first thing to go when the window narrows: the
 icons and tooltips carry the meaning, and a title bar that scrolls
 sideways is worse than one that is terse. */}
          <span className="max-[880px]:hidden">Reload plugins</span>
        </button>

        <button
          type="button"
          onClick={onReloadSession}
          disabled={reloading !== null}
          className="vd-btn !h-[26px] shrink-0"
          title="Reload session — restart the vibe-acp process and open a fresh session"
        >
          <RotateCcw
            size={12}
            strokeWidth={2}
            className={cn(reloading === 'session' && 'vd-spin')}
          />
          <span className="max-[880px]:hidden">Reload session</span>
        </button>

        <span className="mx-0.5 h-4 w-px bg-[var(--vd-line)]" />

        <button
          type="button"
          onClick={() => void toggleCreatorMode()}
          className={cn('vd-chip !h-[22px] hover:brightness-110', creatorMode && 'vd-chip-accent')}
          title={
            creatorMode
              ? 'Creator mode is on — the agent proposes interface changes, you decide'
              : "Creator mode — let the agent build VibeDesk's own interface with you"
          }
        >
          <Hammer size={10} strokeWidth={2.4} />
          creator
        </button>

        <button
          type="button"
          onClick={(event) =>
            revealTheme(theme === 'ink' ? 'paper' : 'ink', {
              x: event.clientX,
              y: event.clientY,
            })
          }
          className="vd-icon-btn"
          title={theme === 'ink' ? 'Switch to paper' : 'Switch to ink'}
        >
          {theme === 'ink' ? (
            <Sun size={13} strokeWidth={1.9} />
          ) : (
            <Moon size={13} strokeWidth={1.9} />
          )}
        </button>

        <button
          type="button"
          onClick={() => setPanel('status')}
          className="vd-icon-btn"
          title={`Agent ${status?.state ?? 'offline'}`}
        >
          <RefreshCw size={13} strokeWidth={1.9} />
        </button>
      </div>
    </header>
  );
}
