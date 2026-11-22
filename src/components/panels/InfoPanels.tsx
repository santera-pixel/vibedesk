import { useEffect, useMemo, useState } from 'react';
import {
  ArrowDownToLine,
  BookOpen,
  Check,
  CircleAlert,
  Download,
  ExternalLink,
  Gauge,
  Keyboard,
  RefreshCw,
  Sparkles,
  UserRound,
} from 'lucide-react';
import type { JsonValue, ReleaseNote, SkillRecord } from '@/types/acp';
import { cn, formatCost, formatTokens, relativeTime } from '@/lib/utils';
import {
  build as buildApi,
  customize as customizeApi,
  dataPlane,
  debug as debugApi,
  updates,
  vibe,
  type BuildInfo,
} from '@/lib/bridge';
import { applyLayout, applyTheme } from '@/lib/customize';
import { checkForUpdates, errorMessage, toggleSwarmSkills } from '@/lib/actions';
import { useApp } from '@/store/app';
import { usePanelData } from '@/store/panelData';
import { Drawer, Empty, KeyValue, Section } from '@/components/chrome/Drawer';
import { Markdown } from '@/components/chat/Markdown';

/* ------------------------------------------------------------------ *
 * Context
 * ------------------------------------------------------------------ */

export function ContextPanel() {
  const usage = useApp((s) => s.usage);
  const environment = useApp((s) => s.environment);
  const ratio = usage && usage.size > 0 ? Math.min(1, usage.used / usage.size) : 0;
  const free = usage ? Math.max(0, usage.size - usage.used) : 0;

  const meta = usage?.meta ?? null;
  const prompt = meta?.promptTokens ?? 0;
  const cached = meta?.cachedTokens ?? 0;
  const hitRate = prompt > 0 ? Math.min(1, cached / prompt) : 0;
  const fresh = Math.max(0, prompt - cached);

  return (
    <Drawer title="Context" subtitle="What fills the model window">
      <Section label="Window">
        {usage ? (
          <>
            <div className="flex h-2 overflow-hidden rounded-full bg-[var(--vd-surface-sunken)]">
              <span
                className="h-full w-full origin-left bg-[var(--vd-accent)] transition-transform duration-[var(--vd-slow)] ease-[var(--vd-ease)]"
                style={{ transform: `scaleX(${ratio})` }}
              />
            </div>
            <div className="mt-2 flex items-baseline justify-between">
              <span className="vd-numeric text-[12px] text-[var(--vd-text)]">
                {formatTokens(usage.used)}
                <span className="text-[var(--vd-text-tertiary)]">
                  {' '}
                  / {formatTokens(usage.size)}
                </span>
              </span>
              <span className="vd-numeric text-[11px] text-[var(--vd-text-tertiary)]">
                {Math.round(ratio * 100)}% used
              </span>
            </div>
            <dl className="mt-3 grid grid-cols-2 gap-2">
              <Stat label="free" value={formatTokens(free)} />
              <Stat label="in context" value={formatTokens(usage.used)} />
              {usage.cost && (
                <Stat label="cost" value={formatCost(usage.cost.amount, usage.cost.currency)} />
              )}
              <Stat label="model" value={environment?.activeModel ?? 'default'} />
            </dl>
          </>
        ) : (
          <Empty>Context usage appears after the first model call.</Empty>
        )}
      </Section>

      <Section label="Cache">
        {meta && prompt > 0 ? (
          <>
            <div className="flex items-baseline justify-between">
              <span className="vd-label !text-[9px]">hit rate</span>
              <span className="vd-numeric text-[12px] text-[var(--vd-text)]">
                {Math.round(hitRate * 100)}%
              </span>
            </div>
            <div className="mt-1 flex h-2 overflow-hidden rounded-full bg-[var(--vd-surface-sunken)]">
              <span
                className="h-full w-full origin-left bg-[var(--vd-info)] transition-transform duration-[var(--vd-slow)] ease-[var(--vd-ease)]"
                style={{ transform: `scaleX(${hitRate})` }}
              />
            </div>
            <dl className="mt-3 grid grid-cols-2 gap-2">
              <Stat label="cached" value={formatTokens(cached)} />
              <Stat label="fresh" value={formatTokens(fresh)} />
              <Stat label="prompt" value={formatTokens(prompt)} />
              <Stat label="completion" value={formatTokens(meta.completionTokens ?? 0)} />
            </dl>
            <p className="m-0 mt-2 text-[10.5px] leading-[1.5] text-[var(--vd-text-tertiary)]">
              {formatTokens(cached)} of {formatTokens(prompt)} prompt tokens were served from the
              provider's cache, so only {formatTokens(fresh)} were billed at the full input rate.
            </p>
          </>
        ) : (
          <Empty>
            {meta
              ? 'No prompt tokens reported for this turn yet.'
              : 'Cache figures appear after the first model call.'}
          </Empty>
        )}
      </Section>

      <Section label="Notes">
        <p className="m-0 text-[11px] leading-[1.5] text-[var(--vd-text-tertiary)]">
          Vibe compacts automatically when the window fills. Run{' '}
          <span className="font-mono text-[var(--vd-text-secondary)]">/compact</span> to summarise
          early, or <span className="font-mono text-[var(--vd-text-secondary)]">/clear</span> to
          start from an empty conversation.
        </p>
      </Section>
    </Drawer>
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

/* ------------------------------------------------------------------ *
 * Status / updates
 * ------------------------------------------------------------------ */

export function StatusPanel() {
  const status = useApp((s) => s.status);
  const logs = useApp((s) => s.logs);
  const update = useApp((s) => s.update);
  const environment = useApp((s) => s.environment);
  const themeConfig = useApp((s) => s.themeConfig);
  const themeError = useApp((s) => s.themeError);
  const layoutConfig = useApp((s) => s.layoutConfig);
  const layoutError = useApp((s) => s.layoutError);
  const customizePaths = useApp((s) => s.customizePaths);
  const [busy, setBusy] = useState(false);
  const [applying, setApplying] = useState(false);
  const notify = useApp((s) => s.notify);

  async function resetTheme() {
    try {
      const cleared = await customizeApi.resetTheme();
      useApp.getState().setThemeConfig(cleared);
      useApp.getState().setThemeError(null);
      applyTheme(cleared);
      notify('Theme reset to the built-in palette.', 'success');
    } catch (error) {
      notify(errorMessage(error), 'danger');
    }
  }

  async function resetLayout() {
    try {
      const cleared = await customizeApi.resetLayout();
      useApp.getState().setLayoutConfig(cleared);
      useApp.getState().setLayoutError(null);
      applyLayout(cleared);
      notify('Layout reset to the default geometry.', 'success');
    } catch (error) {
      notify(errorMessage(error), 'danger');
    }
  }

  async function check() {
    setBusy(true);
    try {
      await checkForUpdates(true);
    } finally {
      setBusy(false);
    }
  }

  async function apply() {
    setApplying(true);
    try {
      const next = await updates.apply();
      useApp.getState().setUpdate(next);
      notify(
        next.error ? next.error : 'Upgrade started. VibeDesk restarts the agent when it finishes.',
        next.error ? 'danger' : 'success',
      );
    } catch (error) {
      notify(errorMessage(error), 'danger');
    } finally {
      setApplying(false);
    }
  }

  return (
    <Drawer title="Agent status" subtitle={status?.binary ?? 'vibe-acp'}>
      <Section label="Process">
        <KeyValue label="state" value={status?.state ?? 'offline'} />
        <KeyValue label="pid" value={status?.pid ?? '—'} />
        <KeyValue label="version" value={status?.version ?? '—'} />
        <KeyValue label="restarts" value={String(status?.restarts ?? 0)} />
        <KeyValue label="started" value={relativeTime(status?.startedAt)} />
        <KeyValue label="session" value={status?.sessionId ?? '—'} />
        {status?.error && (
          <p className="m-0 mt-2 flex items-start gap-1.5 text-[11px] text-[var(--vd-danger)]">
            <CircleAlert size={11} strokeWidth={2} className="mt-0.5 shrink-0" />
            {status.error}
          </p>
        )}
      </Section>

      <Section
        label="Updates"
        action={
          <button
            type="button"
            onClick={() => void check()}
            disabled={busy}
            className="vd-btn vd-btn-ghost !h-[22px] !px-1.5"
          >
            <RefreshCw size={11} strokeWidth={2} className={cn(busy && 'vd-spin')} />
          </button>
        }
      >
        <KeyValue
          label="current"
          value={update?.currentVersion ?? environment?.cliVersion ?? '—'}
        />
        <KeyValue label="latest" value={update?.latestVersion ?? 'not checked'} />
        <KeyValue label="channel" value={update?.channel ?? 'stable'} />
        <KeyValue label="install" value={update?.installMethod ?? 'unknown'} />
        <KeyValue label="checked" value={relativeTime(update?.checkedAt)} />

        {update?.updateAvailable && (
          <div className="mt-2 rounded-[var(--radius-sm)] border border-[color-mix(in_oklab,var(--vd-accent)_30%,transparent)] bg-[var(--vd-accent-wash)] px-2.5 py-2">
            <p className="m-0 text-[11.5px] text-[var(--vd-text)]">
              Vibe {update.latestVersion} is available.
            </p>
            {update.upgradeCommand && (
              <p className="m-0 mt-1 font-mono text-[10.5px] text-[var(--vd-text-secondary)]">
                {update.upgradeCommand}
              </p>
            )}
            <div className="mt-2 flex items-center gap-1.5">
              <button
                type="button"
                onClick={() => void apply()}
                disabled={applying}
                className="vd-btn vd-btn-primary !h-[24px]"
              >
                <Download size={11} strokeWidth={2.2} />
                {applying ? 'Upgrading…' : 'Upgrade now'}
              </button>
              {update.releaseUrl && (
                <button
                  type="button"
                  onClick={() =>
                    void import('@tauri-apps/plugin-opener').then((m) =>
                      m.openUrl(update.releaseUrl!),
                    )
                  }
                  className="vd-btn !h-[24px]"
                >
                  <ExternalLink size={11} strokeWidth={2} />
                  Release notes
                </button>
              )}
            </div>
          </div>
        )}

        {update?.error && (
          <p className="m-0 mt-2 text-[10.5px] text-[var(--vd-warn)]">{update.error}</p>
        )}
      </Section>

      <Section label="Storage">
        <KeyValue label="VibeDesk data" value={environment?.dataDir ?? '—'} />
        <KeyValue label="agent home" value={environment?.vibeHome ?? '—'} />
        <p className="m-0 mt-1.5 text-[10.5px] leading-[1.5] text-[var(--vd-text-tertiary)]">
          VibeDesk keeps its own settings next to the executable when that folder is writable, so an
          install on another drive never writes to your user profile. The agent's own config stays
          in its home folder, shared with the CLI.
        </p>
      </Section>

      <Section
        label="Customization"
        action={
          <button
            type="button"
            onClick={() => void resetTheme()}
            className="vd-btn vd-btn-ghost !h-[22px] !px-1.5"
            title="Remove theme.json and go back to the built-in theme"
          >
            Reset
          </button>
        }
      >
        <KeyValue label="theme file" value={customizePaths?.themePath ?? '—'} />
        <KeyValue
          label="tokens"
          value={themeConfig ? String(Object.keys(themeConfig.tokens ?? {}).length) : '0'}
        />
        <KeyValue
          label="rules"
          value={themeConfig ? String((themeConfig.rules ?? []).length) : '0'}
        />
        <KeyValue label="safe mode" value={customizePaths?.safeMode ? 'on' : 'off'} />
        {themeError && (
          <p className="m-0 mt-1.5 whitespace-pre-wrap text-[10.5px] leading-[1.5] text-[var(--vd-warn)]">
            {themeError}
          </p>
        )}
        <p className="m-0 mt-1.5 text-[10.5px] leading-[1.5] text-[var(--vd-text-tertiary)]">
          Edit theme.json and the window updates on save — no restart. Values are validated before
          they apply, and the approval bar, elicitation form, status bar and composer can never be
          restyled.
        </p>
      </Section>

      <Section
        label="Layout"
        action={
          <button
            type="button"
            onClick={() => void resetLayout()}
            className="vd-btn vd-btn-ghost !h-[22px] !px-1.5"
            title="Remove layout.json and go back to the default geometry"
          >
            Reset
          </button>
        }
      >
        <KeyValue label="layout file" value={customizePaths?.layoutPath ?? '—'} />
        <KeyValue label="rail" value={layoutConfig ? `${layoutConfig.railWidth}px` : '—'} />
        <KeyValue
          label="sidebar"
          value={
            layoutConfig
              ? `${layoutConfig.sidebarWidth}px · ${layoutConfig.sidebarSide}${
                  layoutConfig.sidebarCollapsed ? ' · hidden' : ''
                }`
              : '—'
          }
        />
        <KeyValue
          label="measure"
          value={
            layoutConfig ? `${layoutConfig.transcriptMax} / ${layoutConfig.composerMax}px` : '—'
          }
        />
        <KeyValue label="density" value={layoutConfig?.density ?? '—'} />
        <KeyValue label="motion" value={layoutConfig?.motion ?? '—'} />
        {layoutError && (
          <p className="m-0 mt-1.5 whitespace-pre-wrap text-[10.5px] leading-[1.5] text-[var(--vd-warn)]">
            {layoutError}
          </p>
        )}
        <p className="m-0 mt-1.5 text-[10.5px] leading-[1.5] text-[var(--vd-text-tertiary)]">
          Drag the rail's inner edge to resize it. Widths are clamped to a usable range, so a layout
          can look wrong but can never make the window unusable.
        </p>
      </Section>

      <DataPlaneSection />

      <Section label="Bridge log">
        {logs.length === 0 ? (
          <Empty>No bridge events yet.</Empty>
        ) : (
          <div className="max-h-64 overflow-y-auto rounded-[var(--radius-sm)] border border-[var(--vd-line-soft)] bg-[var(--vd-surface-sunken)] p-1.5">
            {logs
              .slice()
              .reverse()
              .map((line, index) => (
                <div
                  key={`${line.at}-${index}`}
                  className="flex gap-1.5 py-0.5 font-mono text-[10px]"
                >
                  <span
                    className={cn(
                      'shrink-0 uppercase',
                      line.level === 'error'
                        ? 'text-[var(--vd-danger)]'
                        : line.level === 'warn'
                          ? 'text-[var(--vd-warn)]'
                          : 'text-[var(--vd-text-tertiary)]',
                    )}
                  >
                    {line.level}
                  </span>
                  <span className="min-w-0 break-words text-[var(--vd-text-secondary)]">
                    {line.message}
                  </span>
                </div>
              ))}
          </div>
        )}
      </Section>
    </Drawer>
  );
}

/* ------------------------------------------------------------------ *
 * Data plane
 * ------------------------------------------------------------------ */

/**
 * The kill switch and the source list.
 *
 * Pausing is a Rust-side flag, not a frontend one: a paused plane stops its
 * loops, so nothing is fetched while it is on. The list is read from Rust rather
 * than derived from the panel config, because the config says what *should* run
 * and this says what *is* running — the difference is the whole point of a
 * status view.
 */
function DataPlaneSection() {
  const status = usePanelData((s) => s.status);
  const paused = usePanelData((s) => s.paused);
  const setStatus = usePanelData((s) => s.setStatus);
  const notify = useApp((s) => s.notify);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    void dataPlane
      .status()
      .then(setStatus)
      .catch(() => {
        /* a plane that cannot report is shown as unknown, not as broken */
      });
  }, [setStatus]);

  async function toggle() {
    setBusy(true);
    try {
      const next = await dataPlane.setPaused(!paused);
      setStatus(next);
      notify(
        next.paused ? 'Data plane paused. Panels keep their last values.' : 'Data plane resumed.',
        'success',
      );
    } catch (error) {
      notify(errorMessage(error), 'danger');
    } finally {
      setBusy(false);
    }
  }

  const sources = status?.sources ?? [];
  const running = sources.filter((source) => source.running).length;

  return (
    <Section
      label="Data plane"
      action={
        <button
          type="button"
          onClick={() => void toggle()}
          disabled={busy}
          className="vd-btn vd-btn-ghost !h-[22px] !px-1.5"
          title={paused ? 'Resume polling' : 'Stop all polling'}
        >
          {paused ? 'Resume' : 'Pause'}
        </button>
      }
    >
      <KeyValue label="state" value={paused ? 'paused' : 'live'} />
      <KeyValue label="sources" value={`${running} running / ${sources.length} declared`} />
      <KeyValue label="safe mode" value={status?.safeMode ? 'on' : 'off'} />

      {sources.length === 0 ? (
        <Empty>No panel declares a source.</Empty>
      ) : (
        <div className="mt-1.5 flex flex-col gap-1">
          {sources.map((source) => (
            <div
              key={`${source.panelId}/${source.sourceId}`}
              className="flex items-start gap-1.5 rounded-[var(--radius-sm)] border border-[var(--vd-line-soft)] bg-[var(--vd-surface-sunken)] px-1.5 py-1"
            >
              <span
                className="mt-[5px] h-[5px] w-[5px] shrink-0 rounded-full"
                style={{
                  background: source.running ? 'var(--vd-accent)' : 'var(--vd-text-faint)',
                }}
                aria-hidden
              />
              <div className="min-w-0 flex-1">
                <div className="flex items-baseline gap-1.5">
                  <span className="truncate text-[11px] text-[var(--vd-text)]">
                    {source.sourceId}
                  </span>
                  <span className="shrink-0 text-[9.5px] uppercase tracking-[0.04em] text-[var(--vd-text-tertiary)]">
                    {source.kind}
                    {source.mode ? ` · ${source.mode}` : ''}
                  </span>
                </div>
                <div className="truncate font-mono text-[10px] text-[var(--vd-text-tertiary)]">
                  {source.target}
                </div>
                <div className="text-[9.5px] text-[var(--vd-text-faint)]">
                  {source.panelId}
                  {source.intervalMs ? ` · every ${source.intervalMs}ms` : ''}
                </div>
              </div>
            </div>
          ))}
        </div>
      )}

      {status && (
        <p className="m-0 mt-1.5 text-[10.5px] leading-[1.5] text-[var(--vd-text-tertiary)]">
          A panel source may only address loopback — {status.limits.maxBodyBytes / 1024} KiB per
          response, {status.limits.maxConcurrent} fetches at once, intervals between{' '}
          {status.limits.intervalMin}ms and {status.limits.intervalMax}ms. Pausing stops every loop;
          panels keep the last value they had.
        </p>
      )}
    </Section>
  );
}

/* ------------------------------------------------------------------ *
 * Skills
 * ------------------------------------------------------------------ */

export function SkillsPanel() {
  const [skills, setSkills] = useState<SkillRecord[]>([]);
  const [loading, setLoading] = useState(true);
  const addNotice = useApp((s) => s.addNotice);
  const usedSkills = useApp((s) => s.usedSkills);
  const swarmSkills = useApp((s) => s.swarmSkills);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const list = await vibe.skills();
        if (!cancelled) setSkills(list);
      } catch (error) {
        if (!cancelled) addNotice(errorMessage(error), 'error');
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [addNotice]);

  const grouped = useMemo(() => {
    const map = new Map<string, SkillRecord[]>();
    for (const skill of skills) {
      // A mastery bundle is not a plugin: it is a cloned upstream repo the
      // mastery store enabled. Grouping it under "plugin ·" would name the
      // wrong owner, and the user reads that label to decide where a skill
      // came from.
      const key = skill.plugin
        ? `${skill.scope === 'mastery' ? 'mastery' : 'plugin'} · ${skill.plugin}`
        : skill.scope;
      const list = map.get(key) ?? [];
      list.push(skill);
      map.set(key, list);
    }
    return [...map.entries()];
  }, [skills]);

  // Newest load first: the panel is read to answer "what did this chat use",
  // and the answer is usually the last thing it opened.
  const used = useMemo(
    () => Object.values(usedSkills).sort((a, b) => b.at - a.at),
    [usedSkills],
  );

  return (
    <Drawer
      title="Skills"
      subtitle={
        used.length > 0
          ? `${skills.length} discovered · ${used.length} used in this chat`
          : `${skills.length} discovered`
      }
    >
      <Section label="Swarm">
        <div className="flex items-center gap-2">
          <span className="text-[11px] text-[var(--vd-text-secondary)]">
            Hand these to every swarm run
          </span>
          <button
            type="button"
            className="vd-switch ml-auto"
            data-on={swarmSkills}
            aria-pressed={swarmSkills}
            onClick={() => void toggleSwarmSkills()}
            title={
              swarmSkills
                ? 'Runs are handed the method this machine has — turn off to compare against a run that works from the brief alone'
                : 'Runs work from the brief alone'
            }
          />
        </div>
        <p className="m-0 mt-1.5 text-[10px] leading-[1.5] text-[var(--vd-text-tertiary)]">
          {swarmSkills ? (
            <>
              Before a run plans, the catalogue is ranked against its goal and the lead is
              given the shortlist. Each task is assigned the skills it needs, the worker is
              told the exact file to load, and the engine reads the worker&apos;s own tool
              calls to see whether it did. A run that invents a method this machine already
              has is the failure this prevents.
            </>
          ) : (
            <>
              Runs are not handed any skill. They work from the brief alone, which is what a
              run did before this existed — useful for comparing two runs, and the reason the
              switch is here rather than buried in a config file.
            </>
          )}
        </p>
      </Section>

      {used.length > 0 && (
        <Section label={`Used in this chat · ${used.length}`}>
          <div className="flex flex-wrap gap-1">
            {used.map((entry) => (
              <span
                key={entry.name}
                className="vd-aura vd-chip vd-chip-accent !h-[20px] !text-[10px]"
                title={`${entry.name}${entry.bundle ? ` · ${entry.bundle}` : ''} — loaded ${entry.count}×`}
              >
                <Sparkles size={9} strokeWidth={2.4} />
                {entry.name}
                {entry.count > 1 && (
                  <span className="vd-numeric opacity-70">×{entry.count}</span>
                )}
              </span>
            ))}
          </div>
          <p className="m-0 mt-1.5 text-[10px] leading-[1.5] text-[var(--vd-text-tertiary)]">
            Read from the tool calls that loaded them, not from the catalog. A skill that is
            installed but never loaded does not appear here.
          </p>
        </Section>
      )}

      {loading ? (
        <div className="p-3">
          <Empty>Scanning skill directories…</Empty>
        </div>
      ) : skills.length === 0 ? (
        <div className="p-3">
          <Empty>
            No skills found. Add a <span className="font-mono">SKILL.md</span> under{' '}
            <span className="font-mono">~/.vibe/skills/</span> or{' '}
            <span className="font-mono">.vibe/skills/</span>.
          </Empty>
        </div>
      ) : (
        grouped.map(([group, list]) => (
          <Section key={group} label={`${group} · ${list.length}`}>
            <div className="flex flex-col gap-1">
              {list.map((skill) => {
                const use = usedSkills[skill.name];
                return (
                  <div
                    key={`${skill.source}:${skill.name}`}
                    className={cn('vd-card px-2.5 py-1.5', use && 'vd-aura')}
                    title={use ? `Loaded ${use.count}× in this chat` : undefined}
                  >
                    <div className="flex items-center gap-1.5">
                      <Sparkles
                        size={10}
                        strokeWidth={2}
                        className={cn(
                          'shrink-0',
                          use ? 'text-[var(--vd-accent)]' : 'text-[var(--vd-text-tertiary)]',
                        )}
                      />
                      <span className="truncate text-[11.5px] font-medium text-[var(--vd-text)]">
                        {skill.name}
                      </span>
                      {use && (
                        <span className="vd-chip vd-chip-accent !h-[16px] !text-[8.5px]">
                          used{use.count > 1 ? ` ×${use.count}` : ''}
                        </span>
                      )}
                      {skill.userInvocable && (
                        <span className="vd-chip !h-[16px] !text-[8.5px]">/{skill.name}</span>
                      )}
                      {!skill.enabled && (
                        <span className="vd-chip vd-chip-warn ml-auto !h-[16px] !text-[8.5px]">
                          disabled
                        </span>
                      )}
                    </div>
                    <p className="m-0 mt-0.5 line-clamp-2 text-[10.5px] leading-[1.4] text-[var(--vd-text-tertiary)]">
                      {skill.description}
                    </p>
                  </div>
                );
              })}
            </div>
          </Section>
        ))
      )}
    </Drawer>
  );
}

/* ------------------------------------------------------------------ *
 * Release notes
 * ------------------------------------------------------------------ */

export function ReleaseNotesPanel() {
  const [notes, setNotes] = useState<ReleaseNote[]>([]);
  const [loading, setLoading] = useState(true);
  const [active, setActive] = useState<string | null>(null);
  const addNotice = useApp((s) => s.addNotice);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const list = await vibe.releaseNotes();
        if (cancelled) return;
        setNotes(list);
        setActive(list[0]?.version ?? null);
      } catch (error) {
        if (!cancelled) addNotice(errorMessage(error), 'error');
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [addNotice]);

  const current = notes.find((n) => n.version === active);

  return (
    <Drawer title="Release notes" subtitle={`${notes.length} versions`}>
      {loading ? (
        <div className="p-3">
          <Empty>Reading bundled release notes…</Empty>
        </div>
      ) : notes.length === 0 ? (
        <div className="p-3">
          <Empty>
            Release notes ship inside the Vibe package. Run{' '}
            <span className="font-mono">vibe --version</span> to confirm the install.
          </Empty>
        </div>
      ) : (
        <>
          <div className="flex flex-wrap gap-1 border-b border-[var(--vd-line-soft)] px-3 py-2">
            {notes.slice(0, 24).map((note) => (
              <button
                key={note.version}
                type="button"
                onClick={() => setActive(note.version)}
                className={cn('vd-chip ', active === note.version && 'vd-chip-accent')}
              >
                {note.version}
              </button>
            ))}
          </div>
          <div className="px-3 py-3">
            {current ? <Markdown>{current.body}</Markdown> : <Empty>Select a version.</Empty>}
          </div>
        </>
      )}
    </Drawer>
  );
}

/* ------------------------------------------------------------------ *
 * Account
 * ------------------------------------------------------------------ */

export function AccountPanel() {
  const [identity, setIdentity] = useState<JsonValue | null>(null);
  const [account, setAccount] = useState<JsonValue | null>(null);
  const [error, setError] = useState<string | null>(null);
  const environment = useApp((s) => s.environment);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const [id, acc] = await Promise.all([vibe.whoami(), vibe.authStatus()]);
        if (cancelled) return;
        setIdentity(id);
        setAccount(acc);
      } catch (err) {
        if (!cancelled) setError(errorMessage(err));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  return (
    <Drawer title="Account" subtitle="Mistral identity and plan">
      <Section label="Identity">
        {error ? (
          <Empty>{error}</Empty>
        ) : identity ? (
          <pre className="vd-code m-0 max-h-64 overflow-auto rounded-[var(--radius-sm)] border border-[var(--vd-line-soft)] bg-[var(--vd-surface-sunken)] p-2 text-[10.5px] text-[var(--vd-text-secondary)]">
            {JSON.stringify(identity, null, 2)}
          </pre>
        ) : (
          <Empty>Reading identity…</Empty>
        )}
      </Section>

      <Section label="Auth">
        {account ? (
          <pre className="vd-code m-0 max-h-48 overflow-auto rounded-[var(--radius-sm)] border border-[var(--vd-line-soft)] bg-[var(--vd-surface-sunken)] p-2 text-[10.5px] text-[var(--vd-text-secondary)]">
            {JSON.stringify(account, null, 2)}
          </pre>
        ) : (
          <Empty>No auth state reported.</Empty>
        )}
      </Section>

      <Section label="Environment">
        <KeyValue label="vibe home" value={environment?.vibeHome ?? '—'} />
        <KeyValue label="api key" value={environment?.apiKeyPresent ? 'present' : 'missing'} />
        <KeyValue label="model" value={environment?.activeModel ?? 'default'} />
      </Section>
    </Drawer>
  );
}

/* ------------------------------------------------------------------ *
 * Help
 * ------------------------------------------------------------------ */

const SHORTCUTS: Array<[string, string]> = [
  ['Enter', 'Send the prompt'],
  ['Shift + Enter', 'New line'],
  ['Esc', 'Interrupt the running turn, or close an open panel'],
  ['/', 'Open the command palette'],
  ['@', 'Reference a workspace file'],
  ['↑ / ↓', 'Move through the palette'],
  ['Tab / Enter', 'Accept the highlighted palette row'],
  ['Ctrl + R', 'Reload the session'],
  ['Ctrl + Shift + P', 'Reload plugins'],
  ['Ctrl + K', 'Focus the composer'],
  ['Right-click', 'Menu for the item under the cursor'],
  ['Shift + Right-click', 'Native webview menu (inspect)'],
];

export function HelpPanel() {
  // The frontend is embedded at compile time, so the source tree cannot say
  // which UI is running. Reading it from the binary is the only honest answer,
  // and it is what makes a stale build identifiable rather than mysterious.
  const [build, setBuild] = useState<BuildInfo | null>(null);
  // The boot notice is a chat item, so switching chats clears it. A capability
  // this sharp needs somewhere durable to be read back, or the user loses the
  // only sign that the window is drivable.
  const [debugPort, setDebugPort] = useState<number | null>(null);

  useEffect(() => {
    let live = true;
    buildApi
      .info()
      .then((info) => {
        if (live) setBuild(info);
      })
      .catch(() => {
        /* a build that cannot report is not worth an error */
      });
    debugApi
      .endpoint()
      .then((port) => {
        if (live) setDebugPort(port);
      })
      .catch(() => {
        /* an endpoint that cannot report is not worth an error */
      });
    return () => {
      live = false;
    };
  }, []);

  return (
    <Drawer title="Help" subtitle="Shortcuts and behaviour">
      <Section label="Keyboard">
        <div className="flex flex-col gap-1">
          {SHORTCUTS.map(([keys, description]) => (
            <div key={keys} className="flex items-center gap-2">
              <span className="flex shrink-0 gap-1">
                {keys.split(' + ').map((key) => (
                  <kbd key={key} className="vd-kbd">
                    {key}
                  </kbd>
                ))}
              </span>
              <span className="text-[11px] text-[var(--vd-text-secondary)]">{description}</span>
            </div>
          ))}
        </div>
      </Section>

      <Section label="How it works">
        <ul className="m-0 list-none space-y-1.5 p-0 text-[11px] leading-[1.5] text-[var(--vd-text-tertiary)]">
          <li className="flex gap-1.5">
            <Check
              size={11}
              strokeWidth={2.4}
              className="mt-0.5 shrink-0 text-[var(--vd-accent)]"
            />
            <span>
              VibeDesk spawns <span className="font-mono">vibe-acp</span> as a child process and
              speaks JSON-RPC 2.0 over stdio.
            </span>
          </li>
          <li className="flex gap-1.5">
            <Check
              size={11}
              strokeWidth={2.4}
              className="mt-0.5 shrink-0 text-[var(--vd-accent)]"
            />
            <span>
              Tool approvals are forwarded to the UI; nothing runs without your decision unless the
              agent profile auto-approves it.
            </span>
          </li>
          <li className="flex gap-1.5">
            <Check
              size={11}
              strokeWidth={2.4}
              className="mt-0.5 shrink-0 text-[var(--vd-accent)]"
            />
            <span>
              Plugins are pinned per session. Reloading plugins re-discovers them and re-pins the
              snapshot.
            </span>
          </li>
          <li className="flex gap-1.5">
            <BookOpen
              size={11}
              strokeWidth={2.4}
              className="mt-0.5 shrink-0 text-[var(--vd-info)]"
            />
            <span>Docs: docs.mistral.ai/vibe/code/overview</span>
          </li>
        </ul>
      </Section>

      <Section label="Build">
        <div className="flex flex-col gap-1">
          <KeyValue label="version" value={build?.version ?? '—'} />
          <KeyValue label="bundle" value={build?.bundle ?? '—'} />
          <KeyValue
            label="debugging"
            value={debugPort === null ? 'off' : `127.0.0.1:${debugPort}`}
          />
        </div>
        {debugPort !== null && (
          <p className="m-0 mt-2 flex gap-1.5 text-[11px] leading-[1.5] text-[var(--vd-danger)]">
            <CircleAlert size={11} strokeWidth={2.4} className="mt-0.5 shrink-0" />
            <span>
              Remote debugging is on at 127.0.0.1:{debugPort} — any local process can drive this
              window.
            </span>
          </p>
        )}
        {build?.distStale && (
          <p className="m-0 mt-2 flex gap-1.5 text-[11px] leading-[1.5] text-[var(--vd-danger)]">
            <CircleAlert size={11} strokeWidth={2.4} className="mt-0.5 shrink-0" />
            <span>
              This binary embedded a stale frontend. Run{' '}
              <span className="font-mono">npm run build</span> then{' '}
              <span className="font-mono">cargo build</span>.
            </span>
          </p>
        )}
      </Section>

      <Section label="Legend">
        <div className="flex flex-col gap-1 text-[11px] text-[var(--vd-text-secondary)]">
          <span className="flex items-center gap-1.5">
            <Gauge size={11} strokeWidth={2} /> context gauge in the status bar
          </span>
          <span className="flex items-center gap-1.5">
            <UserRound size={11} strokeWidth={2} /> agent profile chip in the composer
          </span>
          <span className="flex items-center gap-1.5">
            <ArrowDownToLine size={11} strokeWidth={2} /> update badge in the title bar
          </span>
          <span className="flex items-center gap-1.5">
            <Keyboard size={11} strokeWidth={2} /> shortcuts above
          </span>
        </div>
      </Section>
    </Drawer>
  );
}
