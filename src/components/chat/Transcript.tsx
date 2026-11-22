import { memo, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import {
  AlertTriangle,
  ArrowDown,
  Brain,
  Check,
  ChevronRight,
  Circle,
  CircleCheck,
  CircleDot,
  Info,
  Sparkles,
  Wrench,
  X,
  XCircle,
} from 'lucide-react';
import type { PlanEntry, ToolCall } from '@/types/acp';
import { cn } from '@/lib/utils';
import { useDisclosure } from '@/lib/disclosure';
import { stagger } from '@/lib/motion';
import { KIND_LABEL, toolRunLabels, toolRunState } from '@/lib/tool-run';
import { formatClock, formatDuration } from '@/lib/time';
import { useApp, type QueuedPrompt, type TranscriptItem } from '@/store/app';
import { Markdown } from '@/components/chat/Markdown';
import { ToolCallCard } from '@/components/chat/ToolCallCard';
import { EmptyState } from '@/components/chat/EmptyState';

/**
 * One transcript row: either a single item, or a run of tool calls.
 *
 * Consecutive tool calls are one row because they are one act. Six cards for
 * "read, edit, read, edit, build" is six times the height for a single thought,
 * and the transcript stops reading as a conversation and starts reading as a
 * log. The calls are still there, one click away: they are the evidence, not
 * the conversation.
 */
type Row =
  | { kind: 'item'; key: string; item: TranscriptItem }
  | { kind: 'tools'; key: string; calls: ToolCall[] };

function groupRows(items: TranscriptItem[]): Row[] {
  const rows: Row[] = [];
  let run: ToolCall[] = [];

  const flush = () => {
    const first = run[0];
    if (!first) return;
    rows.push({ kind: 'tools', key: `tools:${first.toolCallId}`, calls: run });
    run = [];
  };

  for (const item of items) {
    if (item.type === 'tool') {
      run.push(item.call);
      continue;
    }
    flush();
    rows.push({ kind: 'item', key: `${item.type}:${item.id}`, item });
  }
  flush();
  return rows;
}

/**
 * Grouped rows, reusing the previous object when nothing inside it changed.
 *
 * `TranscriptRow` is memoised on its row, and the store keeps the item array's
 * identity when an update touches nothing. Rebuilding every row object on each
 * chunk would throw that away and re-render the whole transcript per token.
 */
function useRows(items: TranscriptItem[]): Row[] {
  const previous = useRef<Row[]>([]);
  return useMemo(() => {
    const before = previous.current;
    const next = groupRows(items).map((row, index) => {
      const old = before[index];
      if (!old || old.kind !== row.kind || old.key !== row.key) return row;
      if (row.kind === 'item' && old.kind === 'item') {
        return old.item === row.item ? old : row;
      }
      if (row.kind === 'tools' && old.kind === 'tools') {
        const same =
          old.calls.length === row.calls.length &&
          old.calls.every((call, at) => call === row.calls[at]);
        return same ? old : row;
      }
      return row;
    });
    previous.current = next;
    return next;
  }, [items]);
}

export function Transcript() {
  const items = useApp((s) => s.items);
  const rows = useRows(items);
  const busy = useApp((s) => s.busy);
  const trimmed = useApp((s) => s.trimmed);
  const chatLoading = useApp((s) => s.chatLoading);
  const queued = useApp((s) => s.queued);
  const sessionId = useApp((s) => s.sessionId);
  const scroller = useRef<HTMLDivElement | null>(null);
  const [pinned, setPinned] = useState(true);

  // A prompt held for another chat must not appear in this one.
  const held = queued.filter((entry) => entry.sessionId === sessionId);

  const settleFrame = useRef(0);

  useLayoutEffect(() => {
    const node = scroller.current;
    if (!node || !pinned) return;
    node.scrollTop = node.scrollHeight;
    // A row that has never rendered is measured at its estimate, so the first
    // scroll lands short of the real bottom and the rows that then render push
    // it further away. Correct on the next frame, and once more after that: two
    // passes settle a transcript whose rows render in batches, and a third
    // would be a loop.
    let passes = 0;
    const settle = () => {
      node.scrollTop = node.scrollHeight;
      if (++passes < 2) settleFrame.current = requestAnimationFrame(settle);
    };
    settleFrame.current = requestAnimationFrame(settle);
    return () => cancelAnimationFrame(settleFrame.current);
  }, [items, pinned]);

  useEffect(() => {
    const node = scroller.current;
    if (!node) return;
    const onScroll = () => {
      const distance = node.scrollHeight - node.scrollTop - node.clientHeight;
      setPinned(distance < 96);
    };
    node.addEventListener('scroll', onScroll, { passive: true });
    return () => node.removeEventListener('scroll', onScroll);
  }, []);

  /**
   * The height a row is assumed to have before it has ever rendered.
   *
   * `contain-intrinsic-size: auto` remembers a row's real height after its
   * first render, so the estimate only matters for rows that have never been
   * on screen — which is most of a long chat on the way up. A fixed 80px is
   * wrong for a markdown answer and wrong for a one-line tool card, so the
   * median of what has actually rendered is measured once per chat and written
   * to the scroller, where every row inherits it.
   *
   * Once per chat, not per chunk: measuring on every streaming update would
   * force a layout on every token.
   */
  useEffect(() => {
    const node = scroller.current;
    if (!node) return;
    const timer = setTimeout(() => {
      const heights = [...node.querySelectorAll('.vd-row')]
        .map((row) => row.getBoundingClientRect().height)
        .filter((height) => height > 0)
        .sort((a, b) => a - b);
      if (heights.length < 3) return;
      const median = heights[Math.floor(heights.length / 2)] ?? 80;
      node.style.setProperty('--vd-row-h', `${Math.round(median)}px`);
    }, 250);
    return () => clearTimeout(timer);
  }, [sessionId, chatLoading]);

  function scrollToBottom() {
    const node = scroller.current;
    if (!node) return;
    node.scrollTo({ top: node.scrollHeight, behavior: 'smooth' });
    setPinned(true);
  }

  return (
    <div className="vd-transcript relative min-h-0 flex-1">
      <div ref={scroller} className="vd-scroll-fade h-full overflow-y-auto overscroll-contain">
        <div className="mx-auto w-full max-w-[var(--vd-transcript-max)] px-6 pb-10 pt-6">
          {items.length === 0 ? (
            chatLoading ? (
              <ReplaySkeleton />
            ) : (
              <EmptyState />
            )
          ) : (
            <div className={cn('flex flex-col gap-5', chatLoading && 'vd-replay')}>
              {trimmed > 0 && <TrimmedMarker count={trimmed} />}
              {rows.map((row) => (
                <div key={row.key} className="vd-row">
                  <TranscriptRow row={row} />
                </div>
              ))}
              {held.map((entry) => (
                <QueuedMessage key={entry.id} prompt={entry} />
              ))}
              {busy && <ThinkingRow />}
            </div>
          )}
        </div>
      </div>

      {!pinned && items.length > 0 && (
        <button
          type="button"
          onClick={scrollToBottom}
          className="vd-enter absolute bottom-4 left-1/2 flex -translate-x-1/2 items-center gap-1.5 rounded-full border border-[var(--vd-line-strong)] bg-[var(--vd-overlay)] px-3 py-1.5 text-[11px] font-medium text-[var(--vd-text-secondary)] shadow-[var(--vd-shadow-pop)] hover:text-[var(--vd-text)]"
        >
          <ArrowDown size={12} strokeWidth={2.2} />
          Jump to latest
        </button>
      )}
    </div>
  );
}

/**
 * Memoised on the item object. The store replaces only the row an update
 * touched, so every other row keeps its identity and skips re-rendering — which
 * is what keeps a long transcript cheap while one message streams.
 */
const TranscriptRow = memo(function TranscriptRow({ row }: { row: Row }) {
  if (row.kind === 'tools') return <ToolRun calls={row.calls} />;
  const item = row.item;
  switch (item.type) {
    case 'user':
      return <UserMessage item={item} />;
    case 'agent':
      return <AgentMessage text={item.text} streaming={item.streaming} />;
    case 'reasoning':
      return <ReasoningBlock text={item.text} streaming={item.streaming} />;
    case 'plan':
      return <PlanCard entries={item.entries} />;
    case 'notice':
      return <NoticeLine level={item.level} text={item.text} />;
    case 'context':
      return <ContextChip text={item.text} />;
    default:
      return null;
  }
});

/**
 * A run of tool calls, folded into one dim bar.
 *
 * The bar is the point: while the agent works, the transcript says "on it" and
 * stays out of the way. It is live — a dot while a call is still running, a
 * count once they have all settled — and the calls themselves are one click
 * away, in the same disclosure the reasoning block uses.
 */
function ToolRun({ calls }: { calls: ToolCall[] }) {
  const { open, mounted, armed, toggle } = useDisclosure();
  const { live, count, failed } = toolRunState(calls);
  const { labels, hidden } = toolRunLabels(calls);

  return (
    <div className="vd-enter flex gap-3">
      <div
        className="mt-0.5 grid size-6 shrink-0 place-items-center rounded-[var(--radius-xs)] border border-[var(--vd-line-soft)] bg-transparent text-[var(--vd-text-tertiary)]"
        aria-hidden
      >
        <Wrench size={12} strokeWidth={1.9} />
      </div>
      <div className="min-w-0 flex-1">
        {/* One line, whatever the run holds. The calls are named here rather
            than only counted, because a bar that says "6 calls" and nothing
            else forces the user to open it to find out what happened — and
            opening it is exactly what turns one thought back into six cards.
            The names truncate; the count does not. */}
        <button
          type="button"
          onClick={toggle}
          aria-expanded={open}
          data-vd-tool-run={live ? 'live' : 'done'}
          title={calls.map((call) => call.title ?? KIND_LABEL[call.kind ?? 'other']).join('\n')}
          className="group flex w-full min-w-0 items-center gap-1.5 text-left text-[11.5px] text-[var(--vd-text-tertiary)] hover:text-[var(--vd-text-secondary)]"
        >
          <ChevronRight
            size={11}
            strokeWidth={2.2}
            className={cn('shrink-0 transition-transform', open && 'rotate-90')}
          />
          <span className="shrink-0 font-medium">on it</span>
          {live ? (
            <span className="size-1 shrink-0 animate-pulse rounded-full bg-[var(--vd-accent)]" />
          ) : (
            <span className="vd-numeric shrink-0 opacity-70">
              {count} call{count === 1 ? '' : 's'}
            </span>
          )}
          <span className="flex min-w-0 items-center gap-1.5 overflow-hidden">
            {labels.map((label, index) => (
              <span key={`${label.text}-${index}`} className="flex min-w-0 items-center gap-1.5">
                <span aria-hidden className="shrink-0 opacity-40">
                  ·
                </span>
                <span
                  className={cn(
                    'truncate',
                    label.failed ? 'text-[var(--vd-danger)]' : 'text-[var(--vd-text-secondary)]',
                  )}
                >
                  {label.text}
                </span>
              </span>
            ))}
            {hidden > 0 && (
              <span className="vd-numeric shrink-0 opacity-60">+{hidden}</span>
            )}
          </span>
          {/* A failure is the one thing the bar must not hide. It stays in the
              bar, in the danger colour, so a collapsed run still says that
              something went wrong. */}
          {failed > 0 && (
            <span className="vd-numeric shrink-0 text-[var(--vd-danger)]">· {failed} failed</span>
          )}
        </button>
        {mounted && (
          <div className="vd-expand mt-2" data-open={open && armed}>
            <div className="flex flex-col gap-2 border-l border-[var(--vd-line)] pl-3">
              {calls.map((call) => (
                <ToolCallCard key={call.toolCallId} call={call} />
              ))}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

/**
 * Shown when the item cap has dropped rows off the front. The transcript is
 * read from the bottom, so the newest rows are the ones kept.
 */
function TrimmedMarker({ count }: { count: number }) {
  return (
    <div className="flex items-center gap-3 text-[11px] text-[var(--vd-text-tertiary)]">
      <span className="h-px flex-1 bg-[var(--vd-line-soft)]" />
      <span className="vd-numeric shrink-0">
        {count} earlier {count === 1 ? 'entry' : 'entries'} not shown
      </span>
      <span className="h-px flex-1 bg-[var(--vd-line-soft)]" />
    </div>
  );
}

/** Placeholder while `session/load` replays a chat into an empty transcript. */
function ReplaySkeleton() {
  return (
    <div className="flex flex-col gap-5" aria-hidden>
      {[0, 1, 2].map((row) => (
        <div key={row} className="flex gap-3">
          <div className="mt-0.5 size-6 shrink-0 rounded-[var(--radius-xs)] border border-[var(--vd-line-soft)] bg-[var(--vd-surface-sunken)]" />
          <div className="flex-1 space-y-2 pt-1">
            <div className="vd-skeleton h-2.5 w-[62%] rounded-full" />
            <div className="vd-skeleton h-2.5 w-[84%] rounded-full" />
            <div className="vd-skeleton h-2.5 w-[41%] rounded-full" />
          </div>
        </div>
      ))}
    </div>
  );
}

/**
 * A clock that ticks once a second, but only while something is running.
 *
 * A settled row must not keep a timer alive: a chat with two hundred messages
 * would otherwise hold two hundred intervals to render one number that is not
 * changing.
 */
function useTick(active: boolean): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!active) return;
    setNow(Date.now());
    const handle = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(handle);
  }, [active]);
  return now;
}

/**
 * A message the user sent, with what it cost in time.
 *
 * The row carries three facts the transcript used to throw away: when it was
 * sent, how long the turn it started took, and the images that went with it.
 * The duration is stamped onto the row by the store when the turn settles, so
 * it survives the next prompt — and while the turn is still running the same
 * slot is filled by a live counter, which is why the number the user watches
 * climb is the number that stays.
 */
function UserMessage({ item }: { item: Extract<TranscriptItem, { type: 'user' }> }) {
  const busy = useApp((s) => s.busy);
  const turn = useApp((s) => s.turn);
  const [zoomed, setZoomed] = useState<string | null>(null);

  // Only the row that started the running turn is live: every earlier row has
  // had its duration stamped, so an undefined `elapsed` while busy is exactly
  // the current turn.
  const running = busy && item.elapsed === undefined && turn !== null;
  const now = useTick(running);
  const elapsed = item.elapsed ?? (running && turn ? Math.max(0, now - turn.startedAt) : null);

  const images = item.images ?? [];

  return (
    <div className="vd-enter flex justify-end">
      <div className="max-w-[85%] min-w-0 rounded-[var(--radius-lg)] rounded-br-[var(--radius-xs)] border border-[var(--vd-line)] bg-[var(--vd-surface)] px-3.5 py-2.5 shadow-[var(--vd-shadow-card)]">
        {images.length > 0 && (
          <div className={cn('mb-2 grid gap-1.5', images.length > 1 && 'grid-cols-2')}>
            {images.map((src, index) => (
              <button
                key={`${item.id}-img-${index}`}
                type="button"
                onClick={() => setZoomed(src)}
                className="overflow-hidden rounded-[var(--radius-xs)] border border-[var(--vd-line-soft)]"
                title="Open full size"
              >
                <img
                  src={src}
                  alt={`Attachment ${index + 1}`}
                  className="max-h-[220px] w-full object-cover"
                  draggable={false}
                />
              </button>
            ))}
          </div>
        )}
        {item.text && (
          <p className="m-0 whitespace-pre-wrap break-words text-[var(--text-body)] leading-[1.6] text-[var(--vd-text)]">
            {item.text}
          </p>
        )}
        <div className="mt-1.5 flex items-center justify-end gap-1.5 text-[10px] text-[var(--vd-text-tertiary)]">
          <span className="vd-numeric" title={new Date(item.at).toLocaleString()}>
            {formatClock(item.at)}
          </span>
          {elapsed !== null && (
            <>
              <span aria-hidden>·</span>
              <span
                className={cn('vd-numeric', running && 'text-[var(--vd-accent)]')}
                title={running ? 'This turn is still running' : 'How long this turn took'}
              >
                {running ? `${formatDuration(elapsed)}…` : formatDuration(elapsed)}
              </span>
            </>
          )}
        </div>
      </div>
      {zoomed && <Lightbox src={zoomed} onClose={() => setZoomed(null)} />}
    </div>
  );
}

/** Full-size view of one attachment. Escape and a click both close it. */
function Lightbox({ src, onClose }: { src: string; onClose: () => void }) {
  const close = useCallback(() => onClose(), [onClose]);
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === 'Escape') close();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [close]);

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-label="Attachment"
      onClick={close}
      className="vd-enter fixed inset-0 z-50 grid place-items-center bg-black/80 p-8"
    >
      <button
        type="button"
        onClick={close}
        aria-label="Close"
        className="absolute right-4 top-4 grid size-8 place-items-center rounded-full border border-white/20 text-white/80 hover:text-white"
      >
        <X size={14} strokeWidth={2.4} />
      </button>
      <img
        src={src}
        alt="Attachment, full size"
        onClick={(event) => event.stopPropagation()}
        className="max-h-full max-w-full rounded-[var(--radius-md)] object-contain shadow-[var(--vd-shadow-pop)]"
      />
    </div>
  );
}

/**
 * A prompt typed while the agent was working. It sits where it will land, in a
 * dashed shell, so it reads as pending rather than sent — and it can be taken
 * back before the turn ends.
 */
function QueuedMessage({ prompt }: { prompt: QueuedPrompt }) {
  const cancelQueued = useApp((s) => s.cancelQueued);

  return (
    <div className="vd-enter flex justify-end">
      <div className="max-w-[85%] rounded-[var(--radius-lg)] rounded-br-[var(--radius-xs)] border border-dashed border-[var(--vd-line-strong)] bg-[var(--vd-surface-sunken)] px-3.5 py-2.5">
        <p className="m-0 whitespace-pre-wrap break-words text-[var(--text-body)] leading-[1.6] text-[var(--vd-text-secondary)]">
          {prompt.text}
        </p>
        <div className="mt-2 flex items-center gap-2">
          <span className="vd-chip !h-[18px] !text-[10px]">Queued</span>
          <button
            type="button"
            onClick={() => cancelQueued(prompt.id)}
            className="text-[10.5px] text-[var(--vd-text-tertiary)] hover:text-[var(--vd-danger)]"
          >
            Cancel
          </button>
        </div>
      </div>
    </div>
  );
}

function AgentMessage({ text, streaming }: { text: string; streaming: boolean }) {
  return (
    <div className="vd-enter flex gap-3">
      <AgentAvatar />
      <div className="min-w-0 flex-1 pt-0.5">
        <Markdown streaming={streaming}>{text}</Markdown>
      </div>
    </div>
  );
}

function AgentAvatar() {
  return (
    <div
      className="mt-0.5 grid size-6 shrink-0 place-items-center rounded-[var(--radius-xs)] border border-[var(--vd-line)] bg-[var(--vd-surface-sunken)] text-[var(--vd-accent)]"
      aria-hidden
    >
      <Sparkles size={12} strokeWidth={2} />
    </div>
  );
}

function ReasoningBlock({ text, streaming }: { text: string; streaming: boolean }) {
  const { open, mounted, armed, toggle } = useDisclosure();
  const lines = text.trim().split('\n').length;

  return (
    <div className="vd-enter flex gap-3">
      <div
        className="mt-0.5 grid size-6 shrink-0 place-items-center rounded-[var(--radius-xs)] border border-[var(--vd-line-soft)] bg-transparent text-[var(--vd-text-tertiary)]"
        aria-hidden
      >
        <Brain size={12} strokeWidth={1.9} />
      </div>
      <div className="min-w-0 flex-1">
        <button
          type="button"
          onClick={toggle}
          aria-expanded={open}
          className="group flex items-center gap-1.5 text-[11.5px] text-[var(--vd-text-tertiary)] hover:text-[var(--vd-text-secondary)]"
        >
          <ChevronRight
            size={11}
            strokeWidth={2.2}
            className={cn('transition-transform', open && 'rotate-90')}
          />
          <span className="font-medium">
            {streaming ? 'Reasoning' : `Thought for ${lines} line${lines === 1 ? '' : 's'}`}
          </span>
          {streaming && <span className="size-1 rounded-full bg-[var(--vd-accent)]" />}
        </button>
        {mounted && (
          <div className="vd-expand mt-2" data-open={open && armed}>
            <div className="border-l border-[var(--vd-line)] pl-3">
              <div className="vd-prose !text-[12.5px] !leading-[1.62] text-[var(--vd-text-secondary)]">
                <Markdown>{text}</Markdown>
              </div>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

function PlanCard({ entries }: { entries: PlanEntry[] }) {
  const done = entries.filter((e) => e.status === 'completed').length;
  return (
    <div className="vd-enter vd-card overflow-hidden">
      <div className="flex h-9 items-center gap-2 border-b border-[var(--vd-line-soft)] px-3">
        <span className="vd-label !text-[10px] !tracking-[0.11em]">plan</span>
        <span className="vd-meta ml-auto !text-[11px] vd-numeric">
          {done}/{entries.length}
        </span>
      </div>
      <ol className="m-0 list-none p-0">
        {entries.map((entry, index) => (
          <li
            key={`${entry.content}-${index}`}
            className="flex items-start gap-2.5 border-b border-[var(--vd-line-soft)] px-3 py-2 last:border-b-0"
          >
            <PlanGlyph status={entry.status} />
            <span
              className={cn(
                'text-[12.5px] leading-[1.5]',
                entry.status === 'completed'
                  ? 'text-[var(--vd-text-tertiary)] line-through decoration-[var(--vd-line-strong)]'
                  : entry.status === 'in_progress'
                    ? 'text-[var(--vd-text)]'
                    : 'text-[var(--vd-text-secondary)]',
              )}
            >
              {entry.content}
            </span>
            {entry.priority === 'high' && entry.status !== 'completed' && (
              <span className="vd-chip vd-chip-warn ml-auto shrink-0 !h-[18px] !text-[9.5px]">
                high
              </span>
            )}
          </li>
        ))}
      </ol>
    </div>
  );
}

function PlanGlyph({ status }: { status: PlanEntry['status'] }) {
  if (status === 'completed') {
    return (
      <CircleCheck size={13} strokeWidth={2} className="mt-0.5 shrink-0 text-[var(--vd-accent)]" />
    );
  }
  if (status === 'in_progress') {
    return (
      <CircleDot size={13} strokeWidth={2} className="mt-0.5 shrink-0 text-[var(--vd-accent)]" />
    );
  }
  return (
    <Circle
      size={13}
      strokeWidth={1.8}
      className="mt-0.5 shrink-0 text-[var(--vd-text-tertiary)]"
    />
  );
}

function NoticeLine({ level, text }: { level: string; text: string }) {
  const Icon =
    level === 'error'
      ? XCircle
      : level === 'warn'
        ? AlertTriangle
        : level === 'success'
          ? Check
          : Info;
  const tone =
    level === 'error'
      ? 'text-[var(--vd-danger)]'
      : level === 'warn'
        ? 'text-[var(--vd-warn)]'
        : level === 'success'
          ? 'text-[var(--vd-accent)]'
          : 'text-[var(--vd-info)]';

  return (
    <div className="vd-enter flex items-start gap-2 pl-9 text-[12px]">
      <Icon size={13} strokeWidth={2} className={cn('mt-0.5 shrink-0', tone)} />
      <p className="m-0 whitespace-pre-wrap break-words text-[var(--vd-text-secondary)]">{text}</p>
    </div>
  );
}

function ContextChip({ text }: { text: string }) {
  return (
    <div className="vd-enter flex justify-end">
      <span className="vd-chip !h-[20px] !text-[10.5px] text-[var(--vd-text-tertiary)]">
        <Sparkles size={10} strokeWidth={2} />
        {text}
      </span>
    </div>
  );
}

function ThinkingRow() {
  return (
    <div className="flex gap-3">
      <AgentAvatar />
      <div className="flex items-center gap-1.5 pt-1.5 text-[11.5px] text-[var(--vd-text-tertiary)]">
        {/* Three dots under a gooey filter: the blur and the steep alpha ramp
            fuse them into one mass as they pulse, which reads as work in
            progress rather than as a spinner that could be anything. The
            filter is only ever on this element, which exists only while a turn
            runs, so nothing in the chrome resolves a filter at rest. */}
        <span className="vd-gooey flex items-center gap-[3px]" aria-hidden>
          {[0, 1, 2].map((i) => (
            <span
              key={i}
              className="vd-gooey-dot size-[5px] rounded-full bg-[var(--vd-accent)]"
              style={stagger(i)}
            />
          ))}
        </span>
        <span>Working…</span>
      </div>
    </div>
  );
}
