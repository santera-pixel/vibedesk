/**
 * The capability drawers.
 *
 * One drawer per capability, plus a ledger that lists all six. No two
 * capabilities share a drawer, and the registry enforces that in a test: a
 * capability with no surface would be a row the user can enable and never
 * inspect.
 *
 * Every panel here reads the same ledger record and the same stats shape, so a
 * number in one drawer means the same thing in another. The panels that can
 * change something — a threshold, a rule set, a registration — write through the
 * same commands the boot path uses, so there is no second way to configure a
 * capability.
 */
import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import {
  ArrowRight,
  Boxes,
  CircleAlert,
  CircleCheck,
  Crosshair,
  Eraser,
  FileSearch,
  Gauge,
  Layers,
  RefreshCw,
  Search,
  Sparkles,
  TerminalSquare,
  Trash2,
  Webhook,
} from 'lucide-react';
import type {
  CapabilityRecord,
  CapabilitySettings,
  ChannelStatus,
  CodeGraphGraph,
  CodeGraphState,
  CompactionStatus,
  DreamResult,
  KernelStatus,
  MemorySession,
  MemoryStatus,
  ReadingCard,
  ReadingProbe,
  ReadingStructure,
  ServerStatus,
  TerminalRules,
} from '@/lib/capabilities';
import {
  counterLabel,
  eventLabel,
  formatBytes,
  hasTokens,
  savedLabel,
  savedPercent,
  structureSummary,
  topCounters,
} from '@/lib/capabilities';
import { capabilities, session as sessionApi } from '@/lib/bridge';
import { errorMessage } from '@/lib/actions';
import { cn, formatTokens, relativeTime } from '@/lib/utils';
import { SwarmEntry } from '@/components/panels/SwarmPanel';
import { useApp } from '@/store/app';
import { Drawer, Empty, KeyValue, Section } from '@/components/chrome/Drawer';
import { CodeGraphCanvas } from '@/components/panels/CodeGraphCanvas';

/* ------------------------------------------------------------------ *
 * Shared plumbing
 * ------------------------------------------------------------------ */

/**
 * Load once, then on an interval when one is given.
 *
 * The compaction drawer polls because the proxy's sessions change while the
 * agent works and a stale token count is worse than no token count. The others
 * do not: their numbers only move when the agent crosses a boundary, and a
 * refresh button is enough.
 */
function usePolled<T>(load: () => Promise<T>, intervalMs = 0) {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(true);
  const loadRef = useRef(load);
  loadRef.current = load;

  const refresh = useCallback(async () => {
    setBusy(true);
    try {
      setData(await loadRef.current());
      setError(null);
    } catch (failure) {
      setError(errorMessage(failure));
    } finally {
      setBusy(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
    if (intervalMs <= 0) return;
    const timer = window.setInterval(() => void refresh(), intervalMs);
    return () => window.clearInterval(timer);
  }, [refresh, intervalMs]);

  return { data, error, busy, refresh };
}

/** The persisted capability settings, with a save that keeps the local copy. */
function useCapabilitySettings() {
  const [settings, setSettings] = useState<CapabilitySettings | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    capabilities
      .settings()
      .then(setSettings)
      .catch((failure) => setError(errorMessage(failure)));
  }, []);

  const save = useCallback(async (next: CapabilitySettings) => {
    try {
      setSettings(await capabilities.saveSettings(next));
      setError(null);
    } catch (failure) {
      setError(errorMessage(failure));
    }
  }, []);

  return { settings, error, save };
}

/** The ledger record for one capability, kept fresh. */
function useRecord(id: string) {
  const { data, error, busy, refresh } = usePolled(() => capabilities.snapshot());
  const record = useMemo(() => data?.find((entry) => entry.id === id) ?? null, [data, id]);
  return { record, error, busy, refresh };
}

/**
 * Point the running session at the model that goes through the proxy, or back
 * at the one it copies.
 *
 * The agent reads `config.toml` once, when it launches, and captures its model
 * list then. A session that started before compaction was switched on is still
 * talking to the provider directly, so flipping the switch would otherwise set
 * a flag and intercept nothing — the panel would read "on" while every request
 * bypassed the kernel.
 *
 * `/reload` is what makes the agent re-read the file. It is a prompt, not a
 * process restart, so the conversation survives; a session restart would drop
 * it, which is too much to do behind a switch. The agent does not announce the
 * new list afterwards, so the picker is updated here from the registration.
 */
async function routeSession(enabled: boolean): Promise<{ model: string | null; note: string }> {
  const state = useApp.getState();
  if (!state.sessionId) {
    return { model: null, note: 'The next session will start on the proxy.' };
  }
  const option = state.configOptions.find((o) => o.id === 'model');
  if (!option || typeof option.currentValue !== 'string') {
    return { model: null, note: 'Reload the session to route it through the proxy.' };
  }
  if (state.busy) {
    return {
      model: null,
      note: 'The turn in flight is still on the old model; reload the session when it ends.',
    };
  }

  const current = option.currentValue;
  const bare = current.replace(/^vibedesk\//, '');
  const wanted = enabled ? `vibedesk/${bare}` : bare;

  // Without the command the text would reach the model as a message, so the
  // switch has to say what it could not do instead of guessing.
  if (!state.commands.some((command) => command.name === 'reload')) {
    return { model: null, note: 'Reload the session to route it through the proxy.' };
  }

  try {
    await sessionApi.prompt(state.sessionId, '/reload');
    await sessionApi.setConfigOption(state.sessionId, 'model', wanted);
  } catch {
    return { model: null, note: 'Reload the session to route it through the proxy.' };
  }

  const status = await capabilities.compaction();
  const direct = (option.options ?? []).filter((value) => !value.value.startsWith('vibedesk/'));
  const options = enabled
    ? [
        ...direct,
        ...status.registration.mirrored.map((entry) => ({
          value: entry.alias,
          name: entry.displayName ?? entry.alias,
        })),
      ]
    : direct;

  state.setConfigOptions(
    state.configOptions.map((o) => (o.id === 'model' ? { ...o, currentValue: wanted, options } : o)),
  );
  return { model: wanted, note: '' };
}

function Toggle({
  record,
  onChanged,
}: {
  record: CapabilityRecord;
  onChanged: () => void;
}) {
  const notify = useApp((s) => s.notify);
  const [busy, setBusy] = useState(false);

  async function flip() {
    setBusy(true);
    try {
      const next = !record.enabled;
      await capabilities.setEnabled(record.id, next);
      onChanged();

      // Compaction is the one capability with a model behind it, so it is the
      // one that has to move the session it is meant to intercept.
      let routing = '';
      if (record.id === 'compaction') {
        const outcome = await routeSession(next);
        routing = outcome.model
          ? ` Session now on ${outcome.model}.`
          : ` ${outcome.note}`;
      }

      notify(
        `${record.name} ${record.enabled ? 'off' : 'on'}.${routing}`,
        record.enabled ? 'neutral' : 'success',
      );
    } catch (failure) {
      notify(errorMessage(failure), 'danger');
    } finally {
      setBusy(false);
    }
  }

  return (
    <button
      type="button"
      onClick={() => void flip()}
      disabled={busy}
      className="vd-switch shrink-0"
      data-on={record.enabled}
      aria-pressed={record.enabled}
      title={record.enabled ? `Turn ${record.name} off` : `Turn ${record.name} on`}
    />
  );
}

/** The header every capability drawer shares: what it is, where it attaches. */
function CapabilityHeader({
  record,
  onChanged,
}: {
  record: CapabilityRecord;
  onChanged: () => void;
}) {
  return (
    <Section
      label="Capability"
      action={<Toggle record={record} onChanged={onChanged} />}
    >
      <p className="m-0 text-[11.5px] leading-[1.5] text-[var(--vd-text-secondary)]">
        {record.summary}
      </p>
      <div className="mt-2 flex flex-wrap items-center gap-1.5">
        <span className="vd-chip !h-[17px] !text-[9px]">{record.repo}</span>
        <span className="vd-chip !h-[17px] !text-[9px]">{record.boundary}</span>
        {record.enabled ? (
          <span className="vd-chip vd-chip-accent !h-[17px] !text-[9px]">on</span>
        ) : (
          <span className="vd-chip !h-[17px] !text-[9px]">off</span>
        )}
      </div>
      {record.blocked && (
        <p className="m-0 mt-2 flex items-start gap-1.5 text-[11px] text-[var(--vd-danger)]">
          <CircleAlert size={11} strokeWidth={2} className="mt-0.5 shrink-0" />
          {record.blocked}
        </p>
      )}
    </Section>
  );
}

/** What a capability has saved, and the last few things it decided. */
function Ledger({ record, onReset }: { record: CapabilityRecord; onReset: () => void }) {
  const stats = record.stats;
  const counters = topCounters(stats);
  const percent = savedPercent(stats);
  // Tokens when the capability measures tokens, bytes when it does not. The
  // window is measured in tokens, so that is the number worth leading with —
  // but a capability that only ever counted characters must not be dressed up
  // with a token figure nobody measured.
  const saved = savedLabel(stats);

  return (
    <>
      <Section
        label="Saved · all time"
        action={
          <button
            type="button"
            onClick={onReset}
            className="vd-btn vd-btn-ghost !h-[22px] !px-1.5"
            title="Clear the counters for this capability"
          >
            <Eraser size={11} strokeWidth={2} />
            Reset
          </button>
        }
      >
        {stats.calls === 0 ? (
          <Empty>Nothing has crossed this boundary yet.</Empty>
        ) : (
          <>
            <div className="flex h-2 overflow-hidden rounded-full bg-[var(--vd-surface-sunken)]">
              <span
                className="h-full w-full origin-left bg-[var(--vd-accent)] transition-transform duration-[var(--vd-slow)] ease-[var(--vd-ease)]"
                style={{ transform: `scaleX(${percent})` }}
              />
            </div>
            <div className="mt-2 flex items-baseline justify-between">
              <span className="vd-numeric text-[12px] text-[var(--vd-text)]">
                {saved.value}
                <span className="text-[var(--vd-text-tertiary)]"> {saved.unit} saved</span>
              </span>
              <span className="vd-numeric text-[11px] text-[var(--vd-text-tertiary)]">
                {Math.round(percent * 100)}% of{' '}
                {hasTokens(stats) ? `${formatTokens(stats.tokensIn)} tokens` : formatBytes(stats.bytesIn)}
              </span>
            </div>
            <dl className="mt-3 grid grid-cols-2 gap-2">
              <Stat label="calls" value={String(stats.calls)} />
              <Stat label="last" value={relativeTime(stats.lastAt)} />
            </dl>
            {/* Both units, when both exist. The bar above is a ratio, and a
                ratio hides the absolute size of what crossed the boundary. */}
            {hasTokens(stats) && (
              <dl className="mt-2 grid grid-cols-2 gap-2">
                <Stat label="tokens in" value={formatTokens(stats.tokensIn)} />
                <Stat label="tokens out" value={formatTokens(stats.tokensOut)} />
              </dl>
            )}
            {counters.length > 0 && (
              <dl className="mt-2 grid grid-cols-2 gap-2">
                {counters.map(([key, value]) => (
                  <Stat key={key} label={counterLabel(key)} value={String(value)} />
                ))}
              </dl>
            )}
          </>
        )}
      </Section>

      {stats.recent.length > 0 && (
        <Section label="Recent · all time">
          <ul className="m-0 flex list-none flex-col gap-1 p-0">
            {stats.recent.slice(0, 8).map((event, index) => {
              // `X → X` reads as a bug. It is not: the capability ran and had
              // nothing to fold, which is the normal case for a turn that never
              // reached the gate. Saying "unchanged" is the same fact without
              // the implication that the arrow failed.
              const shown = eventLabel(event);
              return (
                <li
                  key={`${event.at}-${index}`}
                  className="flex items-baseline gap-2 border-b border-[var(--vd-line-soft)] pb-1 last:border-b-0 last:pb-0"
                >
                  <span className="min-w-0 flex-1 truncate font-mono text-[10.5px] text-[var(--vd-text-secondary)]">
                    {event.label}
                  </span>
                  {event.detail && (
                    <span className="max-w-[45%] truncate text-[10px] text-[var(--vd-text-tertiary)]">
                      {event.detail}
                    </span>
                  )}
                  {(event.tokensIn > 0 || event.bytesIn > 0) &&
                    (shown.unchanged ? (
                      <span
                        className="vd-numeric shrink-0 text-[10px] text-[var(--vd-text-tertiary)]"
                        title={`${shown.from} ${shown.unit} in, nothing folded`}
                      >
                        {shown.from} · unchanged
                      </span>
                    ) : (
                      <span
                        className="vd-numeric shrink-0 text-[10px] text-[var(--vd-text-tertiary)]"
                        title={`${shown.from} → ${shown.to} ${shown.unit}`}
                      >
                        {shown.from} → {shown.to}
                      </span>
                    ))}
                </li>
              );
            })}
          </ul>
        </Section>
      )}
    </>
  );
}

function Stat({ label, value }: { label: string; value: string }) {
  return (
    <div className="rounded-[var(--radius-sm)] border border-[var(--vd-line-soft)] px-2 py-1.5">
      <dt className="vd-label !text-[9px]">{label}</dt>
      <dd className="m-0 mt-0.5 truncate font-mono text-[11px] text-[var(--vd-text-secondary)]">
        {value}
      </dd>
    </div>
  );
}

/** A number the user can change, saved on blur rather than on every keystroke. */
function NumberField({
  label,
  value,
  step = 1,
  min = 0,
  max,
  onCommit,
}: {
  label: string;
  value: number;
  step?: number;
  min?: number;
  max?: number;
  onCommit: (next: number) => void;
}) {
  const [draft, setDraft] = useState(String(value));
  useEffect(() => setDraft(String(value)), [value]);

  function commit() {
    const parsed = Number(draft);
    if (!Number.isFinite(parsed)) {
      setDraft(String(value));
      return;
    }
    const clamped = Math.min(max ?? Number.MAX_SAFE_INTEGER, Math.max(min, parsed));
    setDraft(String(clamped));
    if (clamped !== value) onCommit(clamped);
  }

  return (
    <label className="flex items-center gap-2 py-0.5">
      <span className="w-24 shrink-0 text-[11px] text-[var(--vd-text-tertiary)]">{label}</span>
      <input
        className="vd-input vd-numeric !h-[22px] !w-20 !px-1.5 !text-[11px]"
        value={draft}
        inputMode="decimal"
        step={step}
        onChange={(event) => setDraft(event.target.value)}
        onBlur={commit}
        onKeyDown={(event) => {
          if (event.key === 'Enter') event.currentTarget.blur();
        }}
      />
    </label>
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
 * The ledger — all six, and the way into each one
 * ------------------------------------------------------------------ */

const PANEL_ICON: Record<string, typeof Gauge> = {
  compaction: Layers,
  terminal: TerminalSquare,
  reading: FileSearch,
  memory: Sparkles,
  'code-graph': Boxes,
  web: Webhook,
  channeler: Crosshair,
};

export function CapabilitiesPanel() {
  const { data, error, busy, refresh } = usePolled(() => capabilities.snapshot(), 4000);
  const setPanel = useApp((s) => s.setPanel);

  const total = data?.reduce(
    (sum, record) => sum + Math.max(0, record.stats.bytesIn - record.stats.bytesOut),
    0,
  );

  return (
    <Drawer
      title="Capabilities"
      subtitle="What VibeDesk intercepts, and what it saved"
      actions={<RefreshButton onClick={() => void refresh()} busy={busy} />}
    >
      <Section label="Total">
        <div className="flex items-baseline justify-between">
          <span className="vd-numeric text-[15px] text-[var(--vd-text)]">
            {formatBytes(total ?? 0)}
            <span className="text-[11px] text-[var(--vd-text-tertiary)]"> kept out of context</span>
          </span>
          <span className="vd-numeric text-[11px] text-[var(--vd-text-tertiary)]">
            {data?.filter((record) => record.enabled).length ?? 0} of {data?.length ?? 0} on
          </span>
        </div>
        {error && <p className="m-0 mt-2 text-[11px] text-[var(--vd-danger)]">{error}</p>}
      </Section>

      {/* The swarm sits above the six, not among them. It is not a capability and
          it is not in the array the ledger sums: its row would read as saving
          zero bytes, and the headline would turn "6 of 6 on" into "7 of 7". It is
          what the six make possible, so it is dim until every one is on and
          radiating when they are. */}
      <div className="px-3 pb-3">
        <SwarmEntry
          allOn={(data?.length ?? 0) > 0 && (data?.every((record) => record.enabled) ?? false)}
          on={data?.filter((record) => record.enabled).length ?? 0}
          total={data?.length ?? 0}
          onOpen={() => setPanel('swarm')}
        />
      </div>

      <Section label="Boundaries">
        {!data ? (
          <Empty>Reading the ledger…</Empty>
        ) : (
          <ul className="m-0 flex list-none flex-col gap-1 p-0">
            {data.map((record) => {
              const Icon = PANEL_ICON[record.id] ?? Gauge;
              const saved = Math.max(0, record.stats.bytesIn - record.stats.bytesOut);
              return (
                <li key={record.id}>
                  <button
                    type="button"
                    onClick={() => setPanel(record.panel)}
                    className="flex w-full items-start gap-2 rounded-[var(--radius-sm)] px-1.5 py-1.5 text-left hover:bg-[var(--vd-surface-hover)]"
                  >
                    <Icon
                      size={12}
                      strokeWidth={2}
                      className="mt-0.5 shrink-0 text-[var(--vd-text-tertiary)]"
                    />
                    <span className="min-w-0 flex-1">
                      <span className="flex items-baseline gap-1.5">
                        <span className="truncate text-[11.5px] text-[var(--vd-text)]">
                          {record.name}
                        </span>
                        {!record.enabled && (
                          <span className="vd-chip !h-[15px] !text-[8.5px]">off</span>
                        )}
                        {record.blocked && (
                          <CircleAlert
                            size={10}
                            strokeWidth={2.2}
                            className="shrink-0 text-[var(--vd-danger)]"
                          />
                        )}
                      </span>
                      <span className="mt-0.5 block truncate font-mono text-[10px] text-[var(--vd-text-tertiary)]">
                        {record.boundary} · {record.repo}
                      </span>
                    </span>
                    <span className="vd-numeric shrink-0 text-[10.5px] text-[var(--vd-text-tertiary)]">
                      {saved > 0 ? formatBytes(saved) : '—'}
                    </span>
                    <ArrowRight
                      size={10}
                      strokeWidth={2}
                      className="mt-1 shrink-0 text-[var(--vd-text-tertiary)]"
                    />
                  </button>
                </li>
              );
            })}
          </ul>
        )}
      </Section>
    </Drawer>
  );
}

/* ------------------------------------------------------------------ *
 * Compaction
 * ------------------------------------------------------------------ */

export function CompactionPanel() {
  const { record, error, busy, refresh } = useRecord('compaction');
  const status = usePolled<CompactionStatus>(() => capabilities.compaction(), 2000);
  const { settings, save } = useCapabilitySettings();
  const notify = useApp((s) => s.notify);
  // The model this session is actually on. `registration.activeModel` is what
  // the next session will start on, and the two disagree until the session is
  // re-pointed — which is exactly the state the panel has to show.
  const sessionModel = useApp(
    (s) => s.configOptions.find((option) => option.id === 'model')?.currentValue,
  );

  const compaction = settings?.compaction;
  const view = status.data;

  async function patch(next: Partial<NonNullable<typeof compaction>>) {
    if (!settings || !compaction) return;
    await save({ ...settings, compaction: { ...compaction, ...next } });
  }

  async function reset() {
    try {
      await capabilities.reset('compaction');
      refresh();
      notify('Compaction counters cleared.', 'success');
    } catch (failure) {
      notify(errorMessage(failure), 'danger');
    }
  }

  return (
    <Drawer
      title="Context compaction"
      subtitle="billion-context, in front of the model request"
      back={{ to: 'capabilities', label: 'Back to capabilities' }}
      actions={<RefreshButton onClick={() => void refresh()} busy={busy} />}
    >
      {record ? (
        <CapabilityHeader record={record} onChanged={refresh} />
      ) : (
        <Section label="Capability">
          <Empty>{error ?? 'Reading the ledger…'}</Empty>
        </Section>
      )}

      <Section label="Proxy">
        {!view ? (
          <Empty>{status.error ?? 'Reading the proxy…'}</Empty>
        ) : (
          <>
            <KeyValue
              label="listening"
              value={
                view.listening ? (
                  <span className="flex items-center gap-1.5">
                    <CircleCheck size={11} strokeWidth={2.2} className="text-[var(--vd-accent)]" />
                    127.0.0.1:{view.port}
                  </span>
                ) : (
                  <span className="text-[var(--vd-text-tertiary)]">
                    not started — switch compaction on
                  </span>
                )
              }
            />
            <KeyValue
              label="upstream"
              value={view.upstreamProvider || <span className="text-[var(--vd-text-tertiary)]">—</span>}
            />
            <KeyValue
              label="provider"
              value={
                view.registration.registered ? (
                  <span className="flex items-center gap-1.5">
                    <CircleCheck size={11} strokeWidth={2.2} className="text-[var(--vd-accent)]" />
                    {view.registration.provider}
                  </span>
                ) : (
                  <span className="text-[var(--vd-text-tertiary)]">not registered</span>
                )
              }
            />
            <KeyValue
              label="models"
              value={
                view.registration.models.length > 0
                  ? view.registration.models.join(', ')
                  : '—'
              }
            />
            <KeyValue label="config" value={view.registration.activeModel ?? '—'} />
            <KeyValue
              label="session"
              value={
                typeof sessionModel !== 'string' ? (
                  <span className="text-[var(--vd-text-tertiary)]">no session</span>
                ) : sessionModel.startsWith('vibedesk/') ? (
                  <span className="flex items-center gap-1.5">
                    <CircleCheck size={11} strokeWidth={2.2} className="text-[var(--vd-accent)]" />
                    routed — {sessionModel}
                  </span>
                ) : (
                  <span className="text-[var(--vd-warn)]">
                    direct — {sessionModel} (reload to route)
                  </span>
                )
              }
            />
            <p className="m-0 mt-2 text-[10.5px] leading-[1.5] text-[var(--vd-text-tertiary)]">
              VibeDesk adds its own provider and mirrored models to <code>config.toml</code> and
              never edits yours. Switching compaction off removes exactly what it added.
            </p>
          </>
        )}
      </Section>

      <Section label="Conversations · this session">
        {!view || view.sessions.length === 0 ? (
          <Empty>
            No conversation has reached the proxy yet. The kernel only holds a session once the
            agent sends a request through it, and it forgets it when the app restarts — the
            all-time figures below survive that, these do not.
          </Empty>
        ) : (
          <ul className="m-0 flex list-none flex-col gap-2 p-0">
            {view.sessions.map((session) => (
              <li key={session.session}>
                <KernelRow
                  session={session.session}
                  status={session.status}
                  nudgeAt={compaction?.nudgeThreshold ?? 0}
                />
              </li>
            ))}
          </ul>
        )}
      </Section>

      <Section label="Kernel">
        {!compaction ? (
          <Empty>Reading settings…</Empty>
        ) : (
          <>
            <NumberField
              label="context limit"
              value={compaction.contextLimit}
              step={1000}
              onCommit={(next) => void patch({ contextLimit: next })}
            />
            <NumberField
              label="nudge at"
              value={compaction.nudgeThreshold}
              step={0.05}
              max={1}
              onCommit={(next) => void patch({ nudgeThreshold: next })}
            />
            <NumberField
              label="truncate at"
              value={compaction.truncateThreshold}
              step={0.01}
              max={1}
              onCommit={(next) => void patch({ truncateThreshold: next })}
            />
            <NumberField
              label="keep recent"
              value={compaction.preserveRecent}
              onCommit={(next) => void patch({ preserveRecent: next })}
            />
            <NumberField
              label="ccr floor"
              value={compaction.ccrMinTokens}
              step={100}
              onCommit={(next) => void patch({ ccrMinTokens: next })}
            />
            <NumberField
              label="compress at"
              value={compaction.minCompressRange}
              step={500}
              onCommit={(next) => void patch({ minCompressRange: next })}
            />
            <NumberField
              label="distil at"
              value={compaction.minDistillRange}
              step={100}
              onCommit={(next) => void patch({ minDistillRange: next })}
            />
            <label className="mt-1 flex items-center gap-2 py-0.5">
              <span className="w-24 shrink-0 text-[11px] text-[var(--vd-text-tertiary)]">
                offload
              </span>
              <button
                type="button"
                className="vd-switch"
                data-on={compaction.ccr}
                aria-pressed={compaction.ccr}
                onClick={() => void patch({ ccr: !compaction.ccr })}
                title="Keep large tool results in the store and hand the model a reference"
              />
              <span className="text-[10.5px] text-[var(--vd-text-tertiary)]">
                large tool results become references
              </span>
            </label>
            {compaction.minDistillRange >= compaction.minCompressRange && (
              <p className="m-0 mt-2 flex items-start gap-1.5 text-[11px] text-[var(--vd-warn)]">
                <CircleAlert size={11} strokeWidth={2} className="mt-0.5 shrink-0" />
                <span>
                  The distillation gate is at or above the compression gate, so the nudge will
                  always ask for a compression and tiers 2 and 3 will never fire. Lower{' '}
                  <em>distil at</em> below <em>compress at</em>.
                </span>
              </p>
            )}
            <p className="m-0 mt-2 text-[10.5px] leading-[1.5] text-[var(--vd-text-tertiary)]">
              A context limit of zero means the active model's own window. The nudge gate opens
              at the first fraction; the truncation valve is the emergency path and should stay
              above it. Below the compression gate the nudge asks for a distillation instead, so
              keeping it above the distillation gate is what makes tiers 2 and 3 reachable.
            </p>
          </>
        )}
      </Section>

      {record && <Ledger record={record} onReset={() => void reset()} />}
    </Drawer>
  );
}

/**
 * One conversation as the kernel currently holds it.
 *
 * This is live, in-memory state: it exists only while the proxy has the
 * conversation, and it is empty again after a restart. That is why the panel
 * labels it "this session" and the ledger below it "all time" — the two
 * numbers disagreeing is not a bug, it is two different questions, and the
 * panel used to ask both without saying which was which.
 *
 * The gate marker is the other half of that. A conversation sitting at 12% of
 * its window has nothing to fold, and without the marker the flat tier strip
 * reads as "compaction is broken" rather than "compaction has not been asked
 * to do anything yet".
 */
function KernelRow({
  session,
  status,
  nudgeAt,
}: {
  session: string;
  status: KernelStatus;
  nudgeAt: number;
}) {
  const ratio = status.limit > 0 ? Math.min(1, status.usage) : 0;
  const gate = Math.round(status.limit * nudgeAt);
  const belowGate = gate > 0 && status.tokens < gate;

  return (
    <div className="rounded-[var(--radius-sm)] border border-[var(--vd-line-soft)] px-2 py-1.5">
      <div className="flex items-baseline justify-between">
        <span className="font-mono text-[10.5px] text-[var(--vd-text-secondary)]">{session}</span>
        <span className="vd-numeric text-[10.5px] text-[var(--vd-text-tertiary)]">
          {formatTokens(status.tokens)} / {formatTokens(status.limit)}
        </span>
      </div>
      <div className="relative mt-1 flex h-1.5 overflow-hidden rounded-full bg-[var(--vd-surface-sunken)]">
        <span
          className="h-full w-full origin-left bg-[var(--vd-accent)]"
          style={{ transform: `scaleX(${ratio})` }}
        />
        {nudgeAt > 0 && nudgeAt < 1 && (
          <span
            className="absolute inset-y-0 w-px bg-[var(--vd-text-tertiary)] opacity-70"
            style={{ left: `${nudgeAt * 100}%` }}
            title={`Nudge gate at ${formatTokens(gate)} tokens`}
          />
        )}
      </div>
      <p className="m-0 mt-1 text-[10px] text-[var(--vd-text-tertiary)]">
        {belowGate ? (
          <>
            below the nudge gate ({formatTokens(gate)}) — nothing to fold yet
          </>
        ) : (
          <>past the nudge gate ({formatTokens(gate)})</>
        )}
      </p>
      <dl className="mt-1.5 grid grid-cols-3 gap-1.5">
        <Stat label="blocks" value={`${status.activeBlocks}/${status.totalBlocks}`} />
        <Stat label="compressed" value={formatTokens(status.tokensCompressed)} />
        <Stat label="passes" value={String(status.compressionCount)} />
        <Stat label="stored" value={String(status.storedCount)} />
        <Stat label="recalls" value={String(status.retrievalCount)} />
        <Stat label="nudges" value={String(status.nudgesShown)} />
      </dl>
      <TierStrip counts={status.tierCounts} belowGate={belowGate} gate={gate} />
    </div>
  );
}

/**
 * The tier structure of the active blocks.
 *
 * A single block count cannot tell a flat conversation from a distilled one:
 * three tier-1 blocks and one tier-3 block both read as "1/3". This shows the
 * shape, which is the thing the user needs to judge whether distillation is
 * earning its keep.
 */
function TierStrip({
  counts,
  belowGate,
  gate,
}: {
  counts: Record<string, number>;
  belowGate: boolean;
  gate: number;
}) {
  const tiers = Object.entries(counts)
    .map(([tier, count]) => ({ tier: Number(tier), count }))
    .filter((entry) => entry.count > 0)
    .sort((a, b) => a.tier - b.tier);
  if (tiers.length === 0) {
    return (
      <p className="m-0 mt-1.5 text-[10px] text-[var(--vd-text-tertiary)]">
        {belowGate
          ? `Nothing folded yet — the nudge gate is at ${formatTokens(gate)} tokens.`
          : 'Nothing folded yet — the transcript is still flat.'}
      </p>
    );
  }
  return (
    <div className="mt-1.5 flex flex-wrap items-center gap-1">
      <span className="vd-label !text-[9px]">tiers</span>
      {tiers.map(({ tier, count }) => (
        <span
          key={tier}
          className="vd-chip !h-[16px] !text-[9px]"
          title={TIER_HINT[tier] ?? 'Folded range'}
        >
          T{tier} ×{count}
        </span>
      ))}
    </div>
  );
}

/** What each tier means, in the words the nudge uses. */
const TIER_HINT: Record<number, string> = {
  1: 'Compressed: a range of messages replaced by a summary.',
  2: 'Distilled: several summaries merged into one.',
  3: 'Condensed: distilled blocks merged again.',
};

/* ------------------------------------------------------------------ *
 * Terminal
 * ------------------------------------------------------------------ */

export function TerminalPanel() {
  const { record, error, busy, refresh } = useRecord('terminal');
  const rules = usePolled<TerminalRules>(() => capabilities.terminalRules());
  const { settings, save } = useCapabilitySettings();
  const notify = useApp((s) => s.notify);
  const [query, setQuery] = useState('');

  const terminal = settings?.terminal;
  const families = rules.data?.families ?? [];
  const filtered = useMemo(() => {
    const needle = query.trim().toLowerCase();
    if (!needle) return families;
    return families.filter(
      (family) =>
        family.family.toLowerCase().includes(needle) ||
        family.description.toLowerCase().includes(needle),
    );
  }, [families, query]);

  async function reset() {
    try {
      await capabilities.reset('terminal');
      refresh();
      notify('Terminal counters cleared.', 'success');
    } catch (failure) {
      notify(errorMessage(failure), 'danger');
    }
  }

  async function toggleRule(id: string) {
    if (!settings || !terminal) return;
    const disabled = terminal.disabledRules.includes(id)
      ? terminal.disabledRules.filter((entry) => entry !== id)
      : [...terminal.disabledRules, id];
    await save({ ...settings, terminal: { ...terminal, disabledRules: disabled } });
  }

  return (
    <Drawer
      title="Terminal compaction"
      subtitle="tokenjuice, on command output in the proxy"
      back={{ to: 'capabilities', label: 'Back to capabilities' }}
      actions={<RefreshButton onClick={() => void refresh()} busy={busy} />}
    >
      {record ? (
        <CapabilityHeader record={record} onChanged={refresh} />
      ) : (
        <Section label="Capability">
          <Empty>{error ?? 'Reading the ledger…'}</Empty>
        </Section>
      )}

      <Section label="Rules">
        {!rules.data ? (
          <Empty>{rules.error ?? 'Reading the rule set…'}</Empty>
        ) : (
          <>
            <div className="flex items-baseline justify-between">
              <span className="vd-numeric text-[12px] text-[var(--vd-text)]">
                {rules.data.count}
                <span className="text-[var(--vd-text-tertiary)]"> rules</span>
              </span>
              <span className="vd-numeric text-[11px] text-[var(--vd-text-tertiary)]">
                {families.length} families
              </span>
            </div>
            <KeyValue label="fallback" value={rules.data.fallback} />
            <div className="relative mt-2">
              <Search
                size={11}
                strokeWidth={2}
                className="pointer-events-none absolute left-2 top-1/2 -translate-y-1/2 text-[var(--vd-text-tertiary)]"
              />
              <input
                className="vd-input !h-[24px] !pl-6 !text-[11px]"
                placeholder="Filter families"
                value={query}
                onChange={(event) => setQuery(event.target.value)}
              />
            </div>
            <ul className="m-0 mt-2 flex max-h-64 list-none flex-col gap-0.5 overflow-y-auto p-0">
              {filtered.map((family) => {
                const off = terminal?.disabledRules.includes(family.id) ?? false;
                return (
                  <li key={family.id}>
                    <button
                      type="button"
                      onClick={() => void toggleRule(family.id)}
                      className="flex w-full items-baseline gap-2 rounded-[var(--radius-sm)] px-1 py-0.5 text-left hover:bg-[var(--vd-surface-hover)]"
                      title={off ? 'Switch this family back on' : 'Switch this family off'}
                    >
                      <span
                        className={cn(
                          'min-w-0 flex-1 truncate font-mono text-[10.5px]',
                          off
                            ? 'text-[var(--vd-text-tertiary)] line-through'
                            : 'text-[var(--vd-text-secondary)]',
                        )}
                      >
                        {family.family}
                      </span>
                      <span className="max-w-[50%] truncate text-[10px] text-[var(--vd-text-tertiary)]">
                        {family.description}
                      </span>
                    </button>
                  </li>
                );
              })}
              {filtered.length === 0 && (
                <li>
                  <Empty>No family matches “{query}”.</Empty>
                </li>
              )}
            </ul>
          </>
        )}
      </Section>

      <Section label="Limits">
        {!terminal ? (
          <Empty>Reading settings…</Empty>
        ) : (
          <>
            <NumberField
              label="inline chars"
              value={terminal.maxInlineChars}
              step={100}
              onCommit={(next) =>
                settings && void save({ ...settings, terminal: { ...terminal, maxInlineChars: next } })
              }
            />
            <p className="m-0 mt-2 text-[10.5px] leading-[1.5] text-[var(--vd-text-tertiary)]">
              Output longer than this is reduced to its head and tail. A command that is still
              running is never summarised — a partial run has no shape to summarise.
            </p>
          </>
        )}
      </Section>

      {record && <Ledger record={record} onReset={() => void reset()} />}
    </Drawer>
  );
}

/* ------------------------------------------------------------------ *
 * Reading
 * ------------------------------------------------------------------ */

export function ReadingPanel() {
  const { record, error, busy, refresh } = useRecord('reading');
  const { settings, save } = useCapabilitySettings();
  const notify = useApp((s) => s.notify);
  const [path, setPath] = useState('');
  const [probe, setProbe] = useState<ReadingProbe | null>(null);
  const [probing, setProbing] = useState(false);

  const reading = settings?.reading;

  async function run() {
    const target = path.trim();
    if (!target) return;
    setProbing(true);
    try {
      setProbe(await capabilities.readingProbe(target));
    } catch (failure) {
      setProbe(null);
      notify(errorMessage(failure), 'danger');
    } finally {
      setProbing(false);
    }
  }

  async function reset() {
    try {
      await capabilities.reset('reading');
      refresh();
      notify('Reading counters cleared.', 'success');
    } catch (failure) {
      notify(errorMessage(failure), 'danger');
    }
  }

  return (
    <Drawer
      title="Sparse reading"
      subtitle="SparseReading, on file reads in the proxy"
      back={{ to: 'capabilities', label: 'Back to capabilities' }}
      actions={<RefreshButton onClick={() => void refresh()} busy={busy} />}
    >
      {record ? (
        <CapabilityHeader record={record} onChanged={refresh} />
      ) : (
        <Section label="Capability">
          <Empty>{error ?? 'Reading the ledger…'}</Empty>
        </Section>
      )}

      <Section label="Probe">
        <p className="m-0 mb-2 text-[10.5px] leading-[1.5] text-[var(--vd-text-tertiary)]">
          Run the gate on a file without putting it in the agent's context. This is the decision
          the boundary would make, and the card the agent would receive.
        </p>
        <div className="flex items-center gap-1.5">
          <input
            className="vd-input !h-[24px] !text-[11px]"
            placeholder="/path/to/file"
            value={path}
            onChange={(event) => setPath(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === 'Enter') void run();
            }}
          />
          <button
            type="button"
            onClick={() => void run()}
            disabled={probing || !path.trim()}
            className="vd-btn vd-btn-primary !h-[24px] shrink-0"
          >
            <FileSearch size={11} strokeWidth={2.2} />
            {probing ? 'Probing…' : 'Probe'}
          </button>
        </div>

        {probe && (
          <div className="mt-2 rounded-[var(--radius-sm)] border border-[var(--vd-line-soft)] px-2 py-1.5">
            <div className="flex items-baseline justify-between gap-2">
              <span className="min-w-0 flex-1 truncate font-mono text-[10.5px] text-[var(--vd-text-secondary)]">
                {probe.path}
              </span>
              <span
                className={cn(
                  'vd-chip !h-[16px] !text-[9px]',
                  probe.decision.mode === 'force' && 'vd-chip-accent',
                  probe.decision.mode === 'advisory' && 'vd-chip-warn',
                )}
              >
                {probe.decision.mode}
              </span>
            </div>
            <KeyValue label="action" value={probe.decision.reason} />
            <KeyValue
              label="confidence"
              value={`${Math.round(probe.decision.confidence * 100)}% · ${probe.decision.code}`}
            />
            <KeyValue
              label="chars"
              value={`${probe.rawChars.toLocaleString()} → ${probe.outChars.toLocaleString()}`}
            />
            {probe.denoiseRules.length > 0 && (
              <KeyValue label="denoise" value={probe.denoiseRules.join(', ')} />
            )}
            {probe.card ? (
              <CardView card={probe.card} nextActions={probe.nextActions} />
            ) : (
              <p className="m-0 mt-1.5 text-[10.5px] text-[var(--vd-text-tertiary)]">
                No card: the gate passed this file through unchanged.
              </p>
            )}
          </div>
        )}
      </Section>

      <Section label="Gate">
        {!reading ? (
          <Empty>Reading settings…</Empty>
        ) : (
          <>
            <div className="flex items-center gap-1.5">
              {(['auto', 'force', 'native'] as const).map((mode) => (
                <button
                  key={mode}
                  type="button"
                  onClick={() =>
                    settings && void save({ ...settings, reading: { ...reading, mode } })
                  }
                  className={cn('vd-btn !h-[22px]', reading.mode === mode && 'vd-btn-primary')}
                >
                  {mode}
                </button>
              ))}
            </div>
            <NumberField
              label="document floor"
              value={reading.documentFloor}
              step={512}
              onCommit={(next) =>
                settings && void save({ ...settings, reading: { ...reading, documentFloor: next } })
              }
            />
            <NumberField
              label="collection floor"
              value={reading.collectionFloor}
              onCommit={(next) =>
                settings &&
                void save({ ...settings, reading: { ...reading, collectionFloor: next } })
              }
            />
            <p className="m-0 mt-2 text-[10.5px] leading-[1.5] text-[var(--vd-text-tertiary)]">
              A request that already carries <code>line</code> or <code>limit</code> is never
              second-guessed — that is the agent asking for exact bytes, which is what the card
              exists to make possible.
            </p>
          </>
        )}
      </Section>

      {record && <Ledger record={record} onReset={() => void reset()} />}
    </Drawer>
  );
}

function CardView({ card, nextActions }: { card: ReadingCard; nextActions: string[] | null }) {
  return (
    <div className="mt-2 border-t border-[var(--vd-line-soft)] pt-1.5">
      <div className="flex items-baseline justify-between">
        <span className="vd-label !text-[9px]">card</span>
        <span className="vd-numeric text-[10px] text-[var(--vd-text-tertiary)]">
          {card.kind} · {card.lineCount} lines · {formatBytes(card.sizeBytes)}
        </span>
      </div>
      <p className="m-0 mt-1 font-mono text-[10.5px] text-[var(--vd-text-secondary)]">
        {structureSummary(card.structure)}
      </p>
      <StructureView structure={card.structure} />
      {card.samples.length > 0 && (
        <ul className="m-0 mt-1.5 flex list-none flex-col gap-0.5 p-0">
          {card.samples.map((sample) => (
            <li key={sample.anchor} className="flex items-baseline gap-1.5">
              <span className="shrink-0 font-mono text-[9.5px] text-[var(--vd-text-tertiary)]">
                {sample.anchor}
              </span>
              <span className="min-w-0 flex-1 truncate font-mono text-[10px] text-[var(--vd-text-secondary)]">
                {sample.text}
              </span>
            </li>
          ))}
        </ul>
      )}
      {card.signals.length > 0 && (
        <div className="mt-1.5 flex flex-wrap gap-1">
          {card.signals.map((signal) => (
            <span
              key={`${signal.kind}-${signal.detail}`}
              className="vd-chip !h-[16px] !text-[9px]"
              title={signal.detail}
            >
              {signal.kind}
            </span>
          ))}
        </div>
      )}
      {nextActions && nextActions.length > 0 && (
        <div className="mt-1.5">
          <span className="vd-label !text-[9px]">how the agent gets the rest</span>
          <ul className="m-0 mt-0.5 flex list-none flex-col gap-0.5 p-0">
            {nextActions.map((action) => (
              <li
                key={action}
                className="truncate font-mono text-[10px] text-[var(--vd-text-tertiary)]"
              >
                {action}
              </li>
            ))}
          </ul>
        </div>
      )}
    </div>
  );
}

function StructureView({ structure }: { structure: ReadingStructure }) {
  switch (structure.shape) {
    case 'text':
      return structure.headings.length > 0 ? (
        <ul className="m-0 mt-1 flex list-none flex-col gap-0.5 p-0">
          {structure.headings.slice(0, 6).map((heading) => (
            <li key={heading} className="truncate text-[10px] text-[var(--vd-text-tertiary)]">
              {heading}
            </li>
          ))}
        </ul>
      ) : null;
    case 'log':
      return (
        <div className="mt-1 flex flex-wrap gap-1">
          {Object.entries(structure.levels).map(([level, count]) => (
            <span key={level} className="vd-chip !h-[16px] !text-[9px]">
              {level} {count}
            </span>
          ))}
        </div>
      );
    case 'tabular':
      return (
        <p className="m-0 mt-1 truncate font-mono text-[10px] text-[var(--vd-text-tertiary)]">
          {structure.columns.join(' · ')}
        </p>
      );
    case 'mapping':
      return (
        <p className="m-0 mt-1 truncate font-mono text-[10px] text-[var(--vd-text-tertiary)]">
          {structure.keys.slice(0, 12).join(' · ')}
        </p>
      );
    case 'markup':
      return (
        <p className="m-0 mt-1 truncate font-mono text-[10px] text-[var(--vd-text-tertiary)]">
          {Object.entries(structure.children)
            .map(([child, count]) => `${child} ${count}`)
            .join(' · ')}
        </p>
      );
    case 'collection':
      return (
        <p className="m-0 mt-1 truncate font-mono text-[10px] text-[var(--vd-text-tertiary)]">
          {Object.entries(structure.kindCounts)
            .map(([kind, count]) => `${kind} ${count}`)
            .join(' · ')}
        </p>
      );
    case 'unsupported':
      return (
        <p className="m-0 mt-1 text-[10px] text-[var(--vd-warn)]">
          This port cannot preview this shape: {structure.reason}
        </p>
      );
  }
}

/* ------------------------------------------------------------------ *
 * Memory
 * ------------------------------------------------------------------ */

export function MemoryPanel() {
  const { record, error, refresh } = useRecord('memory');
  const status = usePolled<MemoryStatus>(() => capabilities.memory());
  const { settings, save } = useCapabilitySettings();
  const notify = useApp((s) => s.notify);
  const items = useApp((s) => s.items);
  const sessionId = useApp((s) => s.sessionId);
  const [dreaming, setDreaming] = useState(false);
  const [result, setResult] = useState<DreamResult | null>(null);

  const memory = settings?.memory;

  /** The live conversation, as the consolidator sees it. */
  const session = useMemo<MemorySession>(() => {
    const messages = items
      .filter((item) => item.type === 'user' || item.type === 'agent')
      .map((item) => ({
        role: item.type === 'user' ? 'user' : 'assistant',
        content: item.text,
      }));
    return { id: sessionId ?? 'live', messages };
  }, [items, sessionId]);

  async function consolidate() {
    setDreaming(true);
    try {
      const outcome = await capabilities.dream([session]);
      setResult(outcome);
      status.refresh();
      refresh();
      notify(
        outcome.success ? outcome.summary || 'Consolidation finished.' : outcome.error ?? 'Failed.',
        outcome.success ? 'success' : 'danger',
      );
    } catch (failure) {
      notify(errorMessage(failure), 'danger');
    } finally {
      setDreaming(false);
    }
  }

  async function forget(id: string) {
    try {
      await capabilities.forget(id);
      status.refresh();
      notify('Memory removed.', 'success');
    } catch (failure) {
      notify(errorMessage(failure), 'danger');
    }
  }

  async function reset() {
    try {
      await capabilities.reset('memory');
      refresh();
      notify('Memory counters cleared.', 'success');
    } catch (failure) {
      notify(errorMessage(failure), 'danger');
    }
  }

  const view = status.data;

  return (
    <Drawer
      title="Reflective memory"
      subtitle="dream-memory, over finished sessions"
      back={{ to: 'capabilities', label: 'Back to capabilities' }}
      actions={<RefreshButton onClick={() => void status.refresh()} busy={status.busy} />}
    >
      {record ? (
        <CapabilityHeader record={record} onChanged={refresh} />
      ) : (
        <Section label="Capability">
          <Empty>{error ?? 'Reading the ledger…'}</Empty>
        </Section>
      )}

      <Section label="Store">
        {!view ? (
          <Empty>{status.error ?? 'Reading the store…'}</Empty>
        ) : (
          <>
            <KeyValue label="root" value={view.root} />
            <KeyValue label="memories" value={String(view.count)} />
            {Object.keys(view.byType).length > 0 && (
              <div className="mt-1 flex flex-wrap gap-1">
                {Object.entries(view.byType).map(([kind, count]) => (
                  <span key={kind} className="vd-chip !h-[16px] !text-[9px]">
                    {kind} {count}
                  </span>
                ))}
              </div>
            )}
          </>
        )}
      </Section>

      <Section label="Gate">
        {!view ? (
          <Empty>Reading the gate…</Empty>
        ) : (
          <>
            <KeyValue
              label="ready"
              value={
                view.gate.ready ? (
                  <span className="flex items-center gap-1.5">
                    <CircleCheck size={11} strokeWidth={2.2} className="text-[var(--vd-accent)]" />
                    yes
                  </span>
                ) : (
                  <span className="text-[var(--vd-text-tertiary)]">{view.gate.reason}</span>
                )
              }
            />
            <KeyValue label="sessions" value={String(view.gate.sessionsSince)} />
            <KeyValue
              label="since"
              value={
                Number.isFinite(view.gate.hoursSince)
                  ? `${view.gate.hoursSince.toFixed(1)} h`
                  : 'never'
              }
            />
            <button
              type="button"
              onClick={() => void consolidate()}
              disabled={dreaming || session.messages.length === 0}
              className="vd-btn vd-btn-primary !mt-2 !h-[24px]"
              title={
                session.messages.length === 0
                  ? 'This conversation has nothing to consolidate yet'
                  : 'Consolidate this conversation into memory'
              }
            >
              <Sparkles size={11} strokeWidth={2.2} />
              {dreaming ? 'Consolidating…' : 'Consolidate this conversation'}
            </button>
            <p className="m-0 mt-1.5 text-[10.5px] leading-[1.5] text-[var(--vd-text-tertiary)]">
              The gate is advice, not a lock: a manual pass runs whenever you ask. The pass calls
              the provider directly, never through the compaction proxy — a consolidation is not
              a conversation and must not be compacted.
            </p>
            {result && (
              <div className="mt-2 rounded-[var(--radius-sm)] border border-[var(--vd-line-soft)] px-2 py-1.5">
                <div className="flex items-baseline justify-between">
                  <span className="vd-label !text-[9px]">{result.phase}</span>
                  <span className="vd-numeric text-[10px] text-[var(--vd-text-tertiary)]">
                    {result.durationMs} ms
                  </span>
                </div>
                <dl className="mt-1 grid grid-cols-4 gap-1.5">
                  <Stat label="scanned" value={String(result.memoriesScanned)} />
                  <Stat label="new" value={String(result.memoriesCreated)} />
                  <Stat label="updated" value={String(result.memoriesUpdated)} />
                  <Stat label="pruned" value={String(result.memoriesPruned)} />
                </dl>
                <p
                  className={cn(
                    'm-0 mt-1.5 text-[10.5px]',
                    result.success ? 'text-[var(--vd-text-secondary)]' : 'text-[var(--vd-danger)]',
                  )}
                >
                  {result.error ?? result.summary}
                </p>
              </div>
            )}
          </>
        )}
      </Section>

      <Section label="Memories">
        {!view || view.memories.length === 0 ? (
          <Empty>Nothing remembered yet. A consolidation pass fills this.</Empty>
        ) : (
          <ul className="m-0 flex list-none flex-col gap-1 p-0">
            {view.memories.map((memory) => (
              <li
                key={memory.id}
                className="group flex items-start gap-2 border-b border-[var(--vd-line-soft)] pb-1 last:border-b-0 last:pb-0"
              >
                <span className="min-w-0 flex-1">
                  <span className="flex items-baseline gap-1.5">
                    <span className="truncate text-[11.5px] text-[var(--vd-text)]">
                      {memory.name}
                    </span>
                    <span className="vd-chip !h-[15px] !text-[8.5px]">{memory.type}</span>
                  </span>
                  <span className="mt-0.5 block line-clamp-2 text-[10.5px] leading-[1.45] text-[var(--vd-text-tertiary)]">
                    {memory.description}
                  </span>
                  <span className="mt-0.5 block font-mono text-[9.5px] text-[var(--vd-text-tertiary)]">
                    {memory.id} · {relativeTime(new Date(memory.updatedAt).toISOString())}
                  </span>
                </span>
                <button
                  type="button"
                  onClick={() => void forget(memory.id)}
                  className="vd-btn vd-btn-ghost !h-[20px] !px-1 opacity-0 group-hover:opacity-100"
                  title="Forget this memory"
                >
                  <Trash2 size={10} strokeWidth={2} />
                </button>
              </li>
            ))}
          </ul>
        )}
      </Section>

      <Section label="Policy">
        {!memory ? (
          <Empty>Reading settings…</Empty>
        ) : (
          <>
            <NumberField
              label="max memories"
              value={memory.maxMemories}
              onCommit={(next) =>
                settings && void save({ ...settings, memory: { ...memory, maxMemories: next } })
              }
            />
            <NumberField
              label="min sessions"
              value={memory.minSessions}
              onCommit={(next) =>
                settings && void save({ ...settings, memory: { ...memory, minSessions: next } })
              }
            />
            <label className="mt-1 flex items-center gap-2 py-0.5">
              <span className="w-24 shrink-0 text-[11px] text-[var(--vd-text-tertiary)]">
                recall
              </span>
              <button
                type="button"
                className="vd-switch"
                data-on={memory.injectRecall}
                aria-pressed={memory.injectRecall}
                onClick={() =>
                  void save({
                    ...settings!,
                    memory: { ...memory, injectRecall: !memory.injectRecall },
                  })
                }
                title="Inject relevant memories into the proxy's system prompt"
              />
              <span className="text-[10.5px] text-[var(--vd-text-tertiary)]">
                relevant memories reach the model
              </span>
            </label>
          </>
        )}
      </Section>

      {record && <Ledger record={record} onReset={() => void reset()} />}
    </Drawer>
  );
}

/* ------------------------------------------------------------------ *
 * External MCP servers
 * ------------------------------------------------------------------ */

/**
 * One drawer for both native servers.
 *
 * They are the same shape of thing — a binary VibeDesk finds, registers in the
 * agent's `config.toml`, and reports on — so they share a component. What
 * differs is the data, and that is a prop, not a second implementation.
 */
function ExternalPanel({
  name,
  title,
  subtitle,
  children,
}: {
  name: string;
  title: string;
  subtitle: string;
  /** Rendered between the capability header and the server section. */
  children?: (record: CapabilityRecord | null) => ReactNode;
}) {
  const { record, error, busy, refresh } = useRecord(name);
  const servers = usePolled<ServerStatus[]>(() => capabilities.external());
  const notify = useApp((s) => s.notify);
  const [working, setWorking] = useState(false);

  const server = servers.data?.find((entry) => entry.capability === name) ?? null;

  async function act(register: boolean) {
    if (!server) return;
    setWorking(true);
    try {
      if (register) await capabilities.register(server.name);
      else await capabilities.unregister(server.name);
      servers.refresh();
      refresh();
      notify(
        register
          ? `${server.name} registered with the agent.`
          : `${server.name} removed from the agent's config.`,
        'success',
      );
    } catch (failure) {
      notify(errorMessage(failure), 'danger');
    } finally {
      setWorking(false);
    }
  }

  async function reset() {
    try {
      await capabilities.reset(name);
      refresh();
      notify('Counters cleared.', 'success');
    } catch (failure) {
      notify(errorMessage(failure), 'danger');
    }
  }

  return (
    <Drawer
      title={title}
      subtitle={subtitle}
      back={{ to: 'capabilities', label: 'Back to capabilities' }}
      actions={<RefreshButton onClick={() => void servers.refresh()} busy={servers.busy || busy} />}
    >
      {record ? (
        <CapabilityHeader record={record} onChanged={refresh} />
      ) : (
        <Section label="Capability">
          <Empty>{error ?? 'Reading the ledger…'}</Empty>
        </Section>
      )}

      {children?.(record)}

      <Section label="Server">
        {!server ? (
          <Empty>{servers.error ?? 'Reading the server list…'}</Empty>
        ) : (
          <>
            <KeyValue label="binary" value={server.binary} />
            <KeyValue
              label="found"
              value={
                server.path ? (
                  server.path
                ) : (
                  <span className="text-[var(--vd-warn)]">not on PATH or in the tools folder</span>
                )
              }
            />
            <KeyValue label="version" value={server.version ?? '—'} />
            <KeyValue
              label="registered"
              value={
                server.registered ? (
                  <span className="flex items-center gap-1.5">
                    <CircleCheck size={11} strokeWidth={2.2} className="text-[var(--vd-accent)]" />
                    in config.toml
                  </span>
                ) : (
                  <span className="text-[var(--vd-text-tertiary)]">no</span>
                )
              }
            />
            <KeyValue
              label="declared to agent"
              value={
                server.declared ? (
                  <span className="flex items-center gap-1.5">
                    <CircleCheck size={11} strokeWidth={2.2} className="text-[var(--vd-accent)]" />
                    on session start
                  </span>
                ) : (
                  <span className="text-[var(--vd-text-tertiary)]">no</span>
                )
              }
            />
            <p className="m-0 mt-2 text-[11px] leading-[1.5] text-[var(--vd-text-secondary)]">
              {server.gives}
            </p>
            {server.blocked && (
              <p className="m-0 mt-1.5 flex items-start gap-1.5 text-[11px] text-[var(--vd-warn)]">
                <CircleAlert size={11} strokeWidth={2} className="mt-0.5 shrink-0" />
                {server.blocked}
              </p>
            )}
            <div className="mt-2 flex items-center gap-1.5">
              <button
                type="button"
                onClick={() => void act(true)}
                disabled={working || !server.path}
                className="vd-btn vd-btn-primary !h-[24px]"
                title={
                  server.path
                    ? 'Add this server to the agent config'
                    : 'Install the binary first'
                }
              >
                <Webhook size={11} strokeWidth={2.2} />
                {server.registered ? 'Re-register' : 'Register'}
              </button>
              {server.registered && (
                <button
                  type="button"
                  onClick={() => void act(false)}
                  disabled={working}
                  className="vd-btn !h-[24px]"
                >
                  Remove
                </button>
              )}
            </div>
            <p className="m-0 mt-2 text-[10.5px] leading-[1.5] text-[var(--vd-text-tertiary)]">
              VibeDesk writes a <code>stdio</code> entry into the agent's{' '}
              <code>config.toml</code>, keeps a <code>.bak</code> of the previous file, and
              declares the server to the agent on every session start. It does not vendor the
              server: the binary is the upstream project, and this panel is how it gets found and
              wired up.
            </p>
          </>
        )}
      </Section>

      {record && <Ledger record={record} onReset={() => void reset()} />}
    </Drawer>
  );
}

/**
 * The graph itself, drawn.
 *
 * Fetched apart from the counts because it is a dozen paged round trips: the
 * counts answer "is it indexed", the graph answers "what is in it", and a panel
 * that makes the second wait for the first is a panel that feels broken.
 */
function CodeGraphView({ project }: { project: string }) {
  const graph = usePolled<CodeGraphGraph>(() => capabilities.codegraphGraph(project));

  return (
    <Section
      label="Graph"
      action={
        <button
          type="button"
          onClick={() => void graph.refresh()}
          className="text-[10px] text-[var(--vd-text-tertiary)] hover:text-[var(--vd-text-secondary)]"
        >
          {graph.busy ? 'reading…' : 'reload'}
        </button>
      }
    >
      {graph.data ? (
        <>
          {graph.data.truncated && (
            <p className="m-0 mb-2 flex items-start gap-1.5 text-[11px] text-[var(--vd-warn)]">
              <CircleAlert size={11} strokeWidth={2} className="mt-0.5 shrink-0" />
              Partial graph: {graph.data.truncation}.
            </p>
          )}
          <CodeGraphCanvas graph={graph.data} />
        </>
      ) : (
        <Empty>{graph.error ?? 'Reading the graph…'}</Empty>
      )}
    </Section>
  );
}

/**
 * What the graph holds, asked of the server rather than inferred from a flag.
 *
 * "Registered" answers whether the wiring exists. This answers what is in the
 * index — which repositories, how many nodes and edges, what the parser could
 * not read — which is the question a user has after watching the agent index a
 * folder and seeing nothing change here.
 */
function CodeGraphIndex() {
  const graph = usePolled<CodeGraphState>(() => capabilities.codegraph(true));

  if (!graph.data) {
    return (
      <Section label="Index">
        <Empty>{graph.error ?? 'Asking the server what it holds…'}</Empty>
      </Section>
    );
  }

  const state = graph.data;
  if (!state.available) {
    return (
      <Section label="Index">
        <Empty>{state.reason ?? 'The code graph server is not reachable.'}</Empty>
      </Section>
    );
  }

  const index = state.current;

  return (
    <>
      <Section label="Index">
        {!state.workspaceIndexed && state.contained.length === 0 && (
          <p className="m-0 mb-2 flex items-start gap-1.5 text-[11px] text-[var(--vd-warn)]">
            <CircleAlert size={11} strokeWidth={2} className="mt-0.5 shrink-0" />
            This folder is not indexed yet. Ask the agent to index it and the graph appears here.
          </p>
        )}
        {!state.workspaceIndexed && state.contained.length === 1 && (
          <p className="m-0 mb-2 text-[11px] leading-[1.5] text-[var(--vd-text-tertiary)]">
            This folder is not itself a project. Showing{' '}
            <span className="font-mono">{state.contained[0]?.root}</span>, the indexed project
            inside it.
          </p>
        )}
        {!state.workspaceIndexed && state.contained.length > 1 && (
          <p className="m-0 mb-2 text-[11px] leading-[1.5] text-[var(--vd-text-tertiary)]">
            {state.contained.length} indexed projects sit inside this folder. Open one of them to
            see its graph.
          </p>
        )}
        {index ? (
          <>
            <div className="mb-2 grid grid-cols-2 gap-1.5">
              <Stat label="nodes" value={index.nodes.toLocaleString()} />
              <Stat label="edges" value={index.edges.toLocaleString()} />
            </div>
            <KeyValue label="project" value={index.project} />
            <KeyValue label="status" value={index.status} />
            <KeyValue
              label="indexed"
              value={index.indexedAt ? relativeTime(index.indexedAt) : '—'}
            />
            <KeyValue
              label="parse failures"
              value={`${index.parsePartial} partial, ${index.parseUnusable} unusable`}
            />
            <KeyValue label="skipped" value={String(index.skipped)} />
            <KeyValue
              label="left out"
              value={`${index.notIndexedDirs} dirs, ${index.notIndexedFiles} files`}
            />
            <KeyValue label="watching" value={index.watching ? 'yes' : 'no'} />
          </>
        ) : (
          <Empty>No index for this folder yet.</Empty>
        )}
      </Section>

      {index && <CodeGraphView project={index.project} />}

      <Section label={`Projects · ${state.projects.length}`}>
        {state.projects.length === 0 ? (
          <Empty>Nothing indexed yet.</Empty>
        ) : (
          <ul className="m-0 flex list-none flex-col gap-1 p-0">
            {state.projects.map((project) => (
              <li
                key={project.name}
                className="rounded-[var(--radius-sm)] border border-[var(--vd-line-soft)] px-2 py-1.5"
              >
                <div className="break-all font-mono text-[10.5px] text-[var(--vd-text-secondary)]">
                  {project.root}
                </div>
                <div className="mt-0.5 flex items-baseline gap-2 text-[10px] text-[var(--vd-text-tertiary)]">
                  <span className="min-w-0 flex-1 truncate">{project.name}</span>
                  {project.nodes !== undefined && (
                    <span className="vd-numeric shrink-0">
                      {project.nodes.toLocaleString()} nodes ·{' '}
                      {(project.edges ?? 0).toLocaleString()} edges
                    </span>
                  )}
                </div>
              </li>
            ))}
          </ul>
        )}
      </Section>
    </>
  );
}

/** The web tools whose calls are worth listing by name. */
const WEB_TOOLS = new Set(['web_search', 'web_fetch', 'web_crawl', 'web_screenshot']);

/**
 * What the web server has actually been asked to do.
 *
 * donsetch keeps no history — its tools are stateless — so the only record of a
 * search is the agent's own tool call. Those are recorded as they happen, which
 * is why this list is real rather than a placeholder.
 */
function WebActivity({ record }: { record: CapabilityRecord | null }) {
  const calls = (record?.stats.recent ?? []).filter((event) => WEB_TOOLS.has(event.label));

  return (
    <Section label={`Searches · ${calls.length}`}>
      {calls.length === 0 ? (
        <Empty>
          No searches yet. The agent's web_search and web_fetch calls appear here as it makes
          them.
        </Empty>
      ) : (
        <ul className="m-0 flex list-none flex-col gap-1.5 p-0">
          {calls.map((event, index) => (
            <li
              key={`${event.at}-${index}`}
              className="rounded-[var(--radius-sm)] border border-[var(--vd-line-soft)] px-2 py-1.5"
            >
              <div className="flex items-baseline gap-2">
                <span className="font-mono text-[10.5px] text-[var(--vd-text-secondary)]">
                  {event.label}
                </span>
                <span className="ml-auto shrink-0 text-[10px] text-[var(--vd-text-tertiary)]">
                  {relativeTime(event.at)}
                </span>
              </div>
              {event.detail && (
                <div className="mt-0.5 break-all font-mono text-[10px] text-[var(--vd-text-tertiary)]">
                  {event.detail}
                </div>
              )}
            </li>
          ))}
        </ul>
      )}
    </Section>
  );
}

export function CodeGraphPanel() {
  return (
    <ExternalPanel
      name="code-graph"
      title="Code graph"
      subtitle="codebase-memory-mcp, over MCP"
    >
      {() => <CodeGraphIndex />}
    </ExternalPanel>
  );
}

export function WebPanel() {
  return (
    <ExternalPanel name="web" title="Web search and read" subtitle="donsetch, over MCP">
      {(record) => <WebActivity record={record} />}
    </ExternalPanel>
  );
}

/**
 * The channeler.
 *
 * Unlike the other external capabilities this one has no binary to find and
 * nothing to register: it is the app's own loopback server, started with the app
 * and declared to every session. So the panel answers the two questions that are
 * actually open — is it listening, and what is it driving.
 */
export function ChannelerPanel() {
  const { record, error, busy, refresh } = useRecord('channeler');
  const status = usePolled<ChannelStatus>(() => capabilities.channelStatus(), 3000);

  return (
    <Drawer
      title="Test channel"
      subtitle="a supervised input channel, over MCP"
      back={{ to: 'capabilities', label: 'Back to capabilities' }}
      actions={
        <RefreshButton
          onClick={() => {
            void status.refresh();
            refresh();
          }}
          busy={status.busy || busy}
        />
      }
    >
      {record ? (
        <CapabilityHeader record={record} onChanged={refresh} />
      ) : (
        <Section label="Capability">
          <Empty>{error ?? 'Reading the ledger…'}</Empty>
        </Section>
      )}

      <Section label="Server">
        {!status.data ? (
          <Empty>{status.error ?? 'Reading the server…'}</Empty>
        ) : (
          <>
            <KeyValue
              label="listening"
              value={
                status.data.listening ? (
                  <span className="flex items-center gap-1.5">
                    <CircleCheck size={11} strokeWidth={2.2} className="text-[var(--vd-accent)]" />
                    {status.data.url}
                  </span>
                ) : (
                  <span className="text-[var(--vd-warn)]">not listening</span>
                )
              }
            />
            <KeyValue label="declared to agent" value="on session start" />
            <p className="m-0 mt-2 text-[11px] leading-[1.5] text-[var(--vd-text-secondary)]">
              The agent opens a channel onto something it built, drives it with a sequence of
              actions, and gets every result plus everything that broke in one reply. Input lands
              at the renderer, so the cursor does not move and the window need not be in front.
            </p>
          </>
        )}
      </Section>

      <Section label="Open channels">
        {!status.data ? (
          <Empty>Reading…</Empty>
        ) : status.data.channels.length === 0 ? (
          <Empty>No channel is open. The agent opens one when it has something to test.</Empty>
        ) : (
          <ul className="m-0 flex list-none flex-col gap-1.5 p-0">
            {status.data.channels.map((channel) => (
              <li
                key={channel.id}
                className="flex items-center justify-between gap-2 rounded-[var(--vd-radius-sm)] border border-[var(--vd-border)] px-2 py-1.5"
              >
                <span className="flex items-center gap-1.5 text-[11px] text-[var(--vd-text)]">
                  <Crosshair size={11} strokeWidth={2.2} className="text-[var(--vd-accent)]" />
                  {channel.id}
                </span>
                <span className="text-[11px] text-[var(--vd-text-secondary)]">
                  {channel.backend} · {channel.host} · {channel.actions.length} actions
                </span>
              </li>
            ))}
          </ul>
        )}
      </Section>
    </Drawer>
  );
}
