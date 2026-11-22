import { useEffect, useState } from 'react';
import { FolderClock, RefreshCw } from 'lucide-react';
import type { SessionSummary } from '@/types/acp';
import { cn, chatTitle, relativeTime, truncateMiddle } from '@/lib/utils';
import { session as sessionApi } from '@/lib/bridge';
import { errorMessage, openChat } from '@/lib/actions';
import { useApp } from '@/store/app';
import { Drawer, Empty, Section } from '@/components/chrome/Drawer';

export function SessionsPanel() {
  const cwd = useApp((s) => s.cwd);
  const activeId = useApp((s) => s.sessionId);
  const addNotice = useApp((s) => s.addNotice);
  const [sessions, setSessions] = useState<SessionSummary[]>([]);
  const [loading, setLoading] = useState(false);

  async function refresh() {
    setLoading(true);
    try {
      const list = await sessionApi.list(cwd ?? undefined);
      setSessions(list);
    } catch (error) {
      addNotice(errorMessage(error), 'error');
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    void refresh();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [cwd]);

  async function resume(entry: SessionSummary) {
    if (!cwd) return;
    // `openChat` rather than a bare `session/load`: it clears the transcript
    // first, so the replayed history replaces what is on screen instead of
    // being appended to it, and it marks the preamble as already sent, so the
    // next prompt does not inject the VibeDesk contract into a session that
    // already carries it.
    await openChat(entry.sessionId);
    if (useApp.getState().sessionId === entry.sessionId) {
      addNotice(`Resumed “${chatTitle(entry.title)}”.`, 'success');
    }
  }

  return (
    <Drawer
      title="Sessions"
      subtitle={cwd ? truncateMiddle(cwd, 40) : 'no folder open'}
      actions={
        <button
          type="button"
          onClick={() => void refresh()}
          className="vd-btn vd-btn-ghost !h-[24px] !px-1.5"
          title="Refresh"
        >
          <RefreshCw size={12} strokeWidth={2} className={cn(loading && 'vd-spin')} />
        </button>
      }
    >
      <Section label={`Saved in this folder · ${sessions.length}`}>
        {sessions.length === 0 ? (
          <Empty>
            {loading ? 'Reading session history…' : 'No saved sessions for this folder yet.'}
          </Empty>
        ) : (
          <div className="flex flex-col gap-1">
            {sessions.map((entry) => (
              <button
                key={entry.sessionId}
                type="button"
                onClick={() => void resume(entry)}
                className={cn(
                  'vd-card vd-card-interactive flex items-start gap-2 px-2.5 py-2 text-left',
                  entry.sessionId === activeId && 'border-[var(--vd-accent-dim)]',
                )}
              >
                <FolderClock
                  size={12}
                  strokeWidth={1.9}
                  className="mt-0.5 shrink-0 text-[var(--vd-text-tertiary)]"
                />
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-[11.5px] text-[var(--vd-text)]">
                    {chatTitle(entry.title)}
                  </span>
                  <span className="mt-0.5 flex items-center gap-2 text-[10px] text-[var(--vd-text-tertiary)]">
                    <span className="font-mono">{entry.sessionId.slice(0, 8)}</span>
                    <span>{relativeTime(entry.updatedAt)}</span>
                    {entry.sessionId === activeId && (
                      <span className="vd-chip vd-chip-accent !h-[15px] !text-[8.5px]">active</span>
                    )}
                  </span>
                </span>
              </button>
            ))}
          </div>
        )}
      </Section>
    </Drawer>
  );
}
