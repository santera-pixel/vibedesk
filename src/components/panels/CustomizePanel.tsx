import { useEffect, useMemo, useState } from 'react';
import type { ReactNode } from 'react';
import { AlertTriangle, Plus, RotateCcw, X } from 'lucide-react';
import type {
  ActionRun,
  BuiltinCommand,
  CustomAction,
  CustomPanel,
  Density,
  ElicitationAnswer,
  LayoutConfig,
  Motion,
  PermissionScope,
  PluginSource,
  Side,
  ThemeConfig,
} from '@/lib/customize';
import { applyLayout, applyTheme, defaultLayout } from '@/lib/customize';
import { clamp, cn } from '@/lib/utils';
import { customize, sources as sourcesApi } from '@/lib/bridge';
import { errorMessage } from '@/lib/actions';
import { useApp } from '@/store/app';
import { Drawer, Empty, Section } from '@/components/chrome/Drawer';

const THEME_TOKENS = [
  '--vd-accent',
  '--vd-accent-strong',
  '--vd-canvas',
  '--vd-surface',
  '--vd-text',
  '--vd-line',
];

type LayoutRangeKey = 'railWidth' | 'sidebarWidth' | 'transcriptMax' | 'composerMax';

const LAYOUT_RANGES: { key: LayoutRangeKey; label: string; min: number; max: number }[] = [
  { key: 'railWidth', label: 'Rail width', min: 40, max: 72 },
  { key: 'sidebarWidth', label: 'Sidebar width', min: 200, max: 520 },
  { key: 'transcriptMax', label: 'Transcript max', min: 480, max: 1100 },
  { key: 'composerMax', label: 'Composer max', min: 480, max: 1100 },
];

const RUN_KINDS: ActionRun['kind'][] = [
  'prompt',
  'command',
  'open-url',
  'reveal',
  'mode',
  'option',
  'cancel',
  'approve',
  'reject',
  'answer',
];

const BUILTIN_COMMANDS: BuiltinCommand[] = [
  'reload-session',
  'reload-plugins',
  'new-chat',
  'toggle-theme',
  'toggle-sidebar',
  'focus-composer',
  'check-updates',
  'open-plugins',
  'open-config',
  'open-sessions',
  'open-context',
  'open-skills',
  'open-customize',
  'open-help',
  'open-swarm',
];

const SIDES: { value: Side; label: string }[] = [
  { value: 'left', label: 'Left' },
  { value: 'right', label: 'Right' },
];

const DENSITIES: { value: Density; label: string }[] = [
  { value: 'compact', label: 'Compact' },
  { value: 'comfortable', label: 'Comfortable' },
  { value: 'roomy', label: 'Roomy' },
];

const SCOPES: { value: PermissionScope; label: string }[] = [
  { value: 'once', label: 'Once' },
  { value: 'always', label: 'Always' },
];

const ANSWERS: { value: ElicitationAnswer; label: string }[] = [
  { value: 'accept', label: 'Accept' },
  { value: 'decline', label: 'Decline' },
  { value: 'cancel', label: 'Cancel' },
];

const MOTIONS: { value: Motion; label: string }[] = [
  { value: 'system', label: 'System' },
  { value: 'full', label: 'Full' },
  { value: 'reduced', label: 'Reduced' },
];

const FIELD_CLASS =
  'w-full rounded-[var(--radius-sm)] border border-[var(--vd-line)] bg-[var(--vd-surface-sunken)] px-1.5 py-1 text-[11.5px] text-[var(--vd-text)] outline-none duration-[var(--vd-fast)] placeholder:text-[var(--vd-text-tertiary)] focus:border-[var(--vd-line-strong)]';

const ADD_BUTTON_CLASS = 'vd-btn vd-btn-primary !h-[26px] self-start !px-2 !text-[11px]';

const FORM_CARD_CLASS =
  'mt-2 rounded-[var(--radius-md)] border border-[var(--vd-line)] bg-[var(--vd-surface)] p-2';

/** The value a token resolves to right now, user theme included. */
function readToken(name: string): string {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}

/** A colour input only accepts `#rrggbb`, so anything else falls back. */
function toHex(value: string): string {
  const raw = value.trim();
  if (/^#[0-9a-f]{6}$/i.test(raw)) return raw;
  if (/^#[0-9a-f]{3}$/i.test(raw)) {
    return `#${raw[1]}${raw[1]}${raw[2]}${raw[2]}${raw[3]}${raw[3]}`;
  }
  return '#000000';
}

/** Lowercase, dash-separated, at most 32 characters, unique within `taken`. */
function slugId(label: string, taken: Set<string>): string {
  const base =
    label
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, '-')
      .replace(/^-+|-+$/g, '')
      .slice(0, 32)
      .replace(/-+$/, '') || 'action';
  if (!taken.has(base)) return base;
  let suffix = 2;
  while (taken.has(`${base}-${suffix}`)) suffix += 1;
  return `${base}-${suffix}`;
}

/** A panel a plugin contributed. Rust namespaces its id and sets `plugin`. */
function isPluginPanel(panel: CustomPanel): boolean {
  return Boolean(panel.plugin) || panel.id.startsWith('plugin:');
}

export function CustomizePanel() {
  const themeConfig = useApp((s) => s.themeConfig);
  const themeError = useApp((s) => s.themeError);
  const setThemeConfig = useApp((s) => s.setThemeConfig);
  const setThemeError = useApp((s) => s.setThemeError);
  const layoutConfig = useApp((s) => s.layoutConfig);
  const layoutError = useApp((s) => s.layoutError);
  const setLayoutConfig = useApp((s) => s.setLayoutConfig);
  const setLayoutError = useApp((s) => s.setLayoutError);
  const actionsConfig = useApp((s) => s.actionsConfig);
  const actionsError = useApp((s) => s.actionsError);
  const setActionsConfig = useApp((s) => s.setActionsConfig);
  const setActionsError = useApp((s) => s.setActionsError);
  const panelsConfig = useApp((s) => s.panelsConfig);
  const panelsError = useApp((s) => s.panelsError);
  const setPanelsConfig = useApp((s) => s.setPanelsConfig);
  const setPanelsError = useApp((s) => s.setPanelsError);
  const paths = useApp((s) => s.customizePaths);
  const pluginSources = useApp((s) => s.pluginSources);
  const setPluginSources = useApp((s) => s.setPluginSources);
  const pending = useApp((s) => s.pending);
  // The mode and option pickers below offer what the agent actually advertises,
  // so an action cannot be built around an id that does not exist. With no
  // session there is nothing to offer, and the field falls back to free text.
  const availableModes = useApp((s) => s.modes?.availableModes ?? []);
  const configOptions = useApp((s) => s.configOptions);
  const notify = useApp((s) => s.notify);
  const addNotice = useApp((s) => s.addNotice);

  const actions = actionsConfig?.actions ?? [];
  const allPanels = panelsConfig?.panels ?? [];
  // The editor owns the user's file only. A plugin panel is read-only here:
  // saving it back would persist it into panels.json, where Rust refuses it.
  const panels = allPanels.filter((panel) => !isPluginPanel(panel));
  const pluginPanels = allPanels.filter(isPluginPanel);
  const layout = layoutConfig ?? defaultLayout();

  const applyThemeConfig = (saved: ThemeConfig) => {
    setThemeConfig(saved);
    setThemeError(null);
    applyTheme(saved);
  };

  const applyLayoutConfig = (saved: LayoutConfig) => {
    setLayoutConfig(saved);
    setLayoutError(null);
    applyLayout(saved);
  };

  async function resetSection<T>(
    call: () => Promise<T>,
    apply: (saved: T) => void,
    message: string,
    onError: (message: string) => void,
  ) {
    try {
      apply(await call());
      notify(message, 'success');
    } catch (error) {
      const text = errorMessage(error);
      onError(text);
      addNotice(text, 'error');
    }
  }

  /* ---------------------------------------------------------------- *
   * Theme
   * ---------------------------------------------------------------- */

  const baseTokens = useMemo(() => {
    const map: Record<string, string> = {};
    for (const name of THEME_TOKENS) map[name] = themeConfig?.tokens[name] ?? readToken(name);
    return map;
  }, [themeConfig]);

  const [drafts, setDrafts] = useState<Record<string, string>>(baseTokens);

  useEffect(() => {
    setDrafts(baseTokens);
  }, [baseTokens]);

  async function commitTheme() {
    const tokens: Record<string, string> = { ...(themeConfig?.tokens ?? {}) };
    let dirty = false;

    for (const name of THEME_TOKENS) {
      const value = (drafts[name] ?? '').trim();
      if (!value) continue;
      if (value === baseTokens[name]) {
        if (name in tokens) {
          delete tokens[name];
          dirty = true;
        }
      } else if (tokens[name] !== value) {
        tokens[name] = value;
        dirty = true;
      }
    }

    if (!dirty) return;

    try {
      applyThemeConfig(await customize.setTheme({ tokens, rules: themeConfig?.rules ?? [] }));
    } catch (error) {
      const message = errorMessage(error);
      setThemeError(message);
      addNotice(message, 'error');
    }
  }

  const resetTheme = () =>
    resetSection(customize.resetTheme, applyThemeConfig, 'Theme reset.', setThemeError);

  /* ---------------------------------------------------------------- *
   * Layout
   * ---------------------------------------------------------------- */

  async function commitLayout(patch: Partial<LayoutConfig>) {
    const previous = layout;
    setLayoutConfig({ ...previous, ...patch });
    try {
      applyLayoutConfig(await customize.setLayout({ ...previous, ...patch }));
    } catch (error) {
      const message = errorMessage(error);
      setLayoutConfig(previous);
      setLayoutError(message);
      addNotice(message, 'error');
    }
  }

  const resetLayout = () =>
    resetSection(customize.resetLayout, applyLayoutConfig, 'Layout reset.', setLayoutError);

  /* ---------------------------------------------------------------- *
   * Actions
   * ---------------------------------------------------------------- */

  const [actionForm, setActionForm] = useState({
    label: '',
    kind: 'prompt' as ActionRun['kind'],
    text: '',
    command: 'reload-session' as BuiltinCommand,
    url: '',
    path: '',
    mode: '',
    option: '',
    value: '',
    scope: 'once' as PermissionScope,
    answer: 'decline' as ElicitationAnswer,
    content: '',
  });

  const patchAction = (patch: Partial<typeof actionForm>) =>
    setActionForm((current) => ({ ...current, ...patch }));

  async function saveActions(next: CustomAction[]): Promise<boolean> {
    try {
      setActionsConfig(await customize.setActions({ actions: next }));
      setActionsError(null);
      return true;
    } catch (error) {
      const message = errorMessage(error);
      setActionsError(message);
      addNotice(message, 'error');
      return false;
    }
  }

  function buildRun(): ActionRun | null {
    const { kind, text, command, url, path, mode, option, value, scope, answer, content } =
      actionForm;
    if (kind === 'command') return { kind, command };
    if (kind === 'prompt') return text.trim() ? { kind, text: text.trim() } : null;
    if (kind === 'open-url') return url.trim() ? { kind, url: url.trim() } : null;
    if (kind === 'reveal') return path.trim() ? { kind, path: path.trim() } : null;
    if (kind === 'mode') return mode.trim() ? { kind, mode: mode.trim() } : null;
    if (kind === 'option') {
      return option.trim() && value.trim()
        ? { kind, option: option.trim(), value: value.trim() }
        : null;
    }
    if (kind === 'approve' || kind === 'reject') return { kind, scope };
    if (kind === 'answer') {
      if (answer !== 'accept') return { kind, action: answer };
      // The content is a literal object of form values. Parsed here rather than
      // stored as a string so a malformed one is refused before it is saved,
      // not after the agent rejects it.
      if (!content.trim()) return { kind, action: 'accept', content: {} };
      try {
        const parsed: unknown = JSON.parse(content);
        if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) return null;
        return { kind, action: 'accept', content: parsed as Record<string, unknown> };
      } catch {
        return null;
      }
    }
    return { kind: 'cancel' };
  }

  async function addAction() {
    const label = actionForm.label.trim();
    const run = buildRun();
    if (!label || !run) return;
    const taken = new Set(actions.map((action) => action.id));
    const saved = await saveActions([
      ...actions,
      { id: slugId(label, taken), label, slot: 'composer', run },
    ]);
    if (saved) patchAction({ label: '', text: '', url: '', path: '' });
  }

  const resetActions = () =>
    resetSection(customize.resetActions, setActionsConfig, 'Actions reset.', setActionsError);

  /* ---------------------------------------------------------------- *
   * Panels
   * ---------------------------------------------------------------- */

  const [panelForm, setPanelForm] = useState({ title: '', width: 320 });

  async function savePanels(next: CustomPanel[]): Promise<boolean> {
    try {
      setPanelsConfig(await customize.setPanels({ panels: next }));
      setPanelsError(null);
      return true;
    } catch (error) {
      const message = errorMessage(error);
      setPanelsError(message);
      addNotice(message, 'error');
      return false;
    }
  }

  async function addPanel() {
    const title = panelForm.title.trim();
    if (!title) return;
    const taken = new Set(panels.map((panel) => panel.id));
    const blocks = [
      { kind: 'text' as const, text: 'This panel is empty. Add blocks to panels.json.' },
    ];
    const saved = await savePanels([
      ...panels,
      { id: slugId(title, taken), title, width: panelForm.width, blocks },
    ]);
    if (saved) setPanelForm((current) => ({ ...current, title: '' }));
  }

  const resetPanels = () =>
    resetSection(customize.resetPanels, setPanelsConfig, 'Panels reset.', setPanelsError);

  /* ---------------------------------------------------------------- *
   * Sources
   *
   * A plugin ships a config; the agent writes a proposal. Neither is
   * applied by being present, so this is where the user picks. The
   * pending half lives in the bar above the composer, where it is
   * impossible to miss; this section is the plugin half plus a count.
   * ---------------------------------------------------------------- */

  const [sourceBusy, setSourceBusy] = useState<string | null>(null);
  const [pluginPanelIssues, setPluginPanelIssues] = useState<string[]>([]);

  /**
   * Why a plugin's panels are missing.
   *
   * A plugin panel is read on every load instead of being listed for approval,
   * so a rejected one leaves no trace anywhere else — the panel is simply not
   * in the rail. This is the only place that says which plugin and why.
   */
  async function refreshPluginPanelIssues() {
    try {
      setPluginPanelIssues(await customize.pluginIssues());
    } catch {
      // A backend that cannot report is shown as "nothing to report" rather
      // than as an error: the panels that did load are still on screen.
    }
  }

  useEffect(() => {
    void refreshPluginPanelIssues();
  }, []);

  async function refreshSources() {
    try {
      setPluginSources(await sourcesApi.plugins());
    } catch (error) {
      addNotice(errorMessage(error), 'error');
    }
    await refreshPluginPanelIssues();
  }

  async function applySource(source: PluginSource) {
    setSourceBusy(`${source.plugin}:${source.kind}`);
    try {
      await sourcesApi.applyPlugin(source.plugin, source.kind, source.digest);
      addNotice(`Applied ${source.plugin}'s ${source.kind}.`, 'success');
    } catch (error) {
      addNotice(errorMessage(error), 'error');
    } finally {
      setSourceBusy(null);
    }
  }

  return (
    <Drawer title="Customize" subtitle={`${actions.length} actions · ${panels.length} panels`}>
      <Section label="Appearance">
        <div className="flex flex-col gap-1.5">
          {THEME_TOKENS.map((name) => (
            <TokenRow
              key={name}
              name={name}
              value={drafts[name] ?? ''}
              onChange={(value) => setDrafts((current) => ({ ...current, [name]: value }))}
              onCommit={() => void commitTheme()}
            />
          ))}
        </div>
        {themeError && <WarnBlock>{themeError}</WarnBlock>}
        <div className="mt-2 flex items-center gap-2">
          <ResetButton label="Reset theme" onClick={() => void resetTheme()} />
          <span className="text-[10px] text-[var(--vd-text-tertiary)]">Saves on blur or Enter</span>
        </div>
      </Section>

      <Section label="Layout">
        <div className="flex flex-col gap-2.5">
          {LAYOUT_RANGES.map((range) => (
            <NumberRow
              key={range.key}
              label={range.label}
              value={layout[range.key]}
              min={range.min}
              max={range.max}
              onChange={(value) => void commitLayout({ [range.key]: value })}
            />
          ))}
          <div>
            <span className="text-[11px] text-[var(--vd-text-secondary)]">Sidebar side</span>
            <Segmented
              className="mt-1"
              value={layout.sidebarSide}
              options={SIDES}
              onChange={(sidebarSide) => void commitLayout({ sidebarSide })}
            />
          </div>
          <div>
            <span className="text-[11px] text-[var(--vd-text-secondary)]">Density</span>
            <Segmented
              className="mt-1"
              value={layout.density}
              options={DENSITIES}
              onChange={(density) => void commitLayout({ density })}
            />
          </div>
          <div>
            <span className="text-[11px] text-[var(--vd-text-secondary)]">Motion</span>
            <Segmented
              className="mt-1"
              value={layout.motion}
              options={MOTIONS}
              onChange={(motion) => void commitLayout({ motion })}
            />
            <p className="mt-1 text-[10px] leading-snug text-[var(--vd-text-tertiary)]">
              {layout.motion === 'system'
                ? 'Follows the operating system. Windows reports reduced motion whenever "Animation effects" is off.'
                : layout.motion === 'full'
                  ? 'Animates regardless of the system setting. The theme reveal spreads from the pointer.'
                  : 'No transitions and no theme reveal, regardless of the system setting.'}
            </p>
          </div>
          <div className="flex items-center gap-2">
            <span className="text-[11px] text-[var(--vd-text-secondary)]">Collapse sidebar</span>
            <button
              type="button"
              className="vd-switch ml-auto"
              data-on={layout.sidebarCollapsed}
              aria-pressed={layout.sidebarCollapsed}
              onClick={() => void commitLayout({ sidebarCollapsed: !layout.sidebarCollapsed })}
              title={layout.sidebarCollapsed ? 'Expand sidebar' : 'Collapse sidebar'}
            />
          </div>
        </div>
        {layoutError && <WarnBlock>{layoutError}</WarnBlock>}
        <div className="mt-2">
          <ResetButton label="Reset layout" onClick={() => void resetLayout()} />
        </div>
      </Section>

      <Section label="Actions">
        {actions.length === 0 ? (
          <Empty>No custom actions yet. Add one below to pin it to the composer.</Empty>
        ) : (
          <div className="flex flex-col gap-1">
            {actions.map((action) => (
              <ActionRow
                key={action.id}
                action={action}
                onDelete={() => void saveActions(actions.filter((item) => item.id !== action.id))}
              />
            ))}
          </div>
        )}

        <div className={FORM_CARD_CLASS}>
          <span className="vd-label !text-[9px]">Add action</span>
          <div className="mt-1.5 flex flex-col gap-1.5">
            <input
              type="text"
              value={actionForm.label}
              onChange={(event) => patchAction({ label: event.target.value })}
              placeholder="Label"
              className={FIELD_CLASS}
            />
            <select
              value={actionForm.kind}
              onChange={(event) => patchAction({ kind: event.target.value as ActionRun['kind'] })}
              className={FIELD_CLASS}
            >
              {RUN_KINDS.map((kind) => (
                <option key={kind} value={kind}>
                  {kind}
                </option>
              ))}
            </select>
            {actionForm.kind === 'prompt' && (
              <textarea
                value={actionForm.text}
                onChange={(event) => patchAction({ text: event.target.value })}
                placeholder="Prompt text"
                rows={2}
                className={cn(FIELD_CLASS, 'resize-none leading-[1.45]')}
              />
            )}
            {actionForm.kind === 'command' && (
              <select
                value={actionForm.command}
                onChange={(event) => patchAction({ command: event.target.value as BuiltinCommand })}
                className={FIELD_CLASS}
              >
                {BUILTIN_COMMANDS.map((command) => (
                  <option key={command} value={command}>
                    {command}
                  </option>
                ))}
              </select>
            )}
            {actionForm.kind === 'open-url' && (
              <input
                type="url"
                value={actionForm.url}
                onChange={(event) => patchAction({ url: event.target.value })}
                placeholder="https://example.com"
                className={FIELD_CLASS}
              />
            )}
            {actionForm.kind === 'reveal' && (
              <input
                type="text"
                value={actionForm.path}
                onChange={(event) => patchAction({ path: event.target.value })}
                placeholder="Path to reveal"
                className={FIELD_CLASS}
              />
            )}
            {actionForm.kind === 'mode' &&
              (availableModes.length > 0 ? (
                <select
                  value={actionForm.mode}
                  onChange={(event) => patchAction({ mode: event.target.value })}
                  className={FIELD_CLASS}
                >
                  <option value="">Choose a mode…</option>
                  {availableModes.map((mode) => (
                    <option key={mode.id} value={mode.id}>
                      {mode.name}
                    </option>
                  ))}
                </select>
              ) : (
                <input
                  type="text"
                  value={actionForm.mode}
                  onChange={(event) => patchAction({ mode: event.target.value })}
                  placeholder="Mode id — start a session to pick one"
                  className={FIELD_CLASS}
                />
              ))}
            {actionForm.kind === 'option' && (
              <>
                {configOptions.length > 0 ? (
                  <select
                    value={actionForm.option}
                    onChange={(event) => patchAction({ option: event.target.value, value: '' })}
                    className={FIELD_CLASS}
                  >
                    <option value="">Choose an option…</option>
                    {configOptions.map((option) => (
                      <option key={option.id} value={option.id}>
                        {option.name}
                      </option>
                    ))}
                  </select>
                ) : (
                  <input
                    type="text"
                    value={actionForm.option}
                    onChange={(event) => patchAction({ option: event.target.value })}
                    placeholder="Option id — start a session to pick one"
                    className={FIELD_CLASS}
                  />
                )}
                {(() => {
                  const chosen = configOptions.find((option) => option.id === actionForm.option);
                  if (chosen?.options?.length) {
                    return (
                      <select
                        value={actionForm.value}
                        onChange={(event) => patchAction({ value: event.target.value })}
                        className={FIELD_CLASS}
                      >
                        <option value="">Choose a value…</option>
                        {chosen.options.map((value) => (
                          <option key={value.value} value={value.value}>
                            {value.name}
                          </option>
                        ))}
                      </select>
                    );
                  }
                  return (
                    <input
                      type="text"
                      value={actionForm.value}
                      onChange={(event) => patchAction({ value: event.target.value })}
                      placeholder="Value"
                      className={FIELD_CLASS}
                    />
                  );
                })()}
              </>
            )}
            {actionForm.kind === 'cancel' && (
              <p className="m-0 text-[10.5px] text-[var(--vd-text-tertiary)]">
                Stops the turn the agent is running. Nothing to configure.
              </p>
            )}
            {(actionForm.kind === 'approve' || actionForm.kind === 'reject') && (
              <>
                <Segmented
                  value={actionForm.scope}
                  options={SCOPES}
                  onChange={(scope) => patchAction({ scope })}
                />
                <p className="m-0 text-[10.5px] leading-snug text-[var(--vd-text-tertiary)]">
                  Answers the permission request the agent is paused on. The option is chosen by
                  kind, so it works with any agent. Nothing waiting is a no-op, not an error.
                </p>
              </>
            )}
            {actionForm.kind === 'answer' && (
              <>
                <Segmented
                  value={actionForm.answer}
                  options={ANSWERS}
                  onChange={(answer) => patchAction({ answer })}
                />
                {actionForm.answer === 'accept' && (
                  <textarea
                    value={actionForm.content}
                    onChange={(event) => patchAction({ content: event.target.value })}
                    placeholder='Form values as JSON, e.g. {"region":"eu"}'
                    rows={2}
                    className={cn(FIELD_CLASS, 'resize-none font-mono leading-[1.45]')}
                  />
                )}
                <p className="m-0 text-[10.5px] leading-snug text-[var(--vd-text-tertiary)]">
                  Answers the elicitation form the agent is paused on. `accept` submits the values
                  above; `decline` and `cancel` carry none.
                </p>
              </>
            )}
            <button
              type="button"
              onClick={() => void addAction()}
              disabled={!actionForm.label.trim()}
              className={ADD_BUTTON_CLASS}
            >
              <Plus size={11} strokeWidth={2.2} />
              Add
            </button>
          </div>
        </div>

        {actionsError && <WarnBlock>{actionsError}</WarnBlock>}
        <div className="mt-2">
          <ResetButton label="Reset actions" onClick={() => void resetActions()} />
        </div>
      </Section>

      <Section label="Panels">
        {panels.length === 0 ? (
          <Empty>No custom panels yet. Add one below to open it from the rail.</Empty>
        ) : (
          <div className="flex flex-col gap-1">
            {panels.map((panel) => (
              <PanelRow
                key={panel.id}
                panel={panel}
                onDelete={() => void savePanels(panels.filter((item) => item.id !== panel.id))}
              />
            ))}
          </div>
        )}

        {pluginPanels.length > 0 && (
          <p className="m-0 mt-1.5 text-[10.5px] leading-[1.5] text-[var(--vd-text-tertiary)]">
            From plugins, read-only here:{' '}
            {pluginPanels.map((panel) => `${panel.title} (${panel.plugin ?? 'plugin'})`).join(', ')}
            . Disable the plugin to remove one.
          </p>
        )}

        {pluginPanelIssues.length > 0 && (
          <div className="mt-1.5">
            <span className="vd-label !text-[9px]">Plugin panels not loaded</span>
            <ul className="m-0 mt-1 list-none space-y-1 p-0">
              {pluginPanelIssues.map((issue) => (
                <li
                  key={issue}
                  className="flex items-start gap-1.5 text-[10.5px] leading-[1.4] text-[var(--vd-warn)]"
                >
                  <AlertTriangle size={10} strokeWidth={2} className="mt-0.5 shrink-0" />
                  <span className="min-w-0 break-words">{issue}</span>
                </li>
              ))}
            </ul>
          </div>
        )}

        <div className={FORM_CARD_CLASS}>
          <span className="vd-label !text-[9px]">Add panel</span>
          <div className="mt-1.5 flex flex-col gap-1.5">
            <input
              type="text"
              value={panelForm.title}
              onChange={(event) =>
                setPanelForm((current) => ({ ...current, title: event.target.value }))
              }
              placeholder="Title"
              className={FIELD_CLASS}
            />
            <div className="flex items-center gap-1.5">
              <input
                type="number"
                min={240}
                max={520}
                value={panelForm.width}
                onChange={(event) =>
                  setPanelForm((current) => ({
                    ...current,
                    width: clamp(Number(event.target.value) || 240, 240, 520),
                  }))
                }
                className={cn(FIELD_CLASS, 'w-[74px]')}
              />
              <span className="text-[10px] text-[var(--vd-text-tertiary)]">px wide</span>
            </div>
            <button
              type="button"
              onClick={() => void addPanel()}
              disabled={!panelForm.title.trim()}
              className={ADD_BUTTON_CLASS}
            >
              <Plus size={11} strokeWidth={2.2} />
              Add
            </button>
          </div>
        </div>

        {panelsError && <WarnBlock>{panelsError}</WarnBlock>}
        <div className="mt-2">
          <ResetButton label="Reset panels" onClick={() => void resetPanels()} />
        </div>
      </Section>

      <Section label="Sources">
        <p className="m-0 text-[10.5px] leading-[1.5] text-[var(--vd-text-tertiary)]">
          A plugin ships a config in its own <span className="font-mono">vibedesk/</span> folder.
          Installing it lists the config here; it never applies itself.
        </p>

        {pluginSources.length === 0 ? (
          <Empty>No plugin ships a VibeDesk config.</Empty>
        ) : (
          <div className="flex flex-col gap-1">
            {pluginSources.map((source) => (
              <PluginSourceRow
                key={`${source.plugin}:${source.kind}`}
                source={source}
                busy={sourceBusy === `${source.plugin}:${source.kind}`}
                disabled={sourceBusy !== null}
                onApply={() => void applySource(source)}
              />
            ))}
          </div>
        )}

        <div className="mt-2 flex items-center gap-1.5">
          <ResetButton label="Rescan plugins" onClick={() => void refreshSources()} />
          <span className="text-[10px] text-[var(--vd-text-tertiary)]">
            {pending.proposals.length === 0
              ? 'no agent proposals waiting'
              : `${pending.proposals.length} agent ${pending.proposals.length === 1 ? 'proposal' : 'proposals'} above the composer`}
          </span>
        </div>
      </Section>

      <Section label="Storage">
        <div className="flex flex-col gap-1">
          <StorageLine label="config" value={paths?.configDir} />
          <StorageLine label="theme" value={paths?.themePath} />
          <StorageLine label="layout" value={paths?.layoutPath} />
          <StorageLine label="actions" value={paths?.actionsPath} />
          <StorageLine label="panels" value={paths?.panelsPath} />
        </div>
        <p className="m-0 mt-1.5 text-[10.5px] text-[var(--vd-text-tertiary)]">
          Safe mode {paths?.safeMode ? 'on' : 'off'}
        </p>
        {paths?.safeMode && (
          <WarnBlock>
            Customization is disabled by VIBEDESK_SAFE_MODE=1. The editors in this panel will not
            take effect.
          </WarnBlock>
        )}
      </Section>
    </Drawer>
  );
}

function TokenRow({
  name,
  value,
  onChange,
  onCommit,
}: {
  name: string;
  value: string;
  onChange: (value: string) => void;
  onCommit: () => void;
}) {
  return (
    <div className="flex items-center gap-1.5">
      <span className="min-w-0 flex-1 truncate font-mono text-[10.5px] text-[var(--vd-text-secondary)]">
        {name}
      </span>
      <input
        type="color"
        value={toHex(value)}
        onChange={(event) => onChange(event.target.value)}
        onBlur={onCommit}
        aria-label={`${name} colour`}
        className="h-6 w-8 shrink-0 cursor-pointer rounded-[var(--radius-sm)] border border-[var(--vd-line)] bg-transparent p-0"
      />
      <input
        type="text"
        value={value}
        onChange={(event) => onChange(event.target.value)}
        onBlur={onCommit}
        spellCheck={false}
        aria-label={`${name} value`}
        onKeyDown={(event) => {
          if (event.key === 'Enter') {
            event.preventDefault();
            onCommit();
          }
        }}
        className="h-6 w-[84px] shrink-0 rounded-[var(--radius-sm)] border border-[var(--vd-line)] bg-[var(--vd-surface-sunken)] px-1.5 font-mono text-[10.5px] text-[var(--vd-text)] outline-none duration-[var(--vd-fast)] focus:border-[var(--vd-line-strong)]"
      />
    </div>
  );
}

function NumberRow({
  label,
  value,
  min,
  max,
  onChange,
}: {
  label: string;
  value: number;
  min: number;
  max: number;
  onChange: (value: number) => void;
}) {
  // The slider tracks the pointer locally and commits once, on release.
  // Committing on every `input` event wrote layout.json dozens of times per
  // drag, and two writes in flight could land out of order — leaving the file
  // at a width the user had already dragged past.
  const [draft, setDraft] = useState(value);

  useEffect(() => {
    setDraft(value);
  }, [value]);

  const commit = () => {
    if (draft !== value) onChange(draft);
  };

  return (
    <div>
      <div className="flex items-baseline gap-2">
        <span className="text-[11px] text-[var(--vd-text-secondary)]">{label}</span>
        <span className="vd-numeric ml-auto font-mono text-[10.5px] text-[var(--vd-text-tertiary)]">
          {draft}px
        </span>
      </div>
      <input
        type="range"
        min={min}
        max={max}
        value={draft}
        onChange={(event) => setDraft(Number(event.target.value))}
        onPointerUp={commit}
        onKeyUp={commit}
        onBlur={commit}
        aria-label={label}
        className="mt-1 w-full cursor-pointer accent-[var(--vd-accent)]"
      />
    </div>
  );
}

function Segmented<T extends string>({
  value,
  options,
  onChange,
  className,
}: {
  value: T;
  options: { value: T; label: string }[];
  onChange: (value: T) => void;
  className?: string;
}) {
  return (
    <div
      className={cn(
        'flex items-center gap-0.5 rounded-[var(--radius-sm)] border border-[var(--vd-line)] bg-[var(--vd-surface-sunken)] p-0.5',
        className,
      )}
    >
      {options.map((option) => {
        const active = option.value === value;
        return (
          <button
            key={option.value}
            type="button"
            onClick={() => onChange(option.value)}
            aria-pressed={active}
            className={cn(
              'flex-1 rounded-[var(--radius-xs)] px-1.5 py-1 text-[10.5px] font-medium duration-[var(--vd-fast)]',
              active
                ? 'bg-[var(--vd-surface)] text-[var(--vd-text)] shadow-[var(--vd-shadow-card)]'
                : 'text-[var(--vd-text-tertiary)] hover:text-[var(--vd-text-secondary)]',
            )}
          >
            {option.label}
          </button>
        );
      })}
    </div>
  );
}

function ActionRow({ action, onDelete }: { action: CustomAction; onDelete: () => void }) {
  return (
    <div className="flex items-center gap-1.5 rounded-[var(--radius-sm)] border border-[var(--vd-line-soft)] bg-[var(--vd-surface)] px-2 py-1">
      <span className="min-w-0 flex-1 truncate text-[11.5px] text-[var(--vd-text)]">
        {action.label}
      </span>
      <span className="vd-chip !h-[17px] !text-[9px]">{action.run.kind}</span>
      <button
        type="button"
        onClick={onDelete}
        className="vd-icon-btn !h-[22px] !w-[22px]"
        title={`Delete ${action.label}`}
      >
        <X size={11} strokeWidth={2.2} />
      </button>
    </div>
  );
}

function PanelRow({ panel, onDelete }: { panel: CustomPanel; onDelete: () => void }) {
  // A v2 panel's body is a layout tree; `blocks` is its document-order
  // flattening, so counting blocks is still the honest number of things the
  // panel draws. The chip says "layout" when the tree is what the file holds,
  // because "0 blocks" over a panel full of tabs reads as broken.
  const count = panel.blocks.length;
  const unit = panel.layout ? 'layout' : count === 1 ? 'block' : 'blocks';
  return (
    <div className="flex items-center gap-1.5 rounded-[var(--radius-sm)] border border-[var(--vd-line-soft)] bg-[var(--vd-surface)] px-2 py-1">
      <span className="min-w-0 flex-1 truncate text-[11.5px] text-[var(--vd-text)]">
        {panel.title}
      </span>
      <span className="vd-chip !h-[17px] !text-[9px]">
        {count} {unit}
      </span>
      <button
        type="button"
        onClick={onDelete}
        className="vd-icon-btn !h-[22px] !w-[22px]"
        title={`Delete ${panel.title}`}
      >
        <X size={11} strokeWidth={2.2} />
      </button>
    </div>
  );
}

function ResetButton({ label, onClick }: { label: string; onClick: () => void }) {
  return (
    <button
      type="button"
      onClick={onClick}
      className="vd-btn vd-btn-ghost !h-[26px] !px-2 !text-[11px]"
    >
      <RotateCcw size={11} strokeWidth={2} />
      {label}
    </button>
  );
}

function WarnBlock({ children }: { children: ReactNode }) {
  return (
    <div className="mt-2 flex items-start gap-1.5 rounded-[var(--radius-sm)] border border-[color-mix(in_oklab,var(--vd-warn)_32%,transparent)] bg-[var(--vd-warn-wash)] px-2 py-1.5 text-[10.5px] leading-[1.4] text-[var(--vd-warn)]">
      <AlertTriangle size={10} strokeWidth={2} className="mt-0.5 shrink-0" />
      <span className="min-w-0 break-words">{children}</span>
    </div>
  );
}

function PluginSourceRow({
  source,
  busy,
  disabled,
  onApply,
}: {
  source: PluginSource;
  busy: boolean;
  disabled: boolean;
  onApply: () => void;
}) {
  return (
    <div className="rounded-[var(--radius-sm)] border border-[var(--vd-line)] px-2 py-1.5">
      <div className="flex items-center gap-1.5">
        <span className="min-w-0 flex-1 truncate text-[11.5px] font-medium text-[var(--vd-text)]">
          {source.plugin}
        </span>
        <span className="shrink-0 rounded-[var(--radius-xs)] border border-[var(--vd-line)] px-1 py-px text-[9px] uppercase tracking-[0.06em] text-[var(--vd-text-tertiary)]">
          {source.scope}
        </span>
        <span className="shrink-0 text-[10px] text-[var(--vd-text-tertiary)]">{source.kind}</span>
      </div>
      <div className="mt-0.5 flex items-center gap-1.5">
        <span className="min-w-0 flex-1 truncate text-[10.5px] text-[var(--vd-text-secondary)]">
          {source.valid ? source.summary : 'refused'}
        </span>
        <button
          type="button"
          onClick={onApply}
          disabled={disabled || !source.valid}
          className={cn('vd-btn !h-[22px] !px-1.5 !text-[10px]', busy && 'opacity-70')}
        >
          Apply
        </button>
      </div>
      {!source.valid && source.error && (
        <p className="m-0 mt-1 break-words font-mono text-[10px] leading-[1.4] text-[var(--vd-warn)]">
          {source.error}
        </p>
      )}
    </div>
  );
}

function StorageLine({ label, value }: { label: string; value?: string }) {
  return (
    <div className="flex items-baseline gap-2">
      <span className="w-[52px] shrink-0 text-[10px] text-[var(--vd-text-tertiary)]">{label}</span>
      <span
        className="min-w-0 flex-1 truncate font-mono text-[10px] text-[var(--vd-text-secondary)]"
        title={value}
      >
        {value ?? 'not reported'}
      </span>
    </div>
  );
}
