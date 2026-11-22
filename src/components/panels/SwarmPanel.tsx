/**
 * The swarm panel.
 *
 * One panel, four surfaces, because a run is one thing and splitting it across
 * drawers would let the parts disagree:
 *
 *   Roster   who is on the run, lead first, and what each one is doing
 *   Board    what they said to each other, in arrival order
 *   Graph    the dependency DAG, with each task's state on it
 *   Ledger   the plan's shape, its cost, and the integration result
 *
 * The panel is a view over one snapshot. It never patches state from an event:
 * `swarm://changed` only tells it to re-read, so a dropped event cannot leave it
 * showing a run the engine is no longer in.
 *
 * The empty state is not a placeholder. It rehearses a plan — schedules and
 * costs it with no processes and no spend — which is the only honest way to show
 * what a swarm would do before asking for fifteen times the tokens of a chat.
 */
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import {
  ArrowRight,
  Ban,
  CircleAlert,
  CircleCheck,
  Crosshair,
  Focus,
  GitMerge,
  Layers,
  Pause,
  Play,
  RefreshCw,
  Sparkles,
  Square,
  Trash2,
  TriangleAlert,
  Users,
  X,
} from 'lucide-react';
import { Drawer, Empty, KeyValue, Section } from '@/components/chrome/Drawer';
import {
  NO_CREW,
  onSwarm,
  swarm as bridge,
  type CrewChoice,
  type SkillPreview,
} from '@/lib/bridge';
import { errorMessage } from '@/lib/actions';
import { cn, formatTokens, relativeTime } from '@/lib/utils';
import { useApp } from '@/store/app';
import {
  MIN_SPEEDUP,
  activityTone,
  boardKindTone,
  criticalPathLabel,
  exitPair,
  formatDuration,
  isLive,
  isTerminal,
  orderedTasks,
  outrunsTheHour,
  perStepLabel,
  progress,
  roleGlyph,
  roleLabel,
  roleMedianLine,
  runTone,
  describeTask,
  shapeWarningMessage,
  taskTone,
  verdictTone,
  worthRunning,
  type ActivityItem,
  type BoardEntry,
  type Catalog,
  type Member,
  type Plan,
  type Precondition,
  type RehearsalView,
  type Reflection,
  type RoleModels,
  type Run,
  type RunSummary,
  type SwarmSnapshot,
  type ThinkingByRole,
} from '@/lib/swarm';
import {
  NODE_H,
  NODE_W,
  agentSelected,
  countFilter,
  describeSelection,
  droppedBefore,
  graphView,
  inSelection,
  liveActivity,
  liveProcesses,
  memberDetail,
  memberTimeline,
  methodCoverage,
  rateSeries,
  rosterView,
  sparkPath,
  spendView,
  stepSelection,
  taskSelected,
  threads,
  type AgentView,
  type BoardFilter,
  type Exchange,
  type GraphView,
  type Selection,
} from '@/lib/swarm-view';

/* ------------------------------------------------------------------ *
 * Reading
 * ------------------------------------------------------------------ */

/**
 * The snapshot, kept fresh.
 *
 * Polled rather than event-driven alone: the engine emits on every state change,
 * but a panel opened while a run is mid-flight has no event to wait for, and a
 * poll is one cheap read of a file the engine already wrote.
 *
 * `selected` is the member whose feed the panel is showing. It is part of the
 * read rather than a filter applied afterwards, because the feed is two hundred
 * items and the engine only sends the selected member's. Changing the selection
 * therefore re-reads immediately instead of waiting up to two seconds for the
 * next poll — which is what makes a click feel like it did something.
 */
function useSwarm(selected: string | null) {
  const [data, setData] = useState<SwarmSnapshot | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(true);

  const refresh = useCallback(async () => {
    try {
      setData(await bridge.snapshot(selected));
      setError(null);
    } catch (failure) {
      setError(errorMessage(failure));
    } finally {
      setBusy(false);
    }
  }, [selected]);

  useEffect(() => {
    void refresh();
    const timer = window.setInterval(() => void refresh(), 2000);
    let stop: (() => void) | null = null;
    void onSwarm(() => void refresh()).then((unlisten) => {
      stop = unlisten;
    });
    return () => {
      window.clearInterval(timer);
      stop?.();
    };
  }, [refresh]);

  return { data, error, busy, refresh };
}

/* ------------------------------------------------------------------ *
 * Small pieces
 * ------------------------------------------------------------------ */

const TONE_TEXT: Record<string, string> = {
  live: 'text-[var(--vd-accent)]',
  good: 'text-[var(--vd-ok)]',
  bad: 'text-[var(--vd-danger)]',
  idle: 'text-[var(--vd-text-tertiary)]',
};

const TONE_DOT: Record<string, string> = {
  live: 'bg-[var(--vd-accent)]',
  good: 'bg-[var(--vd-ok)]',
  bad: 'bg-[var(--vd-danger)]',
  idle: 'bg-[var(--vd-text-tertiary)]',
};

const TONE_CHIP: Record<string, string> = {
  live: 'vd-chip vd-chip-accent',
  good: 'vd-chip',
  warn: 'vd-chip vd-chip-warn',
  bad: 'vd-chip vd-chip-danger',
  idle: 'vd-chip',
};

function Dot({ tone, pulse }: { tone: string; pulse?: boolean }) {
  return (
    <span className="relative flex h-2 w-2 shrink-0 items-center justify-center">
      {pulse && (
        <span
          className={`vd-swarm-pulse absolute inset-0 rounded-full ${TONE_DOT[tone] ?? TONE_DOT.idle}`}
        />
      )}
      <span className={`h-1.5 w-1.5 rounded-full ${TONE_DOT[tone] ?? TONE_DOT.idle}`} />
    </span>
  );
}

function Chip({ tone, children }: { tone: string; children: React.ReactNode }) {
  return (
    <span className={`${TONE_CHIP[tone] ?? TONE_CHIP.idle} !h-[17px] !text-[9px]`}>{children}</span>
  );
}

function Stat({ label, value }: { label: string; value: string }) {
  return (
    <div className="rounded-[var(--radius-sm)] border border-[var(--vd-line-soft)] px-2 py-1.5">
      <div className="vd-label !text-[9px]">{label}</div>
      <div className="vd-numeric mt-0.5 text-[12px] text-[var(--vd-text)]">{value}</div>
    </div>
  );
}

function RefreshButton({ onClick, busy }: { onClick: () => void; busy: boolean }) {
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={busy}
      className="vd-btn vd-btn-ghost !h-[22px] !px-1.5"
      title="Reload"
    >
      <RefreshCw size={11} strokeWidth={2} className={busy ? 'vd-spin' : undefined} />
    </button>
  );
}

/* ------------------------------------------------------------------ *
 * Preconditions
 * ------------------------------------------------------------------ */

/**
 * What must be true before a run may start.
 *
 * Shown before the goal field, not after a failure. A refusal is only cheap
 * before the user has typed anything, and every one of these is a fact about the
 * workstation rather than about the plan.
 */
function Preconditions({
  items,
  onOpenCapabilities,
}: {
  items: Precondition[];
  onOpenCapabilities: () => void;
}) {
  const blocked = items.filter((p) => !p.ready);
  return (
    <Section
      label="Preconditions"
      action={
        blocked.length === 0 ? (
          <Chip tone="good">ready</Chip>
        ) : (
          <Chip tone="bad">
            {blocked.length} blocked
          </Chip>
        )
      }
    >
      <ul className="m-0 flex list-none flex-col gap-1 p-0">
        {items.map((item, index) => (
          <li
            key={item.key}
            className="vd-enter flex items-start gap-2"
            style={{ ['--vd-i' as string]: index }}
          >
            {item.ready ? (
              <CircleCheck
                size={11}
                strokeWidth={2}
                className="mt-0.5 shrink-0 text-[var(--vd-ok)]"
              />
            ) : (
              <CircleAlert
                size={11}
                strokeWidth={2}
                className="mt-0.5 shrink-0 text-[var(--vd-danger)]"
              />
            )}
            <div className="min-w-0">
              <span className="text-[11.5px] text-[var(--vd-text-secondary)]">{item.label}</span>
              {!item.ready && item.detail && (
                <p className="m-0 mt-0.5 text-[10.5px] leading-[1.45] text-[var(--vd-text-tertiary)]">
                  {item.detail}
                </p>
              )}
              {/* The capability gate is the one precondition the user can fix
                  from here, so it is the one that gets a way to fix it. */}
              {!item.ready && item.key === 'capabilities' && (
                <button
                  type="button"
                  onClick={onOpenCapabilities}
                  className="vd-btn vd-btn-ghost mt-1 !h-[20px] !px-1.5 !text-[10px]"
                >
                  Open capabilities
                  <ArrowRight size={9} strokeWidth={2} />
                </button>
              )}
            </div>
          </li>
        ))}
      </ul>
    </Section>
  );
}

/* ------------------------------------------------------------------ *
 * The rehearsal
 * ------------------------------------------------------------------ */

/**
 * What a plan would cost, before it costs it.
 *
 * The waves are the scheduler's own output, not a drawing of it: the same
 * `advance` the engine runs is what produced them, so the rehearsal cannot
 * disagree with what the run will do.
 */
function RehearsalCard({ view }: { view: RehearsalView }) {
  const { rehearsal } = view;
  const worth = worthRunning(rehearsal);
  const roles = roleMedianLine(view.roleMedians);

  return (
    <div className="vd-enter rounded-[var(--radius-sm)] border border-[var(--vd-line)] bg-[var(--vd-surface-sunken)] p-2.5">
      {view.violations.length > 0 && (
        <ul className="m-0 mb-2 flex list-none flex-col gap-1 p-0">
          {view.violations.map((violation) => (
            <li
              key={violation.key}
              className="flex items-start gap-1.5 text-[11px] leading-[1.45] text-[var(--vd-danger)]"
            >
              <Ban size={11} strokeWidth={2} className="mt-0.5 shrink-0" />
              {violation.message}
            </li>
          ))}
        </ul>
      )}

      <div className="flex items-center gap-2">
        <span className="vd-label !text-[9px]">Waves</span>
        <span className="h-px flex-1 bg-[var(--vd-line-soft)]" />
        <span className="vd-numeric text-[10px] text-[var(--vd-text-tertiary)]">
          {view.concurrency} wide
        </span>
      </div>

      <ol className="m-0 mt-1.5 flex list-none flex-col gap-1 p-0">
        {rehearsal.waves.map((wave, index) => (
          <li
            key={wave.index}
            className="vd-enter flex items-center gap-2"
            style={{ ['--vd-i' as string]: index }}
          >
            <span className="vd-numeric w-4 shrink-0 text-[10px] text-[var(--vd-text-tertiary)]">
              {wave.index + 1}
            </span>
            <div className="flex min-w-0 flex-wrap gap-1">
              {wave.tasks.map((task) => (
                <span key={task} className="vd-chip !h-[17px] !text-[9px]">
                  {task}
                </span>
              ))}
            </div>
          </li>
        ))}
      </ol>

      <div className="mt-2.5 grid grid-cols-2 gap-1.5">
        <Stat label="speedup" value={`${rehearsal.speedup.toFixed(1)}×`} />
        <Stat label="critical path" value={String(rehearsal.criticalPath.length)} />
        <Stat label="swarm tokens" value={formatTokens(rehearsal.estimatedTokens)} />
        <Stat label="one agent" value={formatTokens(rehearsal.singleAgentTokens)} />
        {/* The projection and the budget it implies, side by side. The budget
            used to be a flat hour that never looked at the plan, so a plan whose
            own projection was longer than that was failed mid-task with every
            worktree discarded. Both numbers are here so that cannot happen
            quietly. */}
        <Stat label="projected" value={formatDuration(rehearsal.projectedWallSecs)} />
        <Stat label="budget" value={formatDuration(rehearsal.budgetWallSecs)} />
      </div>

      {/* How the projection was costed. The budget used to be derived from a
          constant that understated a real task by more than half, so the review
          says whether the figure beside it is this workspace's own measurement or
          the assumed ten minutes. */}
      <p className="m-0 mt-1.5 text-[10.5px] leading-[1.5] text-[var(--vd-text-tertiary)]">
        {perStepLabel(rehearsal)}.
      </p>

      {/* What each role's turns actually cost here. The observed run's verifier
          model took 20.7 minutes against the worker's 6, and nothing anywhere
          said so — a user choosing a verifier model should see that number before
          the run rather than after it. Absent until two runs have finished here,
          because one observation is not a median. */}
      {roles ? (
        <p className="m-0 mt-1 text-[10.5px] leading-[1.5] text-[var(--vd-text-tertiary)]">
          <span className="vd-numeric text-[var(--vd-text-secondary)]">{roles}</span>.
        </p>
      ) : null}

      <p className="m-0 mt-2 text-[10.5px] leading-[1.5] text-[var(--vd-text-tertiary)]">
        {worth ? (
          <>
            {rehearsal.serialSteps} steps serially, {rehearsal.parallelSteps} at this width.
            Critical path: {criticalPathLabel(rehearsal.criticalPath)}.
          </>
        ) : (
          <>
            Below {MIN_SPEEDUP}× — the coordination costs more than it saves. Run this as a
            chat.
          </>
        )}
      </p>
      <p className="m-0 mt-1 text-[10.5px] leading-[1.5] text-[var(--vd-text-tertiary)]">
        {outrunsTheHour(rehearsal) ? (
          // The budget is derived from the projection, so it can never be the
          // shorter of the two. The number worth warning about is the plan's own
          // wall clock: a plan that needs more than the hour every run used to
          // get is a decision to make before spending, whatever the budget says.
          <span style={{ color: 'var(--vd-warn)' }}>
            This plan is projected at {formatDuration(rehearsal.projectedWallSecs)} — more
            than the hour a run used to be given. Its budget is{' '}
            {formatDuration(rehearsal.budgetWallSecs)}.
          </span>
        ) : (
          <>
            Projected {formatDuration(rehearsal.projectedWallSecs)} against a{' '}
            {formatDuration(rehearsal.budgetWallSecs)} budget.
          </>
        )}
      </p>
      <p className="m-0 mt-1 text-[10px] text-[var(--vd-text-tertiary)]">
        Token figures are estimates, not measurements.
      </p>
    </div>
  );
}

/* ------------------------------------------------------------------ *
 * The roster
 * ------------------------------------------------------------------ */

/**
 * The token rate over the member's current turn.
 *
 * A rate, not a total: the total says what a member has cost, and the card
 * already shows that. The rate says whether it is doing anything, which is the
 * question a card asks twenty-one times at once — a member that has been quiet
 * for four minutes looks exactly like one that is thinking, unless the rate is
 * drawn.
 *
 * Forty by twelve, one path, no axes. `ChartBlock`'s `Sparkline` is the same idea
 * at a different size and is deliberately not reused: it is not exported, it
 * fills its container rather than taking a width, and it normalises to a 100x32
 * viewBox that would be resampled to forty pixels. Exporting a panel's private
 * component across panels to save four lines of path arithmetic would couple two
 * panels that share nothing else.
 */
function RateSparkline({ rates }: { rates: number[] }) {
  return (
    <svg
      width={40}
      height={12}
      viewBox="0 0 40 12"
      className="shrink-0"
      aria-hidden
      data-vd-sparkline="rate"
    >
      <path
        d={sparkPath(rates)}
        fill="none"
        stroke="var(--vd-accent)"
        strokeWidth={1}
        strokeLinejoin="round"
        strokeLinecap="round"
      />
    </svg>
  );
}

/**
 * One agent, as a card.
 *
 * Every card answers four questions in the same order, so the eye can compare
 * two of them without re-reading: what is it doing, on what, what has it spent,
 * and what did it last say. A card that cannot answer one of them says so
 * rather than leaving a gap, because a gap reads as "nothing to report" and a
 * missing measurement is not the same thing.
 *
 * The card is a button. Selecting an agent narrows the board to what that agent
 * said and what was said to it, which is the only way to read one member's
 * trajectory out of a shared transcript.
 */
function AgentCard({
  agent,
  selected,
  onSelect,
  lead,
  index,
}: {
  agent: AgentView;
  selected: boolean;
  onSelect: () => void;
  lead?: boolean;
  index: number;
}) {
  const tone = agent.tone;
  const { spend, context } = agent;
  const rates = rateSeries(agent.member.activity?.series ?? []);
  const latest = rates.length > 0 ? (rates[rates.length - 1] ?? null) : null;

  return (
    <button
      type="button"
      onClick={onSelect}
      aria-pressed={selected}
      className={`vd-enter group relative flex w-full flex-col gap-1 rounded-[var(--radius-sm)] border px-2 py-1.5 text-left transition-[border-color,background-color] duration-[var(--vd-dur-1)] ${
        selected
          ? 'border-[var(--vd-accent)] bg-[var(--vd-accent-wash)]'
          : 'border-[var(--vd-line)] bg-[var(--vd-surface)] hover:border-[var(--vd-line-strong)] hover:bg-[var(--vd-surface-hover)]'
      } ${lead && agent.member.state === 'working' ? 'vd-aura' : ''}`}
      style={{ ['--vd-i' as string]: index }}
      title={`${agent.member.id} · ${agent.doing}`}
    >
      <div className="flex w-full items-center gap-1.5">
        <Dot tone={tone} pulse={agent.member.state === 'working'} />
        <span
          className={`flex h-4 w-4 shrink-0 items-center justify-center rounded-[3px] border border-[var(--vd-line)] text-[9px] font-semibold ${TONE_TEXT[tone]}`}
          title={roleLabel(agent.role)}
        >
          {roleGlyph(agent.role)}
        </span>
        <span className="min-w-0 flex-1 truncate font-mono text-[10.5px] text-[var(--vd-text-secondary)]">
          {agent.member.id}
        </span>
        {lead && (
          <span className="vd-chip vd-chip-accent !h-[15px] !text-[8.5px]">orchestrator</span>
        )}
        {agent.parked && (
          <Pause size={9} strokeWidth={2} className="shrink-0 text-[var(--vd-text-tertiary)]" />
        )}
      </div>

      <span className={`block truncate text-[10.5px] leading-[1.4] ${TONE_TEXT[tone]}`}>
        {agent.doing}
      </span>

      <div className="flex w-full flex-wrap items-center gap-x-1.5 gap-y-0.5 text-[9.5px] text-[var(--vd-text-tertiary)]">
        {agent.task && (
          <span className="vd-chip !h-[15px] !text-[8.5px]">{agent.task.id}</span>
        )}
        {/* The model and the level the session reports, not the ones asked for.
            A level legal for one model is refused by another, so this is the
            only place the run's real reasoning effort is visible. */}
        <span
          className="truncate font-mono"
          title={`${agent.member.model}${agent.member.thinking ? ` · thinking ${agent.member.thinking}` : ''}`}
        >
          {agent.member.model}
        </span>
        {agent.member.thinking && (
          <span className="vd-chip !h-[15px] !text-[8.5px]" title="Reasoning effort the session reports">
            {agent.member.thinking}
          </span>
        )}
        {spend.tokens > 0 && (
          <span className="vd-numeric" title={`${spend.tokens.toLocaleString()} tokens`}>
            {formatTokens(spend.tokens)} tok
          </span>
        )}
        {spend.cached > 0 && (
          <span
            className="vd-numeric"
            title={`${spend.cached.toLocaleString()} input tokens came from the prompt cache`}
          >
            {Math.round(spend.cacheRate * 100)}% cached
          </span>
        )}
        {spend.secs > 0 && <span className="vd-numeric">{formatDuration(spend.secs)}</span>}
        {spend.turns > 0 && (
          <span className="vd-numeric">
            {spend.turns} {spend.turns === 1 ? 'turn' : 'turns'}
          </span>
        )}
        {agent.lines > 0 && (
          <span className="vd-numeric" title="Board entries this agent wrote">
            {agent.lines} said
          </span>
        )}
      </div>

      {/* The rate over the current turn. Flat means the member is spending
          nothing, which is the answer to "is it doing anything" — and the
          figure beside it is the absolute rate, so the shape is not the only
          reading. A member between turns has no series and draws flat. */}
      <div className="flex w-full items-center gap-1.5">
        <RateSparkline rates={rates} />
        <span
          className="vd-numeric text-[8.5px] text-[var(--vd-text-tertiary)]"
          title={
            latest === null
              ? 'No readings this turn — the member is spending nothing'
              : `${latest.toLocaleString()} tokens per minute over the current turn`
          }
        >
          {latest === null ? 'idle' : `${formatTokens(latest)}/min`}
        </span>
      </div>

      {/* The method this agent was handed, and whether it opened it. A filled
          chip was loaded by the agent's own tool call; an outlined one was named
          in the brief and never read. That distinction is the whole reason the
          engine watches the member's notifications instead of trusting the
          brief: a run that was told about a skill and a run that used one look
          identical from the outside. */}
      {agent.skills.assigned.length > 0 && (
        <div className="flex w-full flex-wrap items-center gap-1">
          {agent.skills.assigned.map((name) => {
            const loaded = agent.skills.loaded.includes(name);
            return (
              <span
                key={name}
                className={`vd-chip !h-[15px] !text-[8.5px] ${
                  loaded ? 'vd-chip-accent' : 'border-dashed opacity-55'
                }`}
                title={
                  loaded
                    ? `${name} · this agent opened it`
                    : `${name} · assigned in the brief, never opened`
                }
              >
                {name}
              </span>
            );
          })}
          {agent.skills.missing.length > 0 && (
            <span
              className="vd-numeric text-[8.5px] text-[var(--vd-warn)]"
              title={`${agent.skills.missing.join(', ')} — named in the brief, never opened`}
            >
              {agent.skills.missing.length} unread
            </span>
          )}
        </div>
      )}

      {/* The context meter. A member near its window is about to be compacted,
          and that is the one thing about a member that predicts its next
          failure. Hidden when the size is unknown rather than drawn at zero. */}
      {context.size > 0 && (
        <div className="flex w-full items-center gap-1.5">
          <span className="h-[3px] flex-1 overflow-hidden rounded-full bg-[var(--vd-surface-sunken)]">
            <span
              className={`block h-full origin-left rounded-full transition-transform duration-[var(--vd-slow)] ease-[var(--vd-ease)] ${
                context.fraction > 0.85 ? 'bg-[var(--vd-danger)]' : 'bg-[var(--vd-text-tertiary)]'
              }`}
              style={{ transform: `scaleX(${context.fraction})` }}
            />
          </span>
          <span
            className="vd-numeric shrink-0 text-[8.5px] text-[var(--vd-text-tertiary)]"
            title={`${context.used.toLocaleString()} of ${context.size.toLocaleString()} context tokens`}
          >
            {Math.round(context.fraction * 100)}% ctx
          </span>
        </div>
      )}

      {agent.lastLine && (
        <span className="line-clamp-2 w-full text-[9.5px] leading-[1.4] text-[var(--vd-text-tertiary)]">
          {agent.lastLine.text}
        </span>
      )}

      {agent.member.error && (
        <span className="line-clamp-2 w-full text-[9.5px] leading-[1.4] text-[var(--vd-danger)]">
          {agent.member.error}
        </span>
      )}
    </button>
  );
}

/**
 * `j` and `k` walk the roster.
 *
 * The roster is the one surface a user reads top to bottom, and reaching for the
 * mouse to move one row down is the moment the panel stops being something you
 * can watch. The order is `rosterView(...).all`, which is the order on screen —
 * a keyboard that walked `run.members` would walk arrival order and disagree
 * with the eye.
 *
 * The guards are the same four `useEscapeToClose` has, for the same reasons: a
 * field keeps the key (typing `j` in the goal box must type a `j`), a
 * contenteditable keeps it, a handler that already claimed it keeps it, and an
 * open menu keeps it. A modifier keeps it too, so `Ctrl+J` and `Cmd+K` stay the
 * window's.
 */
function useRosterKeys(
  order: string[],
  selection: Selection,
  onSelect: (next: Selection) => void,
): void {
  useEffect(() => {
    function onKeyDown(event: KeyboardEvent) {
      if (event.key !== 'j' && event.key !== 'k') return;
      if (event.defaultPrevented) return;
      if (event.metaKey || event.ctrlKey || event.altKey) return;
      if (document.querySelector('[role="menu"]')) return;
      const target = event.target;
      if (
        target instanceof Element &&
        target.closest('input, textarea, select, [contenteditable="true"]')
      ) {
        return;
      }
      event.preventDefault();
      onSelect(stepSelection(selection, order, event.key === 'j' ? 1 : -1));
    }
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [order, selection, onSelect]);
}

/**
 * Who is on the run.
 *
 * The lead is not a row in the list, it is the head of it: it owns the run's
 * acceptance criterion and it is the only member that outlives a task. The
 * workers and verifiers hang off it as a board of cards, grouped by the task
 * they are on, so the shape of the answer to "who is doing what" is the shape of
 * the tree.
 *
 * The counts in the header are the four states worth knowing at a glance, in the
 * order a run passes through them: how many have no process yet, how many are
 * inside a turn, how many are suspended and spending nothing, and how many are
 * broken. A roster of twelve with one working is a run that is stuck, and that
 * has to be visible without reading twelve cards. `queued` is counted separately
 * from `parked` because the two look identical on a card and mean opposite
 * things: one has nothing to hold, the other is holding a session it is not
 * spending on.
 */
function Roster({
  run,
  board,
  selection,
  onSelect,
}: {
  run: Run;
  board: BoardEntry[];
  selection: Selection;
  onSelect: (next: Selection) => void;
}) {
  const view = useMemo(() => rosterView(run, board), [run, board]);
  const order = useMemo(() => view.all.map((agent) => agent.member.id), [view]);
  useRosterKeys(order, selection, onSelect);
  const toggle = (id: string) =>
    onSelect(agentSelected(selection, id) ? null : { kind: 'agent', id });

  return (
    <Section
      label="Roster"
      action={
        <span className="flex items-center gap-1.5">
          {view.queued > 0 && (
            <span
              className="vd-numeric text-[9.5px] text-[var(--vd-text-tertiary)]"
              title="No process yet — the scheduler has not given them a slot"
            >
              {view.queued} queued
            </span>
          )}
          {view.working > 0 && (
            <span className="vd-numeric text-[9.5px] text-[var(--vd-accent)]">
              {view.working} working
            </span>
          )}
          {view.parked > 0 && (
            <span className="vd-numeric text-[9.5px] text-[var(--vd-text-tertiary)]">
              {view.parked} parked
            </span>
          )}
          {view.broken > 0 && (
            <span className="vd-numeric text-[9.5px] text-[var(--vd-danger)]">
              {view.broken} broken
            </span>
          )}
          <span className="vd-numeric text-[9.5px] text-[var(--vd-text-tertiary)]">
            {view.all.length}
          </span>
        </span>
      }
    >
      <AgentCard
        agent={view.lead}
        lead
        index={0}
        selected={agentSelected(selection, view.lead.member.id)}
        onSelect={() => toggle(view.lead.member.id)}
      />

      {view.groups.map((group, groupIndex) => (
        <div key={group.task?.id ?? 'unassigned'} className="mt-2">
          <div className="flex items-center gap-1.5">
            <span className="vd-label !text-[9px]">{group.task?.id ?? 'unassigned'}</span>
            {group.task && (
              <span className="min-w-0 flex-1 truncate text-[9.5px] text-[var(--vd-text-tertiary)]">
                {group.task.title}
              </span>
            )}
            <span className="h-px flex-1 bg-[var(--vd-line-soft)]" />
            {group.task && (
              <button
                type="button"
                onClick={() =>
                  onSelect(
                    taskSelected(selection, group.task!.id)
                      ? null
                      : { kind: 'task', id: group.task!.id },
                  )
                }
                className="vd-btn vd-btn-ghost !h-[16px] !px-1 !text-[8.5px]"
                title="Show only this task on the board"
              >
                <Crosshair size={8} strokeWidth={2} />
              </button>
            )}
          </div>
          <div className="mt-1 grid grid-cols-2 gap-1">
            {group.agents.map((agent, index) => (
              <AgentCard
                key={agent.member.id}
                agent={agent}
                index={index + groupIndex + 1}
                selected={agentSelected(selection, agent.member.id)}
                onSelect={() => toggle(agent.member.id)}
              />
            ))}
          </div>
        </div>
      ))}

      {view.reflector && (
        <div className="mt-2">
          <div className="flex items-center gap-1.5">
            <span className="vd-label !text-[9px]">after the run</span>
            <span className="h-px flex-1 bg-[var(--vd-line-soft)]" />
          </div>
          <div className="mt-1">
            <AgentCard
              agent={view.reflector}
              index={view.all.length}
              selected={agentSelected(selection, view.reflector.member.id)}
              onSelect={() => toggle(view.reflector!.member.id)}
            />
          </div>
        </div>
      )}

      {/* The hint is the feature. A shortcut nobody can discover is not one, and
          the roster is the only place the keys apply, so it is the only place
          they are named. */}
      <p className="m-0 mt-2 flex items-center gap-1 text-[9px] text-[var(--vd-text-tertiary)]">
        <kbd className="vd-kbd">j</kbd>
        <kbd className="vd-kbd">k</kbd>
        move through the roster
      </p>
    </Section>
  );
}

/* ------------------------------------------------------------------ *
 * One member, in detail
 * ------------------------------------------------------------------ */

/**
 * One line of the feed.
 *
 * The stamp is how long ago the line landed, not a wall clock: a feed read top
 * to bottom is a sequence of durations, and `just now` on every line of a member
 * that is working right now would say nothing at all.
 */
function ActivityLine({ item, now }: { item: ActivityItem; now: number }) {
  const tone = activityTone(item);
  const at = new Date(item.at).getTime();
  const ago = Number.isNaN(at) ? 0 : Math.max(0, (now - at) / 1000);
  return (
    <li className="flex items-baseline gap-1.5">
      <span className="vd-numeric w-[2.6rem] shrink-0 text-right text-[9px] text-[var(--vd-text-tertiary)]">
        {formatDuration(ago)}
      </span>
      <Dot tone={tone} />
      <span className="min-w-0 flex-1 truncate font-mono text-[10px] text-[var(--vd-text-secondary)]">
        {item.title}
      </span>
      <span className={`shrink-0 text-[9px] uppercase tracking-wide ${TONE_TEXT[tone]}`}>
        {item.status}
      </span>
    </li>
  );
}

/**
 * One member, in detail.
 *
 * The roster answers "who is on the run" in a card the width of a column. This
 * answers "what is this one doing" in the width of the panel: the tool it is
 * running and how long it has been running, the turn's clock, what it has spent
 * and how fast, how full its window is, the last two hundred things it did, and
 * what it said on the board.
 *
 * The feed is the reason the pane exists. A member's state word says `working`
 * for the whole of a thirteen-minute turn; the feed says it has been in `npm
 * test` for four minutes and that the last thing it said was about a failing
 * assertion. That is the difference between a roster and an instrument.
 *
 * The elapsed times are computed at render, not stored: the panel re-reads every
 * two seconds, so the numbers move with the poll and no second timer is needed.
 * That is also why the projection is not memoised — it reads the clock, and a
 * memo keyed on the run would be a memo that lies about its dependencies.
 */
function MemberDetail({
  run,
  board,
  memberId,
  onClear,
}: {
  run: Run;
  board: BoardEntry[];
  memberId: string;
  onClear: () => void;
}) {
  const member =
    memberId === run.lead.id ? run.lead : (run.members.find((m) => m.id === memberId) ?? null);
  const now = Date.now();
  const view = member ? memberDetail(member, run, board, now) : null;
  if (!member || !view) return null;

  const { agent, current, turnSecs, rate, feed, entries } = view;
  const tone = agent.tone;
  // `EntryLine` looks the author up to print its role. The pane only ever draws
  // this member's own entries, so the roster it needs is one row.
  const roster = new Map([[member.id, member]]);

  return (
    <Section
      label="Member"
      action={
        <button
          type="button"
          onClick={onClear}
          className="vd-icon-btn !h-[16px] !w-[16px]"
          title="Show everything again"
        >
          <X size={9} strokeWidth={2} />
        </button>
      }
    >
      <div className="flex items-center gap-1.5">
        <Dot tone={tone} pulse={member.state === 'working'} />
        <span
          className={`flex h-4 w-4 shrink-0 items-center justify-center rounded-[3px] border border-[var(--vd-line)] text-[9px] font-semibold ${TONE_TEXT[tone]}`}
          title={roleLabel(agent.role)}
        >
          {roleGlyph(agent.role)}
        </span>
        <span className="min-w-0 truncate font-mono text-[11px] text-[var(--vd-text)]">
          {member.id}
        </span>
        <span className="shrink-0 text-[9.5px] text-[var(--vd-text-tertiary)]">
          {roleLabel(agent.role)}
        </span>
        <span className={`ml-auto shrink-0 text-[10px] ${TONE_TEXT[tone]}`}>{agent.doing}</span>
      </div>

      {/* The tool it is running, and how long it has been running. This is the
          line the phase exists for: a state word cannot say `npm test`, and a
          member four minutes into a suite and one that has hung look identical
          without it. */}
      {current ? (
        <div className="mt-1.5 flex items-center gap-1.5 rounded-[var(--radius-sm)] border border-[var(--vd-line)] bg-[var(--vd-surface-sunken)] px-2 py-1.5">
          <span className="vd-label !text-[9px]">now</span>
          <span className="min-w-0 flex-1 truncate font-mono text-[11px] text-[var(--vd-text)]">
            {current.item.title}
          </span>
          <Chip tone={activityTone(current.item)}>{current.item.status}</Chip>
          <span
            className="vd-numeric shrink-0 text-[10px] text-[var(--vd-text-secondary)]"
            title={`running since ${current.item.at}`}
          >
            {formatDuration(current.secs)}
          </span>
        </div>
      ) : (
        <p className="m-0 mt-1.5 text-[10.5px] text-[var(--vd-text-tertiary)]">
          {member.state === 'working'
            ? 'No tool call in flight — it is thinking.'
            : 'Nothing running.'}
        </p>
      )}

      <div className="mt-1.5 grid grid-cols-3 gap-1.5">
        <Stat label="turn" value={turnSecs === null ? '—' : formatDuration(turnSecs)} />
        <Stat label="tokens" value={formatTokens(agent.spend.tokens)} />
        <Stat label="rate" value={rate === null ? '—' : `${formatTokens(rate)}/min`} />
      </div>

      {/* The context meter. A member near its window is about to be compacted,
          and that is the one thing about a member that predicts its next
          failure. Hidden when the size is unknown rather than drawn at zero. */}
      {agent.context.size > 0 && (
        <div className="mt-1.5 flex items-center gap-1.5">
          <span className="h-[3px] flex-1 overflow-hidden rounded-full bg-[var(--vd-surface-sunken)]">
            <span
              className={`block h-full origin-left rounded-full transition-transform duration-[var(--vd-slow)] ease-[var(--vd-ease)] ${
                agent.context.fraction > 0.85
                  ? 'bg-[var(--vd-danger)]'
                  : 'bg-[var(--vd-text-tertiary)]'
              }`}
              style={{ transform: `scaleX(${agent.context.fraction})` }}
            />
          </span>
          <span
            className="vd-numeric shrink-0 text-[8.5px] text-[var(--vd-text-tertiary)]"
            title={`${agent.context.used.toLocaleString()} of ${agent.context.size.toLocaleString()} context tokens`}
          >
            {Math.round(agent.context.fraction * 100)}% ctx
          </span>
        </div>
      )}

      <div className="mt-2 flex items-center gap-1.5">
        <span className="vd-label !text-[9px]">activity</span>
        <span className="h-px flex-1 bg-[var(--vd-line-soft)]" />
        <span className="vd-numeric text-[9px] text-[var(--vd-text-tertiary)]">{feed.length}</span>
      </div>

      {feed.length === 0 ? (
        <Empty>
          {isLive(run.state)
            ? 'Nothing yet. The feed fills as this member works, and the panel re-reads it every two seconds.'
            : 'No feed. Activity is recorded while a run is live, so a finished run has none.'}
        </Empty>
      ) : (
        <ol className="m-0 mt-1 flex max-h-[16rem] list-none flex-col gap-1 overflow-y-auto p-0">
          {feed.map((item, index) => (
            <ActivityLine key={`${item.at}-${index}`} item={item} now={now} />
          ))}
        </ol>
      )}

      {entries.length > 0 && (
        <>
          <div className="mt-2 flex items-center gap-1.5">
            <span className="vd-label !text-[9px]">on the board</span>
            <span className="h-px flex-1 bg-[var(--vd-line-soft)]" />
            <span className="vd-numeric text-[9px] text-[var(--vd-text-tertiary)]">
              {entries.length}
            </span>
          </div>
          <div className="mt-1 flex flex-col gap-1">
            {entries.map((entry) => (
              <EntryLine key={entry.seq} entry={entry} roster={roster} />
            ))}
          </div>
        </>
      )}
    </Section>
  );
}

/* ------------------------------------------------------------------ *
 * The board
 * ------------------------------------------------------------------ */

/** The filters, in the order the panel offers them. */
const FILTERS: Array<{ key: BoardFilter; label: string }> = [
  { key: 'all', label: 'all' },
  { key: 'problems', label: 'problems' },
  { key: 'decisions', label: 'decisions' },
  { key: 'verdicts', label: 'verdicts' },
  { key: 'evidence', label: 'evidence' },
];

/** One entry, as a line. */
function EntryLine({ entry, roster }: { entry: BoardEntry; roster: Map<string, Member> }) {
  const tone = boardKindTone(entry.kind);
  const author = roster.get(entry.from);
  return (
    <div className="rounded-[var(--radius-sm)] border border-[var(--vd-line-soft)] px-2 py-1.5">
      <div className="flex items-center gap-1.5">
        <Dot tone={tone} />
        <span className="truncate font-mono text-[10px] text-[var(--vd-text-secondary)]">
          {entry.from}
        </span>
        {author && (
          <span className="text-[9px] text-[var(--vd-text-tertiary)]">
            {roleLabel(author.role)}
          </span>
        )}
        <span className={`text-[9px] uppercase tracking-wide ${TONE_TEXT[tone]}`}>
          {entry.kind}
        </span>
        <span className="ml-auto shrink-0 text-[9px] text-[var(--vd-text-tertiary)]">
          {relativeTime(entry.at)}
        </span>
      </div>
      <p className="m-0 mt-1 whitespace-pre-wrap break-words text-[11px] leading-[1.5] text-[var(--vd-text-secondary)]">
        {entry.text}
      </p>
      {entry.evidence.length > 0 && (
        <ul className="m-0 mt-1 flex list-none flex-col gap-0.5 border-l border-[var(--vd-line)] p-0 pl-2">
          {entry.evidence.map((line, index) => (
            <li
              key={index}
              className="whitespace-pre-wrap break-words font-mono text-[10px] leading-[1.45] text-[var(--vd-text-tertiary)]"
            >
              {line}
            </li>
          ))}
        </ul>
      )}
      {entry.to.length > 0 && (
        <div className="mt-1 flex items-center gap-1 text-[9px] text-[var(--vd-text-tertiary)]">
          <ArrowRight size={9} strokeWidth={2} />
          {entry.to.join(', ')}
        </div>
      )}
    </div>
  );
}

/**
 * One disagreement and everything that followed it.
 *
 * Drawn as one object with a spine, not as two siblings: the opener is the
 * problem, the replies are what answered it, and the spine is the only thing on
 * the board that says "these belong together". An unresolved exchange keeps its
 * spine in the danger tone, because a question nobody answered is the state a
 * stuck run is in.
 */
function ExchangeBlock({
  exchange,
  roster,
}: {
  exchange: Exchange;
  roster: Map<string, Member>;
}) {
  const tone = boardKindTone(exchange.opener.kind);
  return (
    <div
      className={`rounded-[var(--radius-sm)] border-l-2 pl-2 ${
        exchange.resolved
          ? 'border-l-[var(--vd-line-strong)]'
          : 'border-l-[var(--vd-danger)]'
      }`}
    >
      <EntryLine entry={exchange.opener} roster={roster} />
      {exchange.replies.length > 0 && (
        <ul className="m-0 mt-1 flex list-none flex-col gap-1 border-l border-[var(--vd-line)] p-0 pl-2">
          {exchange.replies.map((reply) => (
            <li key={reply.seq}>
              <EntryLine entry={reply} roster={roster} />
            </li>
          ))}
        </ul>
      )}
      <div className="mt-1 flex items-center gap-1.5 text-[9px] text-[var(--vd-text-tertiary)]">
        {exchange.resolved ? (
          <>
            <CircleCheck size={9} strokeWidth={2} className="text-[var(--vd-ok)]" />
            answered
          </>
        ) : (
          <>
            <CircleAlert size={9} strokeWidth={2} className="text-[var(--vd-danger)]" />
            still open
          </>
        )}
        {exchange.owner && <span>· for {exchange.owner}</span>}
        <span className={`ml-auto uppercase tracking-wide ${TONE_TEXT[tone]}`}>
          {exchange.opener.kind}
        </span>
      </div>
    </div>
  );
}

/**
 * What the selected member is doing, inside the board.
 *
 * The board is a transcript, and a transcript is what *happened*. When the board
 * is narrowed to one member the question being asked is what that member is
 * doing *now*, and the answer used to live in a pane above — so scrolling the
 * transcript lost it, which is the one moment it is worth reading.
 *
 * Deliberately not the detail pane. That one is the instrument: turn clock,
 * spend, context meter, the whole feed, and what the member said. This is the
 * headline — the tool call in flight and the last few things it finished — short
 * enough to sit above a transcript without pushing it off the screen.
 *
 * It is rendered outside the board's scroll container on purpose. The board pins
 * itself to the newest entry, and a block inside that container would be scrolled
 * past on the next entry.
 */
function LiveActivity({ member }: { member: Member }) {
  const { current, elapsed, recent, note } = liveActivity(member, Date.now());

  return (
    <div className="mb-2 rounded-[var(--radius-sm)] border border-[var(--vd-line)] bg-[var(--vd-surface-sunken)] px-2 py-1.5">
      <div className="flex items-center gap-1.5">
        <span className="vd-label !text-[9px]">live</span>
        <span className="min-w-0 flex-1 truncate font-mono text-[10.5px] text-[var(--vd-text)]">
          {current ? current.title : note}
        </span>
        {current && <Chip tone={activityTone(current)}>{current.status}</Chip>}
        {current && (
          <span
            className="vd-numeric shrink-0 text-[9.5px] text-[var(--vd-text-secondary)]"
            title={`running since ${current.at}`}
          >
            {formatDuration(elapsed)}
          </span>
        )}
      </div>
      {recent.length > 0 && (
        <ol className="m-0 mt-1 flex list-none flex-col gap-0.5 p-0">
          {recent.map((item, index) => (
            <ActivityLine key={`${item.at}-${index}`} item={item} now={Date.now()} />
          ))}
        </ol>
      )}
    </div>
  );
}

/**
 * What the members said to each other.
 *
 * Grouped by task and split into exchanges, because the board is read as a
 * sequence of disagreements rather than as a log: a decision only means
 * something next to the problem that forced it. The engine assigns the sequence
 * numbers, so the order here is the order the entries were accepted, not the
 * order they were written.
 *
 * The filter keeps an exchange whole. Filtering to problems shows each problem
 * and what answered it, because a problem with its answer cut off is a worse
 * answer than no filter at all.
 */
function Board({
  entries,
  run,
  filter,
  onFilter,
  selection,
  onSelect,
}: {
  entries: BoardEntry[];
  run: Run;
  filter: BoardFilter;
  onFilter: (next: BoardFilter) => void;
  selection: Selection;
  onSelect: (next: Selection) => void;
}) {
  const endRef = useRef<HTMLDivElement | null>(null);
  const [pinned, setPinned] = useState(true);

  const roster = useMemo(() => {
    const map = new Map<string, Member>();
    map.set(run.lead.id, run.lead);
    for (const member of run.members) map.set(member.id, member);
    return map;
  }, [run.lead, run.members]);

  const visible = useMemo(
    () => (selection ? entries.filter((entry) => inSelection(entry, selection)) : entries),
    [entries, selection],
  );
  // The member the board is narrowed to, when it is narrowed to one. The feed is
  // only sent for the selected member, so this is also the only member whose
  // activity is worth drawing here.
  const selectedMember =
    selection?.kind === 'agent' ? (roster.get(selection.id) ?? null) : null;
  // The engine sends a tail of the transcript, not all of it. The sequence
  // numbers say how much is missing, so the panel can say so instead of showing
  // a partial debate log as if it were the whole one.
  const dropped = useMemo(() => droppedBefore(entries), [entries]);
  const view = useMemo(() => threads(visible, run, filter), [visible, run, filter]);

  useEffect(() => {
    if (pinned) endRef.current?.scrollIntoView({ block: 'nearest' });
  }, [visible.length, pinned]);

  return (
    <Section
      label="Board"
      action={
        <span className="vd-numeric text-[10px] text-[var(--vd-text-tertiary)]">
          {visible.length} of {entries.length}
        </span>
      }
    >
      <div className="mb-2 flex flex-wrap items-center gap-1">
        {FILTERS.map((option) => {
          const count = countFilter(visible, option.key);
          const active = filter === option.key;
          return (
            <button
              key={option.key}
              type="button"
              onClick={() => onFilter(option.key)}
              aria-pressed={active}
              disabled={count === 0 && option.key !== 'all'}
              className={`vd-chip !h-[17px] !text-[9px] transition-colors duration-[var(--vd-dur-1)] ${
                active ? 'vd-chip-accent' : ''
              } ${count === 0 && option.key !== 'all' ? 'opacity-40' : ''}`}
              title={
                count === 0 && option.key !== 'all'
                  ? `Nothing on the board is a ${option.label} entry`
                  : `Show ${option.label}`
              }
            >
              {option.label}
              <span className="vd-numeric ml-1 opacity-70">{count}</span>
            </button>
          );
        })}
      </div>

      {dropped > 0 && (
        <p className="m-0 mb-2 text-[9.5px] leading-[1.45] text-[var(--vd-text-tertiary)]">
          Showing the last {entries.length} entries. The {dropped} before them are on disk
          but not in this view.
        </p>
      )}

      {selection && (
        <div className="mb-2 flex items-center gap-1.5 rounded-[var(--radius-sm)] border border-[var(--vd-accent)] bg-[var(--vd-accent-wash)] px-2 py-1">
          <Focus size={10} strokeWidth={2} className="shrink-0 text-[var(--vd-accent)]" />
          <span className="min-w-0 flex-1 truncate text-[10px] text-[var(--vd-text-secondary)]">
            {describeSelection(selection, run)}
          </span>
          <button
            type="button"
            onClick={() => onSelect(null)}
            className="vd-icon-btn !h-[16px] !w-[16px]"
            title="Show everything again"
          >
            <X size={9} strokeWidth={2} />
          </button>
        </div>
      )}

      {selectedMember && <LiveActivity member={selectedMember} />}

      {view.length === 0 ? (
        <Empty>
          {entries.length === 0
            ? 'Nothing on the board yet. Members write here as they work, and the engine appends what they wrote in arrival order.'
            : selection
              ? 'Nothing on the board is about that. Clear the selection to see the rest.'
              : 'No entry on the board matches that filter.'}
        </Empty>
      ) : (
        <div
          className="max-h-[26rem] overflow-y-auto"
          onScroll={(event) => {
            const el = event.currentTarget;
            setPinned(el.scrollHeight - el.scrollTop - el.clientHeight < 24);
          }}
        >
          <div className="flex flex-col gap-2">
            {view.map((thread) => (
              <div key={thread.task ?? 'run'}>
                <div className="flex items-center gap-1.5">
                  <span className="vd-label !text-[9px]">{thread.task ?? 'run-wide'}</span>
                  <span className="h-px flex-1 bg-[var(--vd-line-soft)]" />
                  <span className="vd-numeric text-[9px] text-[var(--vd-text-tertiary)]">
                    {thread.entries.length}
                  </span>
                </div>
                <div className="mt-1 flex flex-col gap-1">
                  {thread.blocks.map((block, index) =>
                    block.kind === 'exchange' ? (
                      <ExchangeBlock
                        key={`x-${block.exchange.opener.seq}`}
                        exchange={block.exchange}
                        roster={roster}
                      />
                    ) : (
                      <div key={`e-${block.entry.seq}`} className="vd-enter" style={{ ['--vd-i' as string]: index }}>
                        <EntryLine entry={block.entry} roster={roster} />
                      </div>
                    ),
                  )}
                </div>
              </div>
            ))}
          </div>
          <div ref={endRef} />
        </div>
      )}
    </Section>
  );
}

/* ------------------------------------------------------------------ *
 * The graph
 * ------------------------------------------------------------------ */

/**
 * The dependency DAG, and the lanes it actually ran in.
 *
 * Laid out by depth, not by declaration order: a task's column is the longest
 * chain of dependencies behind it, so an edge always points right and the
 * critical path is the widest row of the picture. That is the one property worth
 * drawing — it is the floor on wall-clock, and no amount of concurrency beats it.
 *
 * A node is a button. Selecting one narrows the board to that task and marks the
 * roster group, which is the whole reason the three surfaces share a selection:
 * the graph says where, the roster says who, and the board says what was said.
 */
function Graph({
  run,
  selection,
  onSelect,
}: {
  run: Run;
  selection: Selection;
  onSelect: (next: Selection) => void;
}) {
  const view = useMemo(() => graphView(run), [run]);
  const placed = useMemo(
    () => new Map(view.nodes.map((node) => [node.task.id, node])),
    [view.nodes],
  );
  // The nodes whose task is running. `vd-aura` is a `::before` sweep, and SVG
  // elements do not generate pseudo-elements — so the aura is drawn as an HTML
  // layer over the node rather than as a class on the `<g>`, which would be a
  // class that silently does nothing. Same class, same token, same 2.8s: the
  // shape of the run and the place it is working are the same picture.
  const running = view.nodes.filter((node) => node.task.state === 'running');

  if (run.tasks.length === 0) {
    return <Empty>No plan yet. The lead writes one, and it appears here.</Empty>;
  }

  return (
    <div>
      <div className="relative overflow-x-auto">
        <svg
          width={view.width}
          height={view.height}
          viewBox={`0 0 ${view.width} ${view.height}`}
          className="block"
          role="group"
          aria-label="Task dependency graph"
        >
          {view.links.map((link) => {
            const from = placed.get(link.from);
            const to = placed.get(link.to);
            if (!from || !to) return null;
            const x1 = from.x + NODE_W;
            const y1 = from.y + NODE_H / 2;
            const x2 = to.x;
            const y2 = to.y + NODE_H / 2;
            const mid = (x1 + x2) / 2;
            return (
              <path
                key={`${link.from}-${link.to}`}
                d={`M ${x1} ${y1} C ${mid} ${y1}, ${mid} ${y2}, ${x2} ${y2}`}
                fill="none"
                stroke={
                  link.live
                    ? 'var(--vd-accent)'
                    : link.critical
                      ? 'var(--vd-line-strong)'
                      : 'var(--vd-line)'
                }
                strokeWidth={link.live ? 1.6 : link.critical ? 1.4 : 1}
                strokeDasharray={link.critical ? undefined : '3 3'}
                className={link.live ? 'vd-swarm-edge-live' : undefined}
              />
            );
          })}

          {view.nodes.map((node) => {
            const selected = taskSelected(selection, node.task.id);
            const stroke = selected
              ? 'var(--vd-accent)'
              : node.tone === 'live'
                ? 'var(--vd-accent)'
                : node.tone === 'good'
                  ? 'var(--vd-ok)'
                  : node.tone === 'bad'
                    ? 'var(--vd-danger)'
                    : 'var(--vd-line)';
            return (
              <g
                key={node.task.id}
                role="button"
                tabIndex={0}
                aria-pressed={selected}
                aria-label={`${node.task.id}, ${node.task.title}, ${describeTask(node.task, run.tasks)}`}
                onClick={() =>
                  onSelect(selected ? null : { kind: 'task', id: node.task.id })
                }
                onKeyDown={(event) => {
                  if (event.key !== 'Enter' && event.key !== ' ') return;
                  event.preventDefault();
                  onSelect(selected ? null : { kind: 'task', id: node.task.id });
                }}
                className="cursor-pointer outline-none focus-visible:opacity-80"
              >
                <rect
                  x={node.x}
                  y={node.y}
                  width={NODE_W}
                  height={NODE_H}
                  rx={5}
                  fill={selected ? 'var(--vd-accent-wash)' : 'var(--vd-surface)'}
                  stroke={stroke}
                  strokeWidth={selected || node.critical ? 1.6 : node.tone === 'idle' ? 1 : 1.4}
                />
                <text
                  x={node.x + 8}
                  y={node.y + 15}
                  fill="var(--vd-text)"
                  fontSize={10}
                  fontFamily="var(--vd-font-mono, monospace)"
                >
                  {node.task.id}
                </text>
                <text x={node.x + 8} y={node.y + 29} fill="var(--vd-text-tertiary)" fontSize={9}>
                  {node.task.title.length > 20
                    ? `${node.task.title.slice(0, 19)}…`
                    : node.task.title}
                </text>
                {node.duration > 0 && (
                  <text
                    x={node.x + 8}
                    y={node.y + 40}
                    fill="var(--vd-text-tertiary)"
                    fontSize={8}
                    fontFamily="var(--vd-font-mono, monospace)"
                  >
                    {formatDuration(node.duration)}
                    {node.task.attempts > 1 ? ` · ${node.task.attempts} tries` : ''}
                  </text>
                )}
                <circle
                  cx={node.x + NODE_W - 9}
                  cy={node.y + 11}
                  r={3}
                  fill={
                    node.tone === 'live'
                      ? 'var(--vd-accent)'
                      : node.tone === 'good'
                        ? 'var(--vd-ok)'
                        : node.tone === 'bad'
                          ? 'var(--vd-danger)'
                          : 'var(--vd-text-tertiary)'
                  }
                />
                {node.critical && (
                  <circle
                    cx={node.x + NODE_W - 9}
                    cy={node.y + 11}
                    r={5.5}
                    fill="none"
                    stroke="var(--vd-accent)"
                    strokeWidth={0.8}
                    opacity={0.5}
                  />
                )}
              </g>
            );
          })}
        </svg>
        {running.map((node) => (
          <span
            key={`aura-${node.task.id}`}
            aria-hidden
            className="vd-aura pointer-events-none rounded-[5px]"
            // `position` is inline rather than a utility: `.vd-aura` sets
            // `position: relative` and it is unlayered, so it beats Tailwind's
            // layered `.absolute` whatever the source order. An inline style is
            // the one thing that wins, and a class that silently did nothing
            // would leave the aura in the document flow.
            style={{
              position: 'absolute',
              left: node.x,
              top: node.y,
              width: NODE_W,
              height: NODE_H,
            }}
          />
        ))}
      </div>

      <Timeline run={run} view={view} selection={selection} onSelect={onSelect} />

      <div className="mt-2 flex items-center gap-1.5">
        <span className="vd-label !text-[9px]">critical path</span>
        <span className="h-px flex-1 bg-[var(--vd-line-soft)]" />
        <span className="vd-numeric text-[9.5px] text-[var(--vd-text-tertiary)]">
          {criticalPathLabel(view.criticalPath)}
        </span>
      </div>
    </div>
  );
}

/**
 * The run's wall clock, at member resolution.
 *
 * The task lanes answered "how wide did the run actually go". This answers the
 * question a slow run raises: which member is eating it. A task's worker and its
 * verifier are one bar on the task lanes and two rows here, because a verifier
 * that took twenty minutes against a worker's six is the single most useful
 * thing to know about a run that is running long — and the task lanes cannot
 * show it, because the task's span covers both.
 *
 * A row is a button, and selecting one selects the member: the graph says where,
 * the roster says who, and the timeline says how long. All three are one picture
 * seen three ways, so a click on any of them has to mean the same thing.
 *
 * The critical path is marked by the row's own track rather than by a second
 * colour: a critical row sits on the accent wash and its id is in accent, so the
 * chain that sets the floor on wall-clock is the chain the eye follows down the
 * list. No new token, no gradient, no shadow.
 */
function Timeline({
  run,
  view,
  selection,
  onSelect,
}: {
  run: Run;
  view: GraphView;
  selection: Selection;
  onSelect: (next: Selection) => void;
}) {
  const timeline = useMemo(() => memberTimeline(run), [run]);
  if (!timeline.window) return null;
  const { start, end } = timeline.window;
  const span = Math.max(1, end - start);

  return (
    <div className="mt-2">
      <div className="flex items-center gap-1.5">
        <span className="vd-label !text-[9px]">timeline</span>
        <span className="h-px flex-1 bg-[var(--vd-line-soft)]" />
        <span className="vd-numeric text-[9.5px] text-[var(--vd-text-tertiary)]">
          {/* The width is the task lanes' figure, kept because it is the one
              thing the member rows cannot say: how much of the plan actually ran
              at once. It is absent while the lead is still planning, because no
              task has taken a lane yet. */}
          {view.lanes.length > 0 ? `${view.lanes.length} wide · ` : ''}
          {formatDuration(timeline.elapsed)}
        </span>
      </div>
      <div className="relative mt-1 flex flex-col gap-[3px]">
        {timeline.rows.map((row) => {
          const selected = agentSelected(selection, row.member);
          return (
            <button
              key={row.member}
              type="button"
              onClick={() =>
                onSelect(selected ? null : { kind: 'agent', id: row.member })
              }
              aria-pressed={selected}
              title={`${row.member} · ${roleLabel(row.role)}${
                row.task ? ` · ${row.task}` : ''
              } · ${
                row.spans.length === 0
                  ? 'no turns yet'
                  : `${formatDuration(row.secs)} in ${row.spans.length} ${
                      row.spans.length === 1 ? 'turn' : 'turns'
                    }`
              }${row.critical ? ' · on the critical path' : ''}`}
              className="flex w-full items-center gap-1 text-left"
            >
              <span
                className={`w-[34px] shrink-0 truncate font-mono text-[8.5px] ${
                  selected || row.critical
                    ? 'text-[var(--vd-accent)]'
                    : 'text-[var(--vd-text-tertiary)]'
                }`}
              >
                {row.member}
              </span>
              <span
                className={`relative h-[15px] flex-1 overflow-hidden rounded-[3px] ${
                  row.critical
                    ? 'bg-[var(--vd-accent-wash)]'
                    : 'bg-[var(--vd-surface-sunken)]'
                }`}
              >
                {row.spans.map((drawn, index) => {
                  const left = ((drawn.start - start) / span) * 100;
                  const width = Math.max(1.5, ((drawn.end - drawn.start) / span) * 100);
                  return (
                    <span
                      key={`${drawn.start}-${index}`}
                      className={`absolute top-0 h-full rounded-[2px] transition-[background-color] duration-[var(--vd-dur-1)] ${
                        selected
                          ? 'bg-[var(--vd-accent)]'
                          : drawn.live
                            ? 'bg-[var(--vd-accent)]/70'
                            : row.critical
                              ? 'bg-[var(--vd-accent)]/45'
                              : 'bg-[var(--vd-line-strong)]'
                      }`}
                      style={{ left: `${left}%`, width: `${width}%` }}
                    />
                  );
                })}
              </span>
            </button>
          );
        })}
        {/* The current moment. One line across every row rather than a tick per
            row, so the gap between a member's last span and now reads as one
            distance instead of twenty-one. */}
        {timeline.now !== null && (
          <span
            aria-hidden
            className="pointer-events-none absolute top-0 bottom-0 w-px bg-[var(--vd-accent)]"
            style={{ left: `calc(38px + ${timeline.now} * (100% - 38px))` }}
          />
        )}
      </div>
    </div>
  );
}

/* ------------------------------------------------------------------ *
 * The plan, before approval
 * ------------------------------------------------------------------ */

function PlanReview({
  run,
  onApprove,
  onRehearse,
  rehearsal,
  busy,
  external,
}: {
  run: Run;
  onApprove: () => void;
  onRehearse: () => void;
  rehearsal: RehearsalView | null;
  busy: boolean;
  /** The run is driven by another process, so approving it here would be a lie. */
  external: boolean;
}) {
  // The skills this run authored. They are in the run's catalogue with the
  // `run` scope, which is what distinguishes them from the machine's own.
  const written = run.skills.filter((skill) => skill.scope === 'run');

  return (
    <>
      <Section
        label="Plan"
        action={
          <span className="vd-numeric text-[10px] text-[var(--vd-text-tertiary)]">
            {run.tasks.length} tasks
          </span>
        }
      >
        <ul className="m-0 flex list-none flex-col gap-1.5 p-0">
          {run.tasks.map((task, index) => (
            <li
              key={task.id}
              className="vd-enter rounded-[var(--radius-sm)] border border-[var(--vd-line-soft)] px-2 py-1.5"
              style={{ ['--vd-i' as string]: index }}
            >
              <div className="flex items-center gap-1.5">
                <span className="font-mono text-[10px] text-[var(--vd-text-tertiary)]">
                  {task.id}
                </span>
                <span className="truncate text-[11.5px] text-[var(--vd-text)]">{task.title}</span>
                {task.deps.length > 0 && (
                  <span className="ml-auto shrink-0 text-[9px] text-[var(--vd-text-tertiary)]">
                    after {task.deps.join(', ')}
                  </span>
                )}
              </div>
              <div className="mt-1 flex flex-wrap items-center gap-1">
                <span
                  className="vd-chip !h-[15px] !text-[8.5px]"
                  style={{
                    color:
                      taskTone(task.state) === 'live'
                        ? 'var(--vd-accent)'
                        : taskTone(task.state) === 'good'
                          ? 'var(--vd-ok)'
                          : taskTone(task.state) === 'bad'
                            ? 'var(--vd-danger)'
                            : 'var(--vd-text-tertiary)',
                  }}
                  title={describeTask(task, run.tasks)}
                >
                  {task.state}
                </span>
                {task.owner && (
                  <span className="vd-chip !h-[15px] !text-[8.5px]" title="the worker that owns it">
                    {task.owner}
                  </span>
                )}
                {task.attempts > 1 && (
                  <span
                    className="vd-chip !h-[15px] !text-[8.5px]"
                    title="how many times a worker has tried this task"
                  >
                    try {task.attempts}
                  </span>
                )}
                {task.verdict && (
                  <span
                    className="vd-chip !h-[15px] !text-[8.5px]"
                    style={{
                      color: task.verdict.passed ? 'var(--vd-ok)' : 'var(--vd-danger)',
                    }}
                    title={task.verdict.reason}
                  >
                    {task.verdict.passed ? 'verdict: pass' : 'verdict: fail'}
                  </span>
                )}
                {task.baseCommit && (
                  <span
                    className="vd-chip !h-[15px] !text-[8.5px]"
                    title={
                      'The commit this task started from: the run base with its ' +
                      'dependencies merged in. The base side of its failing-first proof.'
                    }
                  >
                    from {task.baseCommit.slice(0, 7)}
                  </span>
                )}
              </div>
              <p className="m-0 mt-1 text-[10.5px] leading-[1.45] text-[var(--vd-text-secondary)]">
                {task.detail}
              </p>
              <p className="m-0 mt-1 text-[10px] leading-[1.45] text-[var(--vd-text-tertiary)]">
                <span className="text-[var(--vd-text-secondary)]">acceptance:</span>{' '}
                {task.acceptance}
              </p>
              {task.files.length > 0 && (
                <div className="mt-1 flex flex-wrap gap-1">
                  {task.files.map((file) => (
                    <span key={file} className="vd-chip !h-[15px] !text-[8.5px]">
                      {file}
                    </span>
                  ))}
                </div>
              )}
              {/* The method this task will be handed. Shown before approval
                  because it is part of what the user is approving: a task with
                  a skill in its brief is a longer, more expensive turn than one
                  without, and the difference should be visible here rather than
                  discovered in the ledger. */}
              {task.skills.length > 0 && (
                <div className="mt-1 flex flex-wrap items-center gap-1">
                  <span className="text-[9px] uppercase tracking-[0.08em] text-[var(--vd-text-tertiary)]">
                    method
                  </span>
                  {task.skills.map((name) => (
                    <span
                      key={name}
                      className="vd-chip vd-chip-accent !h-[15px] !text-[8.5px]"
                      title={`The worker loads ${name} before it writes`}
                    >
                      {name}
                    </span>
                  ))}
                </div>
              )}
              {task.outOfGrant.length > 0 && (
                <div className="mt-1 flex flex-wrap items-center gap-1">
                  <span
                    className="text-[9px] uppercase tracking-[0.08em]"
                    style={{ color: 'var(--vd-danger)' }}
                  >
                    outside the grant
                  </span>
                  {task.outOfGrant.map((file) => (
                    <span
                      key={file}
                      className="vd-chip !h-[15px] !text-[8.5px]"
                      style={{ color: 'var(--vd-danger)' }}
                      title="The worker committed this and the plan did not grant it. The verifier fails the change for it."
                    >
                      {file}
                    </span>
                  ))}
                </div>
              )}
              {task.reconcile.length > 0 && (
                <div className="mt-1 flex flex-wrap items-center gap-1">
                  <span
                    className="text-[9px] uppercase tracking-[0.08em]"
                    style={{ color: 'var(--vd-accent)' }}
                  >
                    reconciling
                  </span>
                  {task.reconcile.map((file) => (
                    <span
                      key={file}
                      className="vd-chip !h-[15px] !text-[8.5px]"
                      style={{ color: 'var(--vd-accent)' }}
                      title="Its dependencies disagreed about this file. The engine kept the incoming dependency's version and making it right is this task's work."
                    >
                      {file}
                    </span>
                  ))}
                </div>
              )}
              {task.conflict && (
                <p
                  className="m-0 mt-1 text-[10px] leading-[1.45]"
                  style={{ color: 'var(--vd-danger)' }}
                >
                  <span className="text-[var(--vd-text-secondary)]">could not start:</span>{' '}
                  {task.conflict}
                  {task.conflictRepairs > 0 && (
                    <span className="text-[var(--vd-text-tertiary)]">
                      {' '}
                      — the lead has been asked {task.conflictRepairs}{' '}
                      {task.conflictRepairs === 1 ? 'time' : 'times'}
                    </span>
                  )}
                </p>
              )}
            </li>
          ))}
        </ul>

        {/* The skills the lead wrote for this run. They are not installed: they
            live in the run's directory and are handed to this run's members, and
            keeping one is a click after the run. Shown at approval because a plan
            that writes a method is a plan that decided the machine was missing
            one, and that is a decision the user is entitled to see. */}
        {written.length > 0 && (
          <div className="mt-2 rounded-[var(--radius-sm)] border border-dashed border-[var(--vd-line-strong)] px-2 py-1.5">
            <div className="vd-label !text-[9px]">this plan writes</div>
            <ul className="m-0 mt-1 flex list-none flex-col gap-1 p-0">
              {written.map((skill) => (
                <li key={skill.name}>
                  <div className="flex items-center gap-1.5">
                    <span className="min-w-0 flex-1 truncate font-mono text-[10.5px] text-[var(--vd-text-secondary)]">
                      {skill.name}
                    </span>
                    <span className="vd-chip !h-[15px] !text-[8.5px]">run only</span>
                  </div>
                  <p className="m-0 mt-0.5 text-[10px] leading-[1.45] text-[var(--vd-text-tertiary)]">
                    {skill.description}
                  </p>
                  <p
                    className="m-0 mt-0.5 break-all font-mono text-[9px] leading-[1.45] text-[var(--vd-text-tertiary)]"
                    title="Where the run wrote it. It is deleted with the run unless you keep it."
                  >
                    {skill.path}
                  </p>
                </li>
              ))}
            </ul>
          </div>
        )}
      </Section>

      <Section
        label="Rehearsal"
        action={
          <button
            type="button"
            onClick={onRehearse}
            disabled={busy}
            className="vd-btn vd-btn-ghost !h-[22px] !px-1.5"
          >
            <Layers size={11} strokeWidth={2} />
            Rehearse
          </button>
        }
      >
        {rehearsal ? (
          <RehearsalCard view={rehearsal} />
        ) : (
          <Empty>
            Rehearse to see the waves, the critical path and the token estimate before
            anything is spawned.
          </Empty>
        )}
      </Section>

      <Section label="Approve">
        {/* The shape the plan was accepted with. Not a refusal — a lead cannot
            make a genuinely sequential plan parallel without lying about its
            dependencies, so a refusal here is a loop it cannot escape. The
            decision is the user's, and the button below is the approve-anyway. */}
        {run.shapeWarnings.length > 0 && (
          <div
            className="mb-2 rounded-[var(--radius-sm)] border px-2 py-1.5"
            style={{
              borderColor: 'color-mix(in oklab, var(--vd-warn) 32%, transparent)',
              background: 'var(--vd-warn-wash)',
            }}
          >
            <div className="vd-label !text-[9px]" style={{ color: 'var(--vd-warn)' }}>
              accepted with a warning
            </div>
            <ul className="m-0 mt-1 flex list-none flex-col gap-1 p-0">
              {run.shapeWarnings.map((warning) => (
                <li
                  key={warning.kind + ('task' in warning ? warning.task : '')}
                  className="flex items-start gap-1.5 text-[10.5px] leading-[1.45]"
                  style={{ color: 'var(--vd-text-secondary)' }}
                >
                  <TriangleAlert
                    size={11}
                    strokeWidth={2}
                    className="mt-0.5 shrink-0"
                    style={{ color: 'var(--vd-warn)' }}
                  />
                  {shapeWarningMessage(warning)}
                </li>
              ))}
            </ul>
          </div>
        )}
        <p className="m-0 text-[11px] leading-[1.5] text-[var(--vd-text-secondary)]">
          {external
            ? 'This run is driven by another process. Approving it here would write a plan the driver never reads, so the decision belongs where the run was started.'
            : 'Approving spawns one process per task, each in its own worktree, plus a verifier on a different model. Nothing is merged until every task is verified.'}
        </p>
        <button
          type="button"
          onClick={onApprove}
          disabled={busy}
          className="vd-swarm-approval vd-btn vd-btn-primary mt-2 w-full justify-center"
          title={external ? 'Driven by another process — approve it there' : undefined}
        >
          <Play size={12} strokeWidth={2} />
          {external
            ? 'Approve where it was started'
            : run.shapeWarnings.length > 0
              ? 'Approve anyway and run'
              : 'Approve and run'}
        </button>
      </Section>
    </>
  );
}

/* ------------------------------------------------------------------ *
 * The run
 * ------------------------------------------------------------------ */

function RunHeader({
  run,
  external,
  stopRequested,
}: {
  run: Run;
  external: boolean;
  /** A stop has been asked of the driver and not yet answered. */
  stopRequested: boolean;
}) {
  const tone = runTone(run.state);
  const done = run.tasks.filter((t) => t.state === 'verified').length;
  const fraction = progress(run);
  // The ceiling counts what the run was billed for, not the traffic: a member
  // re-sends its whole context every turn, so the wire figure runs an order of
  // magnitude ahead of the spend.
  const spent = Math.max(0, run.budget.usedTokens - run.budget.cachedTokens);

  return (
    <Section
      label="Run"
      action={
        <span className="flex items-center gap-1">
          {external && (
            <span
              className="vd-chip !h-[16px] !text-[9px]"
              title="Another process is driving this run. The panel reads it, and Stop asks that process to stop it."
            >
              external
            </span>
          )}
          <Chip tone={tone}>{run.state}</Chip>
        </span>
      }
    >
      <p className="m-0 text-[11.5px] leading-[1.5] text-[var(--vd-text)]">{run.goal}</p>

      {/* Who is driving, in the header rather than in a tooltip. A run outlives
          the window that started it, so the panel can be showing work it cannot
          drive — and a Stop button that refuses is the moment that has to be
          said out loud. */}
      {external && (
        <p className="m-0 mt-1.5 flex items-start gap-1.5 text-[10.5px] leading-[1.45] text-[var(--vd-text-tertiary)]">
          <Users size={10} strokeWidth={2} className="mt-0.5 shrink-0" />
          {stopRequested
            ? 'Stop requested. The process driving this run checks for the request every tick; this panel will show the run stop when it does.'
            : 'Another process is driving this run. The panel reads it, and Stop asks that process to stop it.'}
        </p>
      )}

      <div className="mt-2 flex h-1.5 overflow-hidden rounded-full bg-[var(--vd-surface-sunken)]">
        <span
          className="h-full w-full origin-left bg-[var(--vd-accent)] transition-transform duration-[var(--vd-slow)] ease-[var(--vd-ease)]"
          style={{ transform: `scaleX(${fraction})` }}
        />
      </div>
      <div className="mt-1 flex items-baseline justify-between">
        <span className="vd-numeric text-[10.5px] text-[var(--vd-text-secondary)]">
          {done} of {run.tasks.length} verified
        </span>
        <span className="vd-numeric text-[10px] text-[var(--vd-text-tertiary)]">
          {formatTokens(spent)} / {formatTokens(run.budget.maxTokens)} tokens
        </span>
      </div>

      <div className="mt-2 grid grid-cols-2 gap-1.5">
        <Stat label="processes" value={`${liveProcesses(run)} / ${run.concurrency}`} />
        <Stat label="elapsed" value={formatDuration(elapsed(run))} />
      </div>

      {/* What the run cost, in the three numbers that answer three questions:
          the bill, whether it was cheap for the work, and how long it took. */}
      <Spend run={run} />

      {/* The workspace, because a run can be in a folder that is not the one the
          window has open — an adopted run almost always is. */}
      <p
        className="m-0 mt-1.5 truncate font-mono text-[9.5px] text-[var(--vd-text-tertiary)]"
        title={run.workspace}
      >
        {run.workspace}
      </p>

      {run.failure && (
        <p className="m-0 mt-2 flex items-start gap-1.5 text-[11px] leading-[1.45] text-[var(--vd-danger)]">
          <CircleAlert size={11} strokeWidth={2} className="mt-0.5 shrink-0" />
          {run.failure}
        </p>
      )}
    </Section>
  );
}

/**
 * What the run cost, and who spent it.
 *
 * A swarm's whole promise is that it is faster than one agent, and its whole
 * risk is that it is fifteen times the tokens. Neither claim is checkable
 * without this: the total says what the run cost, the cache share says whether
 * that cost bought anything, and the per-member bars say which role is eating
 * the budget — usually the lead, because it re-reads the plan on every tick.
 *
 * The bars are shares of the largest member, not of the total, so a run with one
 * expensive member still shows the cheap ones as something other than a sliver.
 */
/* ------------------------------------------------------------------ *
 * Spend
 * ------------------------------------------------------------------ */

/**
 * What the run cost, and who spent it.
 *
 * A swarm's whole promise is that it is faster than one agent, and its whole
 * risk is that it is fifteen times the tokens. Neither claim is checkable
 * without this: the total says what the run cost, the cache share says whether
 * that cost bought anything, and the per-role split says which role is eating
 * the budget — usually the lead, because it re-reads the plan on every tick.
 *
 * The member bars are shares of the largest member, not of the total, so a run
 * with one expensive member still shows the cheap ones as something other than a
 * sliver.
 */
function Spend({ run }: { run: Run }) {
  const view = useMemo(() => spendView(run), [run]);
  const method = useMemo(() => methodCoverage(run), [run]);

  // Nothing spent yet is not a zero-cost run, it is no measurement. Saying
  // "0 tokens" before the first turn reads as a fact and is not one.
  if (view.members.length === 0) return null;

  return (
    <div className="mt-2 rounded-[var(--radius-sm)] border border-[var(--vd-line-soft)] px-2 py-1.5">
      <div className="flex items-baseline justify-between">
        <span className="vd-label !text-[9px]">Spend</span>
        <span className="vd-numeric text-[9.5px] text-[var(--vd-text-tertiary)]">
          {view.turns} {view.turns === 1 ? 'turn' : 'turns'}
        </span>
      </div>

      <div className="mt-1 flex items-baseline justify-between gap-2">
        <span className="vd-numeric text-[12px] text-[var(--vd-text)]">
          {formatTokens(view.tokens)} tokens
        </span>
        <span className="vd-numeric text-[10px] text-[var(--vd-text-secondary)]">
          {formatDuration(view.wallSecs)} wall
        </span>
      </div>

      {/* The cache bar. A cached input token costs a fraction of a fresh one, so
          this is the closest thing to a discount the run has. */}
      <div className="mt-1.5 flex items-center gap-1.5">
        <span className="h-1 flex-1 overflow-hidden rounded-full bg-[var(--vd-surface-sunken)]">
          <span
            className="block h-full origin-left rounded-full bg-[var(--vd-accent)] transition-transform duration-[var(--vd-slow)] ease-[var(--vd-ease)]"
            style={{ transform: `scaleX(${view.cacheRate})` }}
          />
        </span>
        <span
          className="vd-numeric shrink-0 text-[9.5px] text-[var(--vd-text-tertiary)]"
          title={`${view.cached.toLocaleString()} of ${view.tokens.toLocaleString()} tokens came from the prompt cache`}
        >
          {Math.round(view.cacheRate * 100)}% cached
        </span>
      </div>

      {/* The budget bar, only when there is a budget to be a fraction of. */}
      {view.maxTokens > 0 && (
        <div className="mt-1 flex items-center gap-1.5">
          <span className="h-1 flex-1 overflow-hidden rounded-full bg-[var(--vd-surface-sunken)]">
            <span
              className={`block h-full origin-left rounded-full transition-transform duration-[var(--vd-slow)] ease-[var(--vd-ease)] ${
                view.budget > 0.9 ? 'bg-[var(--vd-danger)]' : 'bg-[var(--vd-text-tertiary)]'
              }`}
              style={{ transform: `scaleX(${view.budget})` }}
            />
          </span>
          <span
            className="vd-numeric shrink-0 text-[9.5px] text-[var(--vd-text-tertiary)]"
            title={`${view.spentTokens.toLocaleString()} of ${view.maxTokens.toLocaleString()} budgeted tokens — ${run.budget.usedTokens.toLocaleString()} crossed the wire, ${run.budget.cachedTokens.toLocaleString()} of them cache reads`}
          >
            {Math.round(view.budget * 100)}% budget
          </span>
        </div>
      )}

      {/* The wall-clock bar. A run that blew its time limit is the failure mode
          the token budget does not catch: it can be cheap and still take all
          night, and nothing else on this panel says so. */}
      {view.maxWallSecs > 0 && (
        <div className="mt-1 flex items-center gap-1.5">
          <span className="h-1 flex-1 overflow-hidden rounded-full bg-[var(--vd-surface-sunken)]">
            <span
              className={`block h-full origin-left rounded-full transition-transform duration-[var(--vd-slow)] ease-[var(--vd-ease)] ${
                view.wallBudget >= 1 ? 'bg-[var(--vd-danger)]' : 'bg-[var(--vd-text-tertiary)]'
              }`}
              style={{ transform: `scaleX(${view.wallBudget})` }}
            />
          </span>
          <span
            className="vd-numeric shrink-0 text-[9.5px] text-[var(--vd-text-tertiary)]"
            title={`${formatDuration(view.wallSecs)} of ${formatDuration(view.maxWallSecs)} allowed`}
          >
            {Math.round(view.wallBudget * 100)}% time
          </span>
        </div>
      )}

      {/* Per role. Three or four rows, so the split is readable at a glance. */}
      <div className="mt-1.5 flex flex-col gap-0.5">
        {view.roles.map((role) => (
          <div key={role.role} className="flex items-center gap-1.5">
            <span className="w-[46px] shrink-0 text-[9.5px] text-[var(--vd-text-secondary)]">
              {role.label}
            </span>
            <span className="h-1 flex-1 overflow-hidden rounded-full bg-[var(--vd-surface-sunken)]">
              <span
                className="block h-full origin-left rounded-full bg-[var(--vd-accent)]/60 transition-transform duration-[var(--vd-slow)] ease-[var(--vd-ease)]"
                style={{ transform: `scaleX(${role.share})` }}
              />
            </span>
            <span className="vd-numeric w-[42px] shrink-0 text-right text-[9.5px] text-[var(--vd-text-tertiary)]">
              {formatTokens(role.tokens)}
            </span>
            <span className="vd-numeric w-[26px] shrink-0 text-right text-[9px] text-[var(--vd-text-tertiary)]">
              ×{role.members}
            </span>
          </div>
        ))}
      </div>

      <ul className="m-0 mt-1.5 flex list-none flex-col gap-1 border-t border-[var(--vd-line-soft)] p-0 pt-1.5">
        {view.members.map((row, index) => (
          <li
            key={row.id}
            className="vd-enter flex items-center gap-1.5"
            style={{ ['--vd-i' as string]: index }}
          >
            <span className="w-[52px] shrink-0 truncate font-mono text-[9.5px] text-[var(--vd-text-secondary)]">
              {row.id}
            </span>
            <span className="h-1 flex-1 overflow-hidden rounded-full bg-[var(--vd-surface-sunken)]">
              <span
                className="block h-full origin-left rounded-full bg-[var(--vd-text-tertiary)] transition-transform duration-[var(--vd-slow)] ease-[var(--vd-ease)]"
                style={{ transform: `scaleX(${row.share})` }}
              />
            </span>
            <span className="vd-numeric w-[46px] shrink-0 text-right text-[9.5px] text-[var(--vd-text-tertiary)]">
              {formatTokens(row.tokens)}
            </span>
            <span className="vd-numeric w-[34px] shrink-0 text-right text-[9.5px] text-[var(--vd-text-tertiary)]">
              {row.secs > 0 ? formatDuration(row.secs) : '—'}
            </span>
          </li>
        ))}
      </ul>

      {/* The one efficiency number a swarm has. A run that spent 400k tokens to
          verify two tasks is a different animal from one that spent 40k to
          verify twenty, and the total alone cannot tell them apart. */}
      {view.perVerified !== null && (
        <p className="m-0 mt-1.5 border-t border-[var(--vd-line-soft)] pt-1.5 text-[9.5px] text-[var(--vd-text-tertiary)]">
          <span className="vd-numeric text-[var(--vd-text-secondary)]">
            {formatTokens(view.perVerified)}
          </span>{' '}
          tokens per verified task, across {view.verified}{' '}
          {view.verified === 1 ? 'task' : 'tasks'}
        </p>
      )}

      {/* Method coverage. The comparison a skill plane creates is this run
          against the same goal run bare, so the ledger has to say how much of
          the plan was actually worked with a method — and how much of that was
          assigned and never opened, which is the cost of a longer brief with
          none of the benefit. */}
      {method.assigned > 0 && (
        <p className="m-0 mt-1.5 border-t border-[var(--vd-line-soft)] pt-1.5 text-[9.5px] text-[var(--vd-text-tertiary)]">
          <span className="vd-numeric text-[var(--vd-text-secondary)]">
            {method.opened} of {method.assigned}
          </span>{' '}
          {method.assigned === 1 ? 'task' : 'tasks'} ran with a skill
          {method.unread > 0 && (
            <>
              {' · '}
              <span className="vd-numeric text-[var(--vd-warn)]">{method.unread}</span> assigned
              and never opened
            </>
          )}
        </p>
      )}
    </div>
  );
}

function elapsed(run: Run): number {
  // `endedAt` is stamped once when the run stops, so a finished run reports the
  // time it actually took. `updatedAt` moves on every save, which made a run
  // that had been dead for two hours still read as "still counting".
  const start = new Date(run.budget.startedAt ?? run.createdAt).getTime();
  if (Number.isNaN(start)) return 0;
  const end = run.endedAt
    ? new Date(run.endedAt).getTime()
    : isTerminal(run.state)
      ? new Date(run.updatedAt).getTime()
      : Date.now();
  return Math.max(0, Math.round((end - start) / 1000));
}

function IntegrationCard({ run }: { run: Run }) {
  const integration = run.integration;
  const tone =
    integration.state === 'merged'
      ? 'good'
      : integration.state === 'conflict' || integration.state === 'failed'
        ? 'bad'
        : 'live';

  return (
    <Section label="Integration" action={<Chip tone={tone}>{integration.state}</Chip>}>
      <KeyValue label="branch" value={integration.branch} />
      <KeyValue label="base" value={integration.base.slice(0, 12)} />
      {integration.suite && (
        <KeyValue
          label="suite"
          value={
            <span className="flex items-center gap-1.5">
              {integration.suite}
              {integration.suitePassed === true && (
                <CircleCheck size={10} strokeWidth={2} className="text-[var(--vd-ok)]" />
              )}
              {integration.suitePassed === false && (
                <CircleAlert size={10} strokeWidth={2} className="text-[var(--vd-danger)]" />
              )}
            </span>
          }
        />
      )}
      {integration.merged.length > 0 && (
        <div className="mt-1.5 flex flex-wrap gap-1">
          {integration.merged.map((task) => (
            <span key={task} className="vd-chip !h-[16px] !text-[9px]">
              <GitMerge size={8} strokeWidth={2} className="mr-0.5" />
              {task}
            </span>
          ))}
        </div>
      )}
      {integration.conflict && (
        <div className="mt-1.5 rounded-[var(--radius-sm)] border border-[var(--vd-danger)] px-2 py-1.5">
          <p className="m-0 text-[10.5px] leading-[1.45] text-[var(--vd-danger)]">
            {integration.conflict.detail}
          </p>
          <div className="mt-1 flex flex-wrap gap-1">
            {integration.conflict.files.map((file) => (
              <span key={file} className="vd-chip vd-chip-danger !h-[15px] !text-[8.5px]">
                {file}
              </span>
            ))}
          </div>
        </div>
      )}
    </Section>
  );
}

function ReflectionCard({ reflection }: { reflection: Reflection }) {
  return (
    <Section
      label="Reflection"
      action={
        reflection.degraded ? <Chip tone="idle">degraded</Chip> : <Chip tone="good">complete</Chip>
      }
    >
      <p className="m-0 text-[11px] leading-[1.5] text-[var(--vd-text-secondary)]">
        {reflection.summary}
      </p>
      {reflection.gotchas.length > 0 && (
        <ul className="m-0 mt-2 flex list-none flex-col gap-1 p-0">
          {reflection.gotchas.map((gotcha, index) => (
            <li
              key={`${gotcha.file}-${index}`}
              className="vd-enter rounded-[var(--radius-sm)] border border-[var(--vd-line-soft)] px-2 py-1.5"
              style={{ ['--vd-i' as string]: index }}
            >
              <div className="flex items-center gap-1.5">
                <Sparkles size={10} strokeWidth={2} className="text-[var(--vd-accent)]" />
                <span className="font-mono text-[10px] text-[var(--vd-text-secondary)]">
                  {gotcha.file}
                </span>
              </div>
              <p className="m-0 mt-0.5 text-[10.5px] leading-[1.45] text-[var(--vd-text-secondary)]">
                {gotcha.text}
              </p>
              <p className="m-0 mt-0.5 text-[9px] text-[var(--vd-text-tertiary)]">
                from {gotcha.source}
              </p>
            </li>
          ))}
        </ul>
      )}
      {reflection.degraded && (
        <p className="m-0 mt-2 text-[10px] leading-[1.45] text-[var(--vd-text-tertiary)]">
          {reflection.degraded}
        </p>
      )}
    </Section>
  );
}

/* ------------------------------------------------------------------ *
 * Verdicts
 * ------------------------------------------------------------------ */

/**
 * What each task's verifier actually proved.
 *
 * The verdict is the run's most important artifact and the easiest to fake, so
 * it is shown as the engine recorded it: the command, both exit codes, and
 * whether the failing-first half was observed. A verdict that passed without
 * that observation is marked `claimed`, not `earned` — the difference is the
 * whole point of the adversarial role.
 */
/**
 * The method the run was handed.
 *
 * Three questions, in the order a user asks them:
 *
 *   what did the run think this machine knows   the shortlist, with the terms
 *                                               that matched, so the ranking is
 *                                               arguable rather than magic
 *   what did each task actually get             the assignment, per task, with
 *                                               the skills the members opened
 *                                               filled and the ones they did not
 *                                               outlined
 *   what did the run learn it was missing       the reflection's proposals, each
 *                                               one acceptable into the store
 *
 * The middle one is the reason this section exists. A run that was told to load
 * `systematic-debugging` and never opened it is a run that debugged by instinct,
 * and the roster alone cannot show that: the brief and the transcript look the
 * same from outside. The engine reads the member's own `tool_call`
 * notifications, so a filled chip is evidence and an outlined one is a gap.
 */
function Method({
  run,
  onAccept,
  busy,
}: {
  run: Run;
  onAccept: (name: string) => void;
  busy: boolean;
}) {
  const shortlist = run.skillShortlist ?? [];
  const gaps = run.skillGaps ?? [];
  const proposals = run.skillProposals ?? [];
  const catalogue = run.skills ?? [];
  // The skills this run wrote for itself. They are in the catalogue with the
  // `run` scope, which is what separates them from the machine's own.
  const written = catalogue.filter((skill) => skill.scope === 'run');

  /**
   * Which skills each task's members opened.
   *
   * A task has two members — the worker that wrote it and the verifier that
   * tried to break it — and either may load a skill. The union is the honest
   * reading: the task was worked with the method if anyone on it read it.
   */
  const rows = useMemo(() => {
    const opened = new Map<string, Set<string>>();
    for (const member of run.members) {
      if (!member.task) continue;
      const set = opened.get(member.task) ?? new Set<string>();
      for (const name of member.skillsLoaded ?? []) set.add(name);
      opened.set(member.task, set);
    }
    return orderedTasks(run.tasks)
      .filter((task) => (task.skills ?? []).length > 0)
      // A task nobody has been spawned on has not been worked, so it cannot have
      // skipped a skill. Listing it would report every assignment as unread for
      // the whole of the approval wait.
      .filter((task) => opened.has(task.id))
      .map((task) => ({ task, opened: opened.get(task.id) ?? new Set<string>() }));
  }, [run.tasks, run.members]);

  const unread = rows.reduce(
    (count, row) => count + row.task.skills.filter((name) => !row.opened.has(name)).length,
    0,
  );

  if (catalogue.length === 0 && shortlist.length === 0 && proposals.length === 0) {
    // Two empty cases, two different problems. A run with the switch off was a
    // decision; a run with the switch on and nothing installed is a machine with
    // no method to hand over, and telling the user to flip a switch that is
    // already on would send them looking in the wrong place.
    return (
      <Section label="Method">
        <Empty>
          {run.skillsEnabled
            ? 'This run was handed no skill because none are installed. Every member worked from its brief alone.'
            : 'This run was handed no skill: the switch was off when it started, so every member worked from its brief alone. It is at the top of the Skills panel.'}
        </Empty>
      </Section>
    );
  }

  return (
    <Section
      label="Method"
      action={
        <span className="vd-numeric text-[10px] text-[var(--vd-text-tertiary)]">
          {rows.length} of {run.tasks.length} tasks
        </span>
      }
    >
      {shortlist.length > 0 && (
        <div className="flex flex-wrap items-center gap-1">
          <span className="text-[9.5px] text-[var(--vd-text-tertiary)]">ranked for this goal</span>
          {shortlist.map((match) => (
            <span
              key={match.name}
              className="vd-chip !h-[16px] !text-[9px]"
              title={
                match.why.length > 0
                  ? `matched ${match.why.join(', ')} · score ${match.score.toFixed(1)}`
                  : `score ${match.score.toFixed(1)}`
              }
            >
              {match.name}
            </span>
          ))}
        </div>
      )}

      {written.length > 0 && (
        <div className="mt-1.5 flex flex-wrap items-center gap-1">
          <span className="text-[9.5px] text-[var(--vd-text-tertiary)]">written by this run</span>
          {written.map((skill) => (
            <span
              key={skill.name}
              className="vd-chip !h-[16px] !text-[9px]"
              title={`${skill.path} — deleted with the run unless you keep it`}
            >
              {skill.name}
            </span>
          ))}
        </div>
      )}

      {rows.length > 0 && (
        <ul className="m-0 mt-1.5 flex list-none flex-col gap-1 p-0">
          {rows.map((row, index) => (
            <li
              key={row.task.id}
              className="vd-enter rounded-[var(--radius-sm)] border border-[var(--vd-line-soft)] px-2 py-1.5"
              style={{ ['--vd-i' as string]: index }}
            >
              <div className="flex items-center gap-1.5">
                <span className="vd-label !text-[9px]">{row.task.id}</span>
                <span className="min-w-0 flex-1 truncate text-[10.5px] text-[var(--vd-text-secondary)]">
                  {row.task.title}
                </span>
              </div>
              <div className="mt-1 flex flex-wrap items-center gap-1">
                {row.task.skills.map((name) => {
                  const loaded = row.opened.has(name);
                  return (
                    <span
                      key={name}
                      className={`vd-chip !h-[15px] !text-[8.5px] ${
                        loaded ? 'vd-chip-accent' : 'border-dashed opacity-55'
                      }`}
                      title={
                        loaded
                          ? `${name} · opened by a member on this task`
                          : `${name} · assigned, never opened`
                      }
                    >
                      {name}
                    </span>
                  );
                })}
              </div>
            </li>
          ))}
        </ul>
      )}

      {unread > 0 && (
        <p className="m-0 mt-1.5 flex items-start gap-1.5 text-[10px] leading-[1.45] text-[var(--vd-warn)]">
          <CircleAlert size={10} strokeWidth={2} className="mt-[1px] shrink-0" />
          {unread === 1 ? 'One skill was' : `${unread} skills were`} assigned and never opened.
          The task ran on the brief alone.
        </p>
      )}

      {gaps.length > 0 && (
        <p className="m-0 mt-1.5 flex items-start gap-1.5 text-[10px] leading-[1.45] text-[var(--vd-warn)]">
          <CircleAlert size={10} strokeWidth={2} className="mt-[1px] shrink-0" />
          <span>
            <span className="font-mono">{gaps.join(', ')}</span> — assigned to a task that no
            member loaded.
          </span>
        </p>
      )}

      {proposals.length > 0 && (
        <div className="mt-2">
          <div className="vd-label !text-[9px]">the run says this machine is missing</div>
          <ul className="m-0 mt-1 flex list-none flex-col gap-1 p-0">
            {proposals.map((proposal, index) => (
              <li
                key={proposal.name}
                className="vd-enter rounded-[var(--radius-sm)] border border-[var(--vd-line-soft)] px-2 py-1.5"
                style={{ ['--vd-i' as string]: index }}
              >
                <div className="flex items-center gap-1.5">
                  <span className="min-w-0 flex-1 truncate font-mono text-[10.5px] text-[var(--vd-text-secondary)]">
                    {proposal.name}
                  </span>
                  <button
                    type="button"
                    onClick={() => onAccept(proposal.name)}
                    disabled={busy}
                    className="vd-btn vd-btn-ghost !h-[20px] !px-1.5 !text-[9.5px]"
                    title={`Write this to ~/.vibe/skills/${proposal.name}/SKILL.md`}
                  >
                    <Sparkles size={9} strokeWidth={2} />
                    Accept
                  </button>
                </div>
                <p className="m-0 mt-0.5 text-[10px] leading-[1.45] text-[var(--vd-text-tertiary)]">
                  {proposal.description}
                </p>
                <p className="m-0 mt-0.5 text-[9.5px] leading-[1.45] text-[var(--vd-text-tertiary)]">
                  {proposal.because}
                </p>
              </li>
            ))}
          </ul>
        </div>
      )}
    </Section>
  );
}

/**
 * What the goal would select, before the run is paid for.
 *
 * The engine ranks the catalogue against the goal at start, and this asks the
 * same function the same question while the user is still typing. That is the
 * difference between a switch the user has to trust and one they can see the
 * effect of: a goal about a 3D scene should surface the 3D skills, and if it
 * surfaces nothing the user can fix the goal or the catalogue before spending
 * fifteen times a chat's tokens finding out.
 *
 * Debounced, because the ranking reads the skill directories. A keystroke is not
 * a reason to walk the disk.
 */
function MethodPreview({ goal }: { goal: string }) {
  const [preview, setPreview] = useState<SkillPreview | null>(null);
  const [failed, setFailed] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    const timer = setTimeout(() => {
      void (async () => {
        try {
          const next = await bridge.skills(goal.trim() || undefined);
          if (!cancelled) {
            setPreview(next);
            setFailed(null);
          }
        } catch (error) {
          if (!cancelled) setFailed(errorMessage(error));
        }
      })();
    }, 220);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [goal]);

  if (failed) {
    return (
      <p className="m-0 mt-1.5 text-[10px] leading-[1.45] text-[var(--vd-danger)]">{failed}</p>
    );
  }
  if (!preview) return null;

  if (!preview.enabled) {
    return (
      <p className="m-0 mt-1.5 text-[10px] leading-[1.45] text-[var(--vd-text-tertiary)]">
        Skills are off for runs. Every member will work from its brief alone — the switch is at
        the top of the Skills panel.
      </p>
    );
  }

  const total = preview.catalogue.skills.length;
  if (total === 0) {
    return (
      <p className="m-0 mt-1.5 text-[10px] leading-[1.45] text-[var(--vd-text-tertiary)]">
        No skills are installed, so there is nothing to hand the crew.
      </p>
    );
  }

  return (
    <div className="mt-1.5">
      <div className="flex items-center gap-1.5">
        <span className="vd-label !text-[9px]">method</span>
        <span className="vd-numeric text-[9.5px] text-[var(--vd-text-tertiary)]">
          {total} installed
        </span>
      </div>
      {preview.ranked.length === 0 ? (
        <p className="m-0 mt-1 text-[10px] leading-[1.45] text-[var(--vd-text-tertiary)]">
          Nothing in the catalogue matches this goal yet. The crew will still get the shortlist
          once the lead writes the plan, because a task&apos;s title is a better query than a
          goal.
        </p>
      ) : (
        <div className="mt-1 flex flex-wrap gap-1">
          {preview.ranked.map((match) => (
            <span
              key={match.name}
              className="vd-chip vd-chip-accent !h-[16px] !text-[9px]"
              title={
                match.why.length > 0
                  ? `matched ${match.why.join(', ')} · score ${match.score.toFixed(1)}`
                  : `score ${match.score.toFixed(1)}`
              }
            >
              {match.name}
            </span>
          ))}
        </div>
      )}
    </div>
  );
}

function Verdicts({ run }: { run: Run }) {
  const judged = useMemo(
    () => orderedTasks(run.tasks).filter((task) => task.verdict !== null),
    [run.tasks],
  );
  if (judged.length === 0) return null;

  return (
    <Section
      label="Verdicts"
      action={
        <span className="vd-numeric text-[10px] text-[var(--vd-text-tertiary)]">
          {judged.filter((task) => task.verdict && verdictTone(task.verdict) === 'good').length} of{' '}
          {judged.length} earned
        </span>
      }
    >
      <ul className="m-0 flex list-none flex-col gap-1 p-0">
        {judged.map((task, index) => {
          const verdict = task.verdict!;
          const tone = verdictTone(verdict);
          return (
            <li
              key={task.id}
              className="vd-enter rounded-[var(--radius-sm)] border border-[var(--vd-line-soft)] px-2 py-1.5"
              style={{ ['--vd-i' as string]: index }}
            >
              <div className="flex items-center gap-1.5">
                <span className="vd-label !text-[9px]">{task.id}</span>
                <span className="min-w-0 flex-1 truncate text-[10.5px] text-[var(--vd-text-secondary)]">
                  {task.title}
                </span>
                <Chip tone={tone}>
                  {verdict.baseDirtied
                    ? 'dirty base'
                    : verdict.passed && verdict.failingFirst
                      ? 'earned'
                      : verdict.passed
                        ? 'claimed'
                        : 'failed'}
                </Chip>
              </div>

              {verdict.command ? (
                <div className="mt-1 flex items-start gap-1.5">
                  <span className="mt-[1px] shrink-0 text-[9px] text-[var(--vd-text-tertiary)]">
                    ran
                  </span>
                  <code className="min-w-0 flex-1 break-all font-mono text-[10px] leading-[1.45] text-[var(--vd-text-secondary)]">
                    {verdict.command}
                  </code>
                </div>
              ) : (
                <p className="m-0 mt-1 text-[10px] leading-[1.45] text-[var(--vd-text-tertiary)]">
                  No command named, so failing-first was not tested.
                </p>
              )}

              <div className="mt-1 flex flex-wrap items-center gap-x-2 gap-y-0.5 text-[9.5px] text-[var(--vd-text-tertiary)]">
                <span className="vd-numeric">{exitPair(verdict)}</span>
                {verdict.baseDirtied && (
                  <span className="text-[var(--vd-danger)]">
                    the verifier changed the baseline it measured
                  </span>
                )}
              </div>

              <p className="m-0 mt-1 text-[10.5px] leading-[1.45] text-[var(--vd-text-secondary)]">
                {verdict.reason}
              </p>

              {verdict.evidence.length > 0 && (
                <ul className="m-0 mt-1 flex list-none flex-col gap-0.5 p-0">
                  {verdict.evidence.map((line, i) => (
                    <li
                      key={i}
                      className="break-all font-mono text-[9.5px] leading-[1.45] text-[var(--vd-text-tertiary)]"
                    >
                      {line}
                    </li>
                  ))}
                </ul>
              )}
            </li>
          );
        })}
      </ul>
    </Section>
  );
}

/* ------------------------------------------------------------------ *
 * History
 * ------------------------------------------------------------------ */

function History({
  items,
  onOpen,
  onDelete,
}: {
  items: RunSummary[];
  onOpen: (id: string) => void;
  onDelete: (id: string) => void;
}) {
  if (items.length === 0) {
    return <Empty>No finished runs yet.</Empty>;
  }
  return (
    <ul className="m-0 flex list-none flex-col gap-1 p-0">
      {items.map((item, index) => {
        const tone = runTone(item.state);
        return (
          <li
            key={item.id}
            className="vd-enter flex items-start gap-2 rounded-[var(--radius-sm)] px-1.5 py-1.5 hover:bg-[var(--vd-surface-hover)]"
            style={{ ['--vd-i' as string]: index }}
          >
            <button
              type="button"
              onClick={() => onOpen(item.id)}
              className="min-w-0 flex-1 text-left"
            >
              <div className="flex items-center gap-1.5">
                <Dot tone={tone} />
                <span className="truncate text-[11px] text-[var(--vd-text)]">{item.goal}</span>
              </div>
              <div className="mt-0.5 flex flex-wrap items-center gap-x-2 text-[9.5px] text-[var(--vd-text-tertiary)]">
                <span className={TONE_TEXT[tone]}>{item.state}</span>
                <span className="vd-numeric">
                  {item.verified}/{item.tasks} verified
                </span>
                <span className="vd-numeric">{formatTokens(item.tokens)} tokens</span>
                <span>{relativeTime(item.createdAt)}</span>
              </div>
            </button>
            <button
              type="button"
              onClick={() => onDelete(item.id)}
              className="vd-icon-btn shrink-0"
              title="Delete this run"
            >
              <Trash2 size={11} strokeWidth={2} />
            </button>
          </li>
        );
      })}
    </ul>
  );
}

/* ------------------------------------------------------------------ *
 * The panel
 * ------------------------------------------------------------------ */

export function SwarmPanel() {
  // The three surfaces are one picture seen three ways, so they share one
  // selection: a click on a graph node, a roster card or a board thread means
  // the same thing everywhere.
  //
  // Declared before the read because the read needs it: the feed is two hundred
  // items and only the selected member's is sent, so the selection is part of
  // the question rather than a filter applied to the answer.
  const [selection, setSelection] = useState<Selection>(null);
  const selected = selection?.kind === 'agent' ? selection.id : null;
  const { data, error, busy, refresh } = useSwarm(selected);
  const setPanel = useApp((s) => s.setPanel);

  const [goal, setGoal] = useState('');
  const [concurrency, setConcurrency] = useState(4);
  const [rehearsal, setRehearsal] = useState<RehearsalView | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [working, setWorking] = useState(false);
  const [filter, setFilter] = useState<BoardFilter>('all');
  // Whether a stop has been asked of the process driving an adopted run. Local
  // to this window on purpose: the request is a file the driver reads, and the
  // only thing that can say the run stopped is the run's own state.
  const [stopRequested, setStopRequested] = useState(false);

  // The crew. `choice` is what the user asked for; `picked` is what the engine
  // settled on. They differ whenever a level is refused by the model beside it,
  // and the panel shows the settled one because that is what will run.
  const crew = useCrew();
  const [choice, setChoice] = useState<CrewChoice>(NO_CREW);
  const [picked, setPicked] = useState<RoleModels | null>(null);
  const [defaults, setDefaults] = useState<ThinkingByRole>({
    lead: '',
    worker: '',
    verifier: '',
  });

  useEffect(() => {
    if (!crew.catalog) return;
    let live = true;
    void bridge
      .models(crew.catalog, choice)
      .then((view) => {
        if (!live) return;
        setPicked(view.picked);
        setDefaults(view.defaults);
      })
      .catch(() => {
        // The assignment is a preview. A failed preview leaves the last one on
        // screen rather than blanking the section mid-click.
      });
    return () => {
      live = false;
    };
  }, [crew.catalog, choice]);

  const run = data?.run ?? null;
  const ready = data ? data.preconditions.every((p) => p.ready) : false;

  // A selection names a task or a member of one run. Carrying it into another
  // run would filter the board to an id that no longer exists, which reads as an
  // empty board rather than as a stale filter.
  const runId = run?.id ?? null;
  useEffect(() => {
    setSelection(null);
    setFilter('all');
    setStopRequested(false);
  }, [runId]);
  // A run this process did not start. It is shown in full and cannot be driven
  // from here: the process that owns it is the only writer.
  const external = data?.external ?? false;

  const act = useCallback(
    async (work: () => Promise<unknown>) => {
      setWorking(true);
      setActionError(null);
      try {
        await work();
        await refresh();
      } catch (failure) {
        setActionError(errorMessage(failure));
      } finally {
        setWorking(false);
      }
    },
    [refresh],
  );

  /**
   * Ask the driver to stop, and say only that the request was sent.
   *
   * The flag is set inside `work`, after the call returns: a request that was
   * refused leaves the button alone and puts the refusal in the panel, which is
   * the difference between "asked" and "asked and it worked".
   */
  const requestStop = useCallback(
    () =>
      act(async () => {
        await bridge.requestStop();
        setStopRequested(true);
      }),
    [act],
  );

  const start = useCallback(() => {
    const trimmed = goal.trim();
    if (!trimmed) {
      setActionError('A swarm needs a goal.');
      return;
    }
    void act(async () => {
      await bridge.start(trimmed, concurrency, undefined, crew.catalog ?? undefined, choice);
      setGoal('');
      setRehearsal(null);
    });
  }, [act, choice, concurrency, crew.catalog, goal]);

  const rehearseExample = useCallback(() => {
    void act(async () => {
      setRehearsal(await bridge.rehearseExample(concurrency));
    });
  }, [act, concurrency]);

  const rehearseRun = useCallback(() => {
    if (!run) return;
    const plan: Plan = {
      goal: run.goal,
      tasks: run.tasks,
      integrationSuite: run.integration.suite,
      notes: '',
      // The run already wrote whatever its plan authored, and its catalogue
      // holds them. Re-sending the drafts would write them a second time.
      skillsToWrite: [],
    };
    void act(async () => {
      setRehearsal(await bridge.rehearse(plan, concurrency));
    });
  }, [act, concurrency, run]);

  const approve = useCallback(() => {
    if (!run) return;
    const plan: Plan = {
      goal: run.goal,
      tasks: run.tasks,
      integrationSuite: run.integration.suite,
      notes: '',
      // The run already wrote whatever its plan authored, and its catalogue
      // holds them. Re-sending the drafts would write them a second time.
      skillsToWrite: [],
    };
    void act(async () => {
      await bridge.approve(plan, concurrency, crew.catalog ?? undefined, choice);
      setRehearsal(null);
    });
  }, [act, choice, concurrency, crew.catalog, run]);

  const subtitle = run
    ? `${run.state} · ${run.tasks.length} tasks · ${formatTokens(run.budget.usedTokens)} tokens`
    : data
      ? `${data.capabilitiesOn} of ${data.capabilitiesTotal} capabilities on`
      : 'Reading the run…';

  return (
    <Drawer
      title="Swarm"
      subtitle={subtitle}
      actions={
        <>
          {run && isLive(run.state) && (
            <button
              type="button"
              onClick={() => void (external ? requestStop() : act(() => bridge.stop()))}
              disabled={working || (external && stopRequested)}
              className="vd-btn vd-btn-ghost !h-[22px] !px-1.5"
              title={
                external
                  ? stopRequested
                    ? 'The request is written. The process driving this run checks for it every tick.'
                    : 'Ask the process driving this run to stop it'
                  : 'Stop the run'
              }
            >
              <Square size={10} strokeWidth={2} />
              {external ? (stopRequested ? 'Stop requested' : 'Request stop') : 'Stop'}
            </button>
          )}
          <RefreshButton onClick={() => void refresh()} busy={busy} />
        </>
      }
    >
      {error && (
        <Section label="Error">
          <p className="m-0 text-[11px] text-[var(--vd-danger)]">{error}</p>
        </Section>
      )}

      {actionError && (
        <Section label="Refused">
          <p className="m-0 flex items-start gap-1.5 text-[11px] leading-[1.45] text-[var(--vd-danger)]">
            <CircleAlert size={11} strokeWidth={2} className="mt-0.5 shrink-0" />
            {actionError}
          </p>
        </Section>
      )}

      {data && (
        <Preconditions
          items={data.preconditions}
          onOpenCapabilities={() => setPanel('capabilities')}
        />
      )}

      {/* A live run owns the panel. The start form is not shown beside it: two
          ways to begin a run while one is in flight is two ways to spend. */}
      {run && isLive(run.state) ? (
        <>
          <RunHeader run={run} external={external} stopRequested={stopRequested} />
          <Roster
            run={run}
            board={data?.board ?? []}
            selection={selection}
            onSelect={setSelection}
          />
          <Method
            run={run}
            busy={working || external}
            onAccept={(name) => void act(() => bridge.acceptSkill(run.id, name))}
          />
          <Section label="Graph">
            <Graph run={run} selection={selection} onSelect={setSelection} />
          </Section>
          {/* The detail pane sits above the board, not beside it: the board is
              already filtered to this member, so the pane is the header of what
              follows rather than a second column of the same thing. */}
          {selection?.kind === 'agent' && (
            <MemberDetail
              run={run}
              board={data?.board ?? []}
              memberId={selection.id}
              onClear={() => setSelection(null)}
            />
          )}
          <Board
            entries={data?.board ?? []}
            run={run}
            filter={filter}
            onFilter={setFilter}
            selection={selection}
            onSelect={setSelection}
          />
          {run.state === 'awaiting-approval' && (
            <PlanReview
              run={run}
              onApprove={approve}
              onRehearse={rehearseRun}
              rehearsal={rehearsal}
              busy={working || external}
              external={external}
            />
          )}
          {(run.state === 'integrating' || run.state === 'reflecting') && (
            <IntegrationCard run={run} />
          )}
        </>
      ) : run ? (
        <>
          <RunHeader run={run} external={external} stopRequested={stopRequested} />
          <Roster
            run={run}
            board={data?.board ?? []}
            selection={selection}
            onSelect={setSelection}
          />
          <Method
            run={run}
            busy={working || external}
            onAccept={(name) => void act(() => bridge.acceptSkill(run.id, name))}
          />
          <Section label="Graph">
            <Graph run={run} selection={selection} onSelect={setSelection} />
          </Section>
          {/* The detail pane sits above the board, not beside it: the board is
              already filtered to this member, so the pane is the header of what
              follows rather than a second column of the same thing. */}
          {selection?.kind === 'agent' && (
            <MemberDetail
              run={run}
              board={data?.board ?? []}
              memberId={selection.id}
              onClear={() => setSelection(null)}
            />
          )}
          <Board
            entries={data?.board ?? []}
            run={run}
            filter={filter}
            onFilter={setFilter}
            selection={selection}
            onSelect={setSelection}
          />
          <Verdicts run={run} />
          <IntegrationCard run={run} />
          {run.reflection && <ReflectionCard reflection={run.reflection} />}
          <Section label="New run">
            <button
              type="button"
              onClick={() => void act(() => bridge.remove(run.id))}
              className="vd-btn vd-btn-ghost w-full justify-center"
            >
              <X size={11} strokeWidth={2} />
              Clear this run
            </button>
          </Section>
        </>
      ) : (
        <>
          <Section label="Crew">
            <Crew
              catalog={crew.catalog}
              picked={picked}
              defaults={defaults}
              choice={choice}
              onChoice={setChoice}
              probing={crew.probing}
              exact={crew.exact}
              probeError={crew.probeError}
            />
          </Section>

          <Section label="Goal">
            <textarea
              value={goal}
              onChange={(event) => setGoal(event.target.value)}
              placeholder="What should the swarm produce? One sentence, and the lead will plan it."
              rows={3}
              className="vd-input w-full resize-none !h-auto !py-1.5 text-[11.5px] leading-[1.5]"
            />
            <MethodPreview goal={goal} />
            <div className="mt-2 flex items-center gap-2">
              <label className="flex items-center gap-1.5 text-[10.5px] text-[var(--vd-text-tertiary)]">
                workers
                <input
                  type="number"
                  min={1}
                  max={8}
                  value={concurrency}
                  onChange={(event) =>
                    setConcurrency(Math.min(8, Math.max(1, Number(event.target.value) || 1)))
                  }
                  className="vd-input vd-numeric !h-[22px] !w-12 !px-1.5 !text-[11px]"
                />
              </label>
              <button
                type="button"
                onClick={start}
                disabled={!ready || working}
                className="vd-btn vd-btn-primary ml-auto"
                title={ready ? 'Start a run' : 'A precondition is not met'}
              >
                <Play size={12} strokeWidth={2} />
                Start
              </button>
            </div>
            {!ready && (
              <p className="m-0 mt-1.5 text-[10.5px] leading-[1.45] text-[var(--vd-text-tertiary)]">
                The swarm needs every precondition above. Each one is a fact about this
                workstation, not about the plan.
              </p>
            )}
          </Section>

          <Section
            label="Rehearse"
            action={
              <button
                type="button"
                onClick={rehearseExample}
                disabled={working}
                className="vd-btn vd-btn-ghost !h-[22px] !px-1.5"
              >
                <Layers size={11} strokeWidth={2} />
                Example
              </button>
            }
          >
            {rehearsal ? (
              <RehearsalCard view={rehearsal} />
            ) : (
              <Empty>
                Rehearse the example plan to see what a swarm would schedule and cost,
                with no processes and no spend.
              </Empty>
            )}
          </Section>

          <Section label="History">
            <History
              items={data?.history ?? []}
              onOpen={(id) => void act(() => bridge.open(id))}
              onDelete={(id) => void act(() => bridge.remove(id))}
            />
          </Section>
        </>
      )}
    </Drawer>
  );
}

/* ------------------------------------------------------------------ *
 * The crew: which model each role runs on, and how hard it thinks
 * ------------------------------------------------------------------ */

/**
 * The catalogue the panel assigns from.
 *
 * Two sources, and the difference matters. The window already holds the agent's
 * `configOptions` from its own session, which is free and correct for the active
 * model — but the `thinking` option lists the levels of the model the session is
 * *on*, so using it for every model would offer a level the agent refuses for
 * the model beside it. `swarm_catalog` probes the agent properly, one switch per
 * model, and is cached in the Rust process; the panel asks for it in the
 * background and upgrades when it arrives.
 */
function useCrew() {
  const configOptions = useApp((s) => s.configOptions);
  const [probed, setProbed] = useState<Catalog | null>(null);
  const [probing, setProbing] = useState(false);
  const [probeError, setProbeError] = useState<string | null>(null);

  // The window's own view: every model, with the active model's levels. Honest
  // as a fallback because the active model's levels are the ones it really has.
  const fromWindow = useMemo<Catalog | null>(() => {
    const model = configOptions.find((o) => o.id === 'model');
    if (!model?.options?.length) return null;
    const thinking = configOptions.find((o) => o.id === 'thinking');
    const levels = (thinking?.options ?? []).map((v) => v.value);
    return {
      source: 'agent',
      models: model.options.map((v) => ({
        value: v.value,
        name: v.name,
        detail: v.description && v.description !== v.value ? v.description : null,
        thinking: levels,
      })),
    };
  }, [configOptions]);

  useEffect(() => {
    let live = true;
    setProbing(true);
    void bridge
      .catalog()
      .then((catalog) => {
        if (live && catalog.models.length) setProbed(catalog);
      })
      .catch((failure) => {
        if (live) setProbeError(errorMessage(failure));
      })
      .finally(() => {
        if (live) setProbing(false);
      });
    return () => {
      live = false;
    };
  }, []);

  const catalog = probed ?? fromWindow;
  return { catalog, probing, probeError, exact: probed !== null };
}

/** One role's row: a model, and the levels that model accepts. */
function CrewRow({
  role,
  glyph,
  model,
  asked,
  settled,
  catalog,
  defaults,
  onModel,
  onThinking,
  index,
}: {
  role: string;
  glyph: string;
  model: string;
  /** What the user clicked, or empty for the role default. */
  asked: string;
  /** What the engine will actually ask the agent for. */
  settled: string;
  catalog: Catalog;
  defaults: ThinkingByRole;
  onModel: (value: string) => void;
  onThinking: (value: string) => void;
  index: number;
}) {
  const chosen = catalog.models.find((m) => m.value === model);
  const levels = chosen?.thinking ?? [];
  const fallback = defaults[role as keyof ThinkingByRole] ?? '';
  // A level the model does not accept. Reachable by picking a level and then
  // switching the model underneath it, which is exactly when a silent repair
  // would look like the click did nothing.
  const refused = asked !== '' && levels.length > 0 && !levels.includes(asked);

  return (
    <div
      className="vd-enter flex flex-col gap-1 rounded-[var(--radius-sm)] border border-[var(--vd-line)] bg-[var(--vd-surface)] px-2 py-1.5"
      style={{ ['--vd-i' as string]: index }}
    >
      <div className="flex items-center gap-1.5">
        <span className="flex h-4 w-4 shrink-0 items-center justify-center rounded-[3px] border border-[var(--vd-line)] text-[9px] font-semibold text-[var(--vd-text-secondary)]">
          {glyph}
        </span>
        <span className="text-[10.5px] font-medium text-[var(--vd-text)]">{role}</span>
        {chosen?.detail && (
          <span className="ml-auto truncate font-mono text-[9px] text-[var(--vd-text-tertiary)]">
            {chosen.detail}
          </span>
        )}
      </div>

      <select
        value={model}
        onChange={(event) => onModel(event.target.value)}
        className="vd-input !h-[22px] w-full !py-0 text-[10.5px]"
        title="The model this role runs on"
      >
        {catalog.models.map((option) => (
          <option key={option.value} value={option.value}>
            {option.name}
          </option>
        ))}
      </select>

      <div className="flex flex-wrap items-center gap-1">
        {levels.length === 0 ? (
          <span className="text-[9.5px] text-[var(--vd-text-tertiary)]">
            levels unknown — the agent decides
          </span>
        ) : (
          levels.map((level) => {
            const active = settled === level;
            return (
              <button
                key={level}
                type="button"
                onClick={() => onThinking(active ? '' : level)}
                className={cn('vd-chip !h-[17px] !text-[9px]', active && 'vd-chip-accent')}
                title={
                  active
                    ? `${level} — click to fall back to the role default (${fallback || 'session'})`
                    : `Ask this role for ${level}`
                }
              >
                {level}
              </button>
            );
          })
        )}
        {refused && (
          <span
            className="text-[9px] text-[var(--vd-warn)]"
            title={`${model} does not accept \`${asked}\` — the engine will ask for \`${settled || 'the model default'}\` instead`}
          >
            {asked} refused
          </span>
        )}
      </div>
    </div>
  );
}

/**
 * The crew picker.
 *
 * Three roles, each with a model and a reasoning level. The levels are the ones
 * the chosen model accepts, because they are per model: `glm-5-3` takes
 * low/high/max and `mistral-medium-3.5` takes only off/high, so a single list
 * would offer a level the agent refuses.
 *
 * The assignment shown is what the engine settled on, not what was clicked. A
 * level the model refuses is repaired to the model's own default, and the panel
 * says so rather than showing a choice that will not happen.
 */
function Crew({
  catalog,
  picked,
  defaults,
  choice,
  onChoice,
  probing,
  exact,
  probeError,
}: {
  catalog: Catalog | null;
  picked: RoleModels | null;
  defaults: ThinkingByRole;
  choice: CrewChoice;
  onChoice: (next: CrewChoice) => void;
  probing: boolean;
  exact: boolean;
  probeError: string | null;
}) {
  if (!catalog || catalog.models.length === 0) {
    return (
      <Empty>
        {probing
          ? 'Reading the agent model catalogue…'
          : 'No models yet — start a session and the agent will report what it offers.'}
      </Empty>
    );
  }

  const set = (patch: Partial<CrewChoice>) => onChoice({ ...choice, ...patch });
  const setThinking = (role: keyof ThinkingByRole, value: string) =>
    onChoice({ ...choice, thinking: { ...choice.thinking, [role]: value } });

  return (
    <div className="flex flex-col gap-1.5">
      <div className="flex flex-wrap items-center gap-1.5 text-[9.5px] text-[var(--vd-text-tertiary)]">
        <span>
          {catalog.models.length} {catalog.models.length === 1 ? 'model' : 'models'}
        </span>
        <span className="opacity-40">·</span>
        <span
          title={
            exact
              ? 'Read from the agent, one switch per model'
              : 'Read from this window session — correct for the active model only'
          }
        >
          {exact ? 'levels per model' : probing ? 'reading levels…' : 'levels for the active model'}
        </span>
        {picked && !picked.diverse && (
          <>
            <span className="opacity-40">·</span>
            <span className="text-[var(--vd-warn)]">verifier shares the worker model</span>
          </>
        )}
      </div>

      {probeError && (
        <p className="m-0 text-[9.5px] leading-[1.4] text-[var(--vd-text-tertiary)]">
          The agent did not answer the catalogue probe — {probeError}
        </p>
      )}

      <CrewRow
        role="lead"
        glyph="L"
        index={0}
        model={picked?.lead ?? choice.lead}
        asked={choice.thinking.lead}
        settled={picked?.thinking.lead ?? ''}
        catalog={catalog}
        defaults={defaults}
        onModel={(value) => set({ lead: value })}
        onThinking={(value) => setThinking('lead', value)}
      />
      <CrewRow
        role="worker"
        glyph="W"
        index={1}
        model={picked?.worker ?? choice.worker}
        asked={choice.thinking.worker}
        settled={picked?.thinking.worker ?? ''}
        catalog={catalog}
        defaults={defaults}
        onModel={(value) => set({ worker: value })}
        onThinking={(value) => setThinking('worker', value)}
      />
      <CrewRow
        role="verifier"
        glyph="V"
        index={2}
        model={picked?.verifier ?? choice.verifier}
        asked={choice.thinking.verifier}
        settled={picked?.thinking.verifier ?? ''}
        catalog={catalog}
        defaults={defaults}
        onModel={(value) => set({ verifier: value })}
        onThinking={(value) => setThinking('verifier', value)}
      />

      <p className="m-0 text-[9.5px] leading-[1.45] text-[var(--vd-text-tertiary)]">
        The verifier is the one role that must differ: a verifier on the worker model shares
        its blind spots, so the error that produced the change also produces the approval.
      </p>
    </div>
  );
}

/* ------------------------------------------------------------------ *
 * The entry in the capability ledger
 * ------------------------------------------------------------------ */

/**
 * The swarm, as a row in the capabilities panel.
 *
 * It is not a capability and it is deliberately not in the ledger's array: the
 * ledger sums `bytesIn - bytesOut` clamped at zero, so a swarm row would read as
 * saving nothing, and the headline would turn "6 of 6 on" into "7 of 7". It is
 * what the six make possible, so it sits above them, dim until every one is on
 * and radiating when they are.
 *
 * It is not a toggle. There is nothing to switch on: the swarm is available
 * exactly when the capabilities it leans on are, and a switch that could be on
 * while the gate is shut would be a switch that lies.
 */
export function SwarmEntry({
  allOn,
  on,
  total,
  onOpen,
}: {
  allOn: boolean;
  on: number;
  total: number;
  onOpen: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onOpen}
      className={`group relative mt-3 flex w-full items-start gap-2.5 rounded-[var(--radius-sm)] border px-2.5 py-2 text-left transition-[border-color,background-color] duration-[var(--vd-dur-1)] ${
        allOn
          ? 'vd-aura border-[var(--vd-accent)] bg-[var(--vd-accent-wash)]'
          : 'border-dashed border-[var(--vd-line)] opacity-55 hover:opacity-80'
      }`}
      title={
        allOn
          ? 'Open the swarm — every capability it leans on is on'
          : `The swarm needs all ${total} capabilities on`
      }
    >
      <Users
        size={13}
        strokeWidth={2}
        className={`mt-0.5 shrink-0 ${
          allOn ? 'text-[var(--vd-accent)]' : 'text-[var(--vd-text-tertiary)]'
        }`}
      />
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-1.5">
          <span
            className={`text-[12px] font-semibold ${
              allOn ? 'text-[var(--vd-text)]' : 'text-[var(--vd-text-secondary)]'
            }`}
          >
            Swarm
          </span>
          {allOn ? (
            <span className="vd-chip vd-chip-accent !h-[16px] !text-[9px]">ready</span>
          ) : (
            <span className="vd-chip !h-[16px] !text-[9px]">
              {on} of {total} on
            </span>
          )}
        </div>
        <p className="m-0 mt-0.5 text-[10.5px] leading-[1.45] text-[var(--vd-text-tertiary)]">
          A lead, workers in isolated worktrees, and verifiers that try to break the
          change.
        </p>
      </div>
      <ArrowRight
        size={11}
        strokeWidth={2}
        className={`mt-1 shrink-0 ${
          allOn ? 'text-[var(--vd-accent)]' : 'text-[var(--vd-text-tertiary)]'
        }`}
      />
    </button>
  );
}
