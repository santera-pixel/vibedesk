import { useEffect, useState } from 'react';
import { Circle, Clock, Cpu, Layers, Sparkles, Users } from 'lucide-react';
import { formatCost, formatTokens } from '@/lib/utils';
import { formatClock, formatSpan } from '@/lib/time';
import { useMinute } from '@/lib/clock';
import { onSwarm, swarm as swarmBridge } from '@/lib/bridge';
import { isLive, type Run } from '@/lib/swarm';
import { selectAgentLabel, selectModelLabel, useApp } from '@/store/app';

/**
 * The live run, if there is one.
 *
 * A run outlives the panel that started it, so closing the panel must not make
 * it invisible: a swarm spending tokens with no way back to it is the one thing
 * this bar exists to prevent. Polled slowly and event-driven, because the run
 * can also be started from a slash command while this bar is already mounted.
 */
function useLiveRun(): Run | null {
  const [run, setRun] = useState<Run | null>(null);

  useEffect(() => {
    let alive = true;
    const read = () => {
      swarmBridge
        .snapshot()
        .then((snapshot) => {
          if (alive) setRun(snapshot.run && isLive(snapshot.run.state) ? snapshot.run : null);
        })
        .catch(() => {
          // A bar that reports a bridge failure is a bar that is wrong about
          // something else. The panel is where a swarm problem is explained.
        });
    };
    read();
    const timer = window.setInterval(read, 5000);
    let stop: (() => void) | null = null;
    void onSwarm(read).then((unlisten) => {
      stop = unlisten;
    });
    return () => {
      alive = false;
      window.clearInterval(timer);
      stop?.();
    };
  }, []);

  return run;
}

export function StatusBar() {
  const usage = useApp((s) => s.usage);
  const status = useApp((s) => s.status);
  const plugins = useApp((s) => s.plugins);
  const environment = useApp((s) => s.environment);
  const model = useApp(selectModelLabel);
  const agentName = useApp(selectAgentLabel);
  const setPanel = useApp((s) => s.setPanel);
  const openedAt = useApp((s) => s.openedAt);
  const sessionId = useApp((s) => s.sessionId);
  const now = useMinute();
  const liveRun = useLiveRun();

  const ratio = usage && usage.size > 0 ? Math.min(1, usage.used / usage.size) : 0;
  const tone =
    ratio > 0.9 ? 'var(--vd-danger)' : ratio > 0.75 ? 'var(--vd-warn)' : 'var(--vd-accent)';

  return (
    <footer
      data-vd-protected="status"
      className="flex h-7 shrink-0 items-center gap-3 border-t border-[var(--vd-line)] bg-[var(--vd-canvas-raised)] px-3 text-[10.5px] text-[var(--vd-text-tertiary)]"
    >
      <span className="flex items-center gap-1.5">
        <Circle
          size={6}
          strokeWidth={0}
          fill={
            status?.state === 'ready' || status?.state === 'busy'
              ? 'var(--vd-accent)'
              : 'var(--vd-warn)'
          }
        />
        {status?.state ?? 'offline'}
        {status?.pid ? <span className="font-mono opacity-60">pid {status.pid}</span> : null}
      </span>

      <span className="h-3 w-px bg-[var(--vd-line)]" />

      <button
        type="button"
        onClick={() => setPanel('config')}
        className="flex items-center gap-1 hover:text-[var(--vd-text-secondary)]"
        title="Active model"
      >
        <Sparkles size={10} strokeWidth={2} />
        {model}
      </button>

      <button
        type="button"
        onClick={() => setPanel('config')}
        className="flex items-center gap-1 hover:text-[var(--vd-text-secondary)]"
        title="Agent profile"
      >
        <Cpu size={10} strokeWidth={2} />
        {agentName}
      </button>

      <button
        type="button"
        onClick={() => setPanel('plugins')}
        className="flex items-center gap-1 hover:text-[var(--vd-text-secondary)]"
        title="Installed plugins"
      >
        <Layers size={10} strokeWidth={2} />
        {plugins.length} plugin{plugins.length === 1 ? '' : 's'}
      </button>

      {liveRun && (
        <button
          type="button"
          onClick={() => setPanel('swarm')}
          className="flex items-center gap-1 text-[var(--vd-accent)] hover:text-[var(--vd-accent-strong)]"
          title={`Swarm ${liveRun.state} — ${liveRun.tasks.filter((t) => t.state === 'verified').length} of ${liveRun.tasks.length} verified`}
        >
          <Users size={10} strokeWidth={2} />
          <span className="vd-numeric">
            swarm {liveRun.tasks.filter((t) => t.state === 'verified').length}/
            {liveRun.tasks.length}
          </span>
        </button>
      )}

      <div className="ml-auto flex items-center gap-3">
        {/* How long this chat has been open. It sits with the other session
            facts rather than in the transcript, because it is a property of
            the chat and not of any one message — and it is the number that
            makes a long turn legible as long. */}
        <span
          className="flex items-center gap-1"
          title={
            sessionId
              ? `This chat opened at ${formatClock(openedAt, now)}`
              : 'No chat open yet'
          }
        >
          <Clock size={10} strokeWidth={2} />
          <span className="vd-numeric">
            {sessionId ? `open ${formatSpan(Math.max(0, now - openedAt))}` : 'no chat'}
          </span>
        </span>

        {usage && (
          <button
            type="button"
            onClick={() => setPanel('context')}
            className="flex items-center gap-1.5 hover:text-[var(--vd-text-secondary)]"
            title={`${usage.used.toLocaleString()} of ${usage.size.toLocaleString()} tokens in context`}
          >
            <span className="relative h-[3px] w-16 overflow-hidden rounded-full bg-[var(--vd-line)]">
              <span
                className="absolute inset-0 origin-left rounded-full transition-transform duration-[var(--vd-slow)] ease-[var(--vd-ease)]"
                style={{ transform: `scaleX(${Math.max(0.02, ratio)})`, background: tone }}
              />
            </span>
            <span className="vd-numeric">
              {formatTokens(usage.used)}/{formatTokens(usage.size)}
            </span>
          </button>
        )}

        {usage?.cost && (
          <span className="vd-numeric" title="Cumulative session cost">
            {formatCost(usage.cost.amount, usage.cost.currency)}
          </span>
        )}

        <span className="font-mono opacity-70">
          {environment?.cliVersion ? `vibe ${environment.cliVersion}` : 'vibe —'}
        </span>
      </div>
    </footer>
  );
}
