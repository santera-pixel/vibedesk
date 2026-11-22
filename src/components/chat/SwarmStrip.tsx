/**
 * The swarm, in the conversation.
 *
 * The panel is where a run is read in full: roster, board, graph, verdicts. This
 * is the other half of the same feature. A run started with `/swarm <goal>` has
 * to stay visible while the user keeps talking, or the only way to know it is
 * alive is to leave the conversation and open a drawer — which is exactly the
 * thing that made the swarm feel like a separate program.
 *
 * So the strip is one line: what the run is doing, how far it has got, who is on
 * it, and a door to the panel. It renders only when there is something to say —
 * a live run, or a run that finished while the user was watching. A finished run
 * lingers for a few seconds so the outcome is not something that happened while
 * they blinked, and a run opened from history never appears here at all: reading
 * an old run is not a run in progress.
 *
 * It polls rather than subscribing. `swarm://changed` fires on every state
 * change, but a strip mounted before a run exists has no event to wait for, and
 * one small file read every few seconds is cheaper than the bookkeeping that
 * would make the subscription complete.
 */
import { useCallback, useEffect, useRef, useState } from 'react';
import { ArrowUpRight, CircleCheck, CircleX, Square, Users } from 'lucide-react';
import { swarm as bridge } from '@/lib/bridge';
import { errorMessage } from '@/lib/actions';
import { formatTokens } from '@/lib/utils';
import { useApp } from '@/store/app';
import {
  describeRunState,
  isLive,
  memberTone,
  progress,
  roleGlyph,
  roleLabel,
  runTone,
  type Member,
  type SwarmSnapshot,
} from '@/lib/swarm';

/** Slow enough to be free, fast enough that a state change is not a surprise. */
const POLL_MS = 2500;

/** How long a finished run stays on screen after it stops being live. */
const LINGER_MS = 12_000;

/** Glyphs shown before the roster folds into a count. */
const SHOWN_MEMBERS = 8;

const DOT: Record<string, string> = {
  live: 'bg-[var(--vd-accent)]',
  good: 'bg-[var(--vd-ok)]',
  bad: 'bg-[var(--vd-danger)]',
  idle: 'bg-[var(--vd-text-tertiary)]',
};

const GLYPH: Record<string, string> = {
  live: 'border-[color-mix(in_oklab,var(--vd-accent)_45%,transparent)] text-[var(--vd-accent)]',
  good: 'border-[color-mix(in_oklab,var(--vd-ok)_45%,transparent)] text-[var(--vd-ok)]',
  bad: 'border-[color-mix(in_oklab,var(--vd-danger)_45%,transparent)] text-[var(--vd-danger)]',
  idle: 'border-[var(--vd-line)] text-[var(--vd-text-tertiary)]',
};

/**
 * One member, as a glyph.
 *
 * The lead is drawn first and with a ring, because "who is the orchestrator" is
 * the question this row exists to answer at a glance. A member that is burning
 * pulses; a parked one goes flat. Reduced motion keeps the colour and drops the
 * pulse, so the state is still readable without the movement.
 */
function Glyph({ member, lead }: { member: Member; lead: boolean }) {
  const tone = memberTone(member.state);
  const burning = member.state === 'working' || member.state === 'starting';
  return (
    <span
      title={`${roleLabel(member.role)} ${member.id} — ${member.state}${
        member.task ? ` on ${member.task}` : ''
      }`}
      className={`relative grid h-[15px] w-[15px] shrink-0 place-items-center rounded-[3px] border text-[8.5px] font-semibold leading-none ${GLYPH[tone]} ${
        lead ? 'ring-1 ring-[color-mix(in_oklab,var(--vd-accent)_35%,transparent)]' : ''
      }`}
    >
      {roleGlyph(member.role)}
      {burning && (
        <span
          className={`vd-swarm-pulse absolute -right-[2px] -top-[2px] h-[4px] w-[4px] rounded-full ${DOT[tone]}`}
        />
      )}
    </span>
  );
}

export function SwarmStrip() {
  const [data, setData] = useState<SwarmSnapshot | null>(null);
  const [lingering, setLingering] = useState<string | null>(null);
  const [stopping, setStopping] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const setPanel = useApp((s) => s.setPanel);
  const previous = useRef<{ id: string; live: boolean } | null>(null);

  const read = useCallback(async () => {
    try {
      const next = await bridge.snapshot();
      setData(next);
      setError(null);
    } catch (failure) {
      setError(errorMessage(failure));
    }
  }, []);

  useEffect(() => {
    void read();
    const timer = window.setInterval(() => void read(), POLL_MS);
    return () => window.clearInterval(timer);
  }, [read]);

  // A run that was live and is now terminal is the one case worth lingering on:
  // the user was watching it, so the outcome is news.
  useEffect(() => {
    const run = data?.run;
    if (!run) return;
    const was = previous.current;
    const live = isLive(run.state);
    if (was && was.id === run.id && was.live && !live) setLingering(run.id);
    previous.current = { id: run.id, live };
  }, [data]);

  useEffect(() => {
    if (!lingering) return;
    const timer = window.setTimeout(() => setLingering(null), LINGER_MS);
    return () => window.clearTimeout(timer);
  }, [lingering]);

  const run = data?.run ?? null;
  if (!run) return null;

  const live = isLive(run.state);
  if (!live && lingering !== run.id) return null;

  const tone = runTone(run.state);
  const external = data?.external ?? false;
  const done = run.tasks.filter((task) => task.state === 'verified').length;
  const share = progress(run);
  const roster = [run.lead, ...run.members];
  const shown = roster.slice(0, SHOWN_MEMBERS);
  const hidden = roster.length - shown.length;

  const stop = async () => {
    setStopping(true);
    try {
      await bridge.stop();
      await read();
    } catch (failure) {
      setError(errorMessage(failure));
    } finally {
      setStopping(false);
    }
  };

  return (
    <div className="mx-auto w-full max-w-[var(--vd-composer-max)] px-6 pb-1.5">
      <div
        className={`vd-enter relative flex items-center gap-2 rounded-[var(--radius-sm)] border px-2 py-1.5 ${
          live
            ? 'vd-aura border-[color-mix(in_oklab,var(--vd-accent)_30%,transparent)] bg-[var(--vd-accent-wash)]'
            : 'border-[var(--vd-line)] bg-[var(--vd-surface-sunken)]'
        }`}
      >
        <span className="relative grid h-[13px] w-[13px] shrink-0 place-items-center">
          <span className={`h-[6px] w-[6px] rounded-full ${DOT[tone]}`} />
          {live && (
            <span
              className={`vd-swarm-pulse absolute inset-0 rounded-full ${DOT[tone]} opacity-40`}
            />
          )}
        </span>

        <span className="shrink-0 text-[11px] font-semibold text-[var(--vd-text)]">Swarm</span>

        <span className="shrink-0 text-[10.5px] text-[var(--vd-text-tertiary)]">
          {describeRunState(run.state)}
        </span>

        {external && (
          <span
            className="vd-chip !h-[15px] !text-[9px]"
            title="This run is driven by another process. Stop it there."
          >
            external
          </span>
        )}

        <span className="min-w-0 flex-1 truncate text-[10.5px] text-[var(--vd-text-secondary)]">
          {run.goal}
        </span>

        {run.tasks.length > 0 && (
          <span className="flex shrink-0 items-center gap-1.5" title={`${done} of ${run.tasks.length} tasks verified`}>
            <span className="relative h-[3px] w-[52px] overflow-hidden rounded-full bg-[var(--vd-line)]">
              <span
                className={`absolute inset-y-0 left-0 rounded-full transition-[width] duration-[var(--vd-dur-2)] ease-[var(--vd-ease)] ${
                  tone === 'bad' ? 'bg-[var(--vd-danger)]' : 'bg-[var(--vd-accent)]'
                }`}
                style={{ width: `${Math.round(share * 100)}%` }}
              />
            </span>
            <span className="vd-numeric text-[10px] text-[var(--vd-text-tertiary)]">
              {done}/{run.tasks.length}
            </span>
          </span>
        )}

        <span className="flex shrink-0 items-center gap-[3px]" title="The lead, then the members it spawned">
          {shown.map((member) => (
            <Glyph key={member.id} member={member} lead={member.role === 'lead'} />
          ))}
          {hidden > 0 && (
            <span className="text-[9.5px] text-[var(--vd-text-tertiary)]">+{hidden}</span>
          )}
        </span>

        <span className="vd-numeric shrink-0 text-[10px] text-[var(--vd-text-tertiary)]">
          {formatTokens(run.budget.usedTokens)}
        </span>

        {live && !external && (
          <button
            type="button"
            onClick={() => void stop()}
            disabled={stopping}
            className="vd-icon-btn shrink-0"
            title="Stop the run — worktrees are kept and named"
          >
            <Square size={9} strokeWidth={2.5} />
          </button>
        )}

        <button
          type="button"
          onClick={() => setPanel('swarm')}
          className="vd-btn vd-btn-ghost !h-[19px] !px-1.5 !text-[10px]"
          title="Open the swarm panel — roster, board, graph, verdicts"
        >
          {tone === 'good' ? (
            <CircleCheck size={10} strokeWidth={2} />
          ) : tone === 'bad' ? (
            <CircleX size={10} strokeWidth={2} />
          ) : (
            <Users size={10} strokeWidth={2} />
          )}
          Panel
          <ArrowUpRight size={9} strokeWidth={2} />
        </button>
      </div>

      {error && (
        <p className="m-0 mt-1 text-[10px] text-[var(--vd-danger)]" role="status">
          {error}
        </p>
      )}
    </div>
  );
}
