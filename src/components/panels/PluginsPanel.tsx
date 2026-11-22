import { useMemo, useState } from 'react';
import {
  AlertTriangle,
  BookOpen,
  Bot,
  ChevronRight,
  FolderOpen,
  Plug,
  RefreshCw,
  Server,
  Sparkles,
  Webhook,
} from 'lucide-react';
import type { PluginComponent, PluginRecord } from '@/types/acp';
import { cn } from '@/lib/utils';
import { plugins as pluginsApi } from '@/lib/bridge';
import { errorMessage, refreshPanels, reloadPlugins } from '@/lib/actions';
import { useApp } from '@/store/app';
import { Drawer, Empty, Section } from '@/components/chrome/Drawer';

const COMPONENT_ICON: Record<PluginComponent['kind'], typeof Sparkles> = {
  skill: Sparkles,
  mcp: Server,
  hook: Webhook,
  agent: Bot,
  knowledge: BookOpen,
  connector: Plug,
  library: FolderOpen,
};

const COMPONENT_LABEL: Record<PluginComponent['kind'], string> = {
  skill: 'Skills',
  mcp: 'MCP servers',
  hook: 'Hooks',
  agent: 'Agents',
  knowledge: 'Knowledge',
  connector: 'Connectors',
  library: 'Libraries',
};

export function PluginsPanel() {
  const plugins = useApp((s) => s.plugins);
  const setPlugins = useApp((s) => s.setPlugins);
  const environment = useApp((s) => s.environment);
  const notify = useApp((s) => s.notify);
  const addNotice = useApp((s) => s.addNotice);
  const [busy, setBusy] = useState(false);

  const grouped = useMemo(() => {
    const user = plugins.filter((p) => p.scope === 'user');
    const project = plugins.filter((p) => p.scope === 'project');
    return { user, project };
  }, [plugins]);

  async function reload() {
    setBusy(true);
    try {
      await reloadPlugins();
    } finally {
      setBusy(false);
    }
  }

  async function toggle(plugin: PluginRecord) {
    try {
      const next = await pluginsApi.setEnabled(plugin.name, !plugin.enabled);
      setPlugins(next);
      // A plugin's panels leave the rail with it, so the list is re-read rather
      // than left to the next launch.
      await refreshPanels();
      notify(`${plugin.name} ${plugin.enabled ? 'disabled' : 'enabled'}`, 'success');
    } catch (error) {
      addNotice(errorMessage(error), 'error');
    }
  }

  return (
    <Drawer
      title="Plugins"
      subtitle={`${plugins.length} installed · ${plugins.filter((p) => p.enabled).length} active`}
      actions={
        <button
          type="button"
          onClick={reload}
          disabled={busy}
          className="vd-btn vd-btn-ghost !h-[24px] !px-1.5"
          title="Re-discover plugins and re-pin the session"
        >
          <RefreshCw size={12} strokeWidth={2} className={cn(busy && 'vd-spin')} />
        </button>
      }
    >
      {plugins.length === 0 ? (
        <div className="p-3">
          <Empty>
            No plugins found. Drop an Agent Plugins 1.0 package into{' '}
            <span className="font-mono">~/.vibe/plugins/</span> or{' '}
            <span className="font-mono">./.vibe/plugins/</span>, then reload.
          </Empty>
        </div>
      ) : (
        <>
          {grouped.project.length > 0 && (
            <Section label="Project scope">
              <div className="flex flex-col gap-1.5">
                {grouped.project.map((plugin) => (
                  <PluginCard key={plugin.path} plugin={plugin} onToggle={toggle} />
                ))}
              </div>
            </Section>
          )}
          {grouped.user.length > 0 && (
            <Section label="User scope">
              <div className="flex flex-col gap-1.5">
                {grouped.user.map((plugin) => (
                  <PluginCard key={plugin.path} plugin={plugin} onToggle={toggle} />
                ))}
              </div>
            </Section>
          )}
        </>
      )}

      <Section label="Search paths">
        <div className="flex flex-col gap-1">
          {(environment?.pluginsDirs ?? []).map((dir) => (
            <span
              key={dir}
              className="truncate font-mono text-[10.5px] text-[var(--vd-text-tertiary)]"
            >
              {dir}
            </span>
          ))}
          {environment?.pluginsDirs.length === 0 && (
            <span className="text-[11px] text-[var(--vd-text-tertiary)]">
              Paths appear once the agent reports its environment.
            </span>
          )}
        </div>
      </Section>
    </Drawer>
  );
}

function PluginCard({
  plugin,
  onToggle,
}: {
  plugin: PluginRecord;
  onToggle: (plugin: PluginRecord) => void;
}) {
  const [open, setOpen] = useState(false);
  const counts = useMemo(() => {
    const map = new Map<PluginComponent['kind'], PluginComponent[]>();
    for (const component of plugin.components) {
      const list = map.get(component.kind) ?? [];
      list.push(component);
      map.set(component.kind, list);
    }
    return map;
  }, [plugin.components]);

  return (
    <div
      className={cn('vd-card overflow-hidden transition-opacity', !plugin.enabled && 'opacity-60')}
    >
      <div className="flex items-start gap-2 px-2.5 py-2">
        <button
          type="button"
          onClick={() => setOpen((v) => !v)}
          className="min-w-0 flex-1 text-left"
          aria-expanded={open}
        >
          <span className="flex items-center gap-1.5">
            <ChevronRight
              size={11}
              strokeWidth={2.2}
              className={cn(
                'shrink-0 text-[var(--vd-text-tertiary)] transition-transform',
                open && 'rotate-90',
              )}
            />
            <span className="truncate text-[12px] font-medium text-[var(--vd-text)]">
              {plugin.displayName ?? plugin.name}
            </span>
            {plugin.version && (
              <span className="vd-chip !h-[17px] !text-[9px]">v{plugin.version}</span>
            )}
            {plugin.format !== 'native' && (
              <span className="vd-chip vd-chip-warn !h-[17px] !text-[9px]">{plugin.format}</span>
            )}
          </span>
          {plugin.description && (
            <span className="mt-0.5 line-clamp-2 block pl-[18px] text-[11px] leading-[1.45] text-[var(--vd-text-tertiary)]">
              {plugin.description}
            </span>
          )}
        </button>

        <button
          type="button"
          onClick={() => onToggle(plugin)}
          className="vd-switch mt-1 shrink-0"
          data-on={plugin.enabled}
          title={plugin.enabled ? 'Disable plugin' : 'Enable plugin'}
          aria-pressed={plugin.enabled}
        />
      </div>

      {open && (
        <div className="border-t border-[var(--vd-line-soft)] px-2.5 py-2">
          {plugin.components.length === 0 ? (
            <p className="m-0 text-[11px] text-[var(--vd-text-tertiary)]">
              This package declares no components Vibe can load.
            </p>
          ) : (
            <div className="flex flex-col gap-2">
              {[...counts.entries()].map(([kind, list]) => {
                const Icon = COMPONENT_ICON[kind];
                return (
                  <div key={kind}>
                    <div className="flex items-center gap-1.5">
                      <Icon size={10} strokeWidth={2} className="text-[var(--vd-text-tertiary)]" />
                      <span className="vd-label !text-[9px]">
                        {COMPONENT_LABEL[kind]} · {list.length}
                      </span>
                    </div>
                    <div className="mt-1 flex flex-wrap gap-1">
                      {list.map((component) => (
                        <span
                          key={`${kind}-${component.name}`}
                          className="vd-chip !h-[18px] !text-[9.5px]"
                          title={component.detail ?? undefined}
                        >
                          {component.name}
                        </span>
                      ))}
                    </div>
                  </div>
                );
              })}
            </div>
          )}

          <div className="mt-2 flex flex-col gap-0.5 border-t border-[var(--vd-line-soft)] pt-1.5">
            <span className="truncate font-mono text-[10px] text-[var(--vd-text-tertiary)]">
              {plugin.path}
            </span>
            <span className="flex items-center gap-2 text-[10px] text-[var(--vd-text-tertiary)]">
              <span>{plugin.scope} scope</span>
              {plugin.toolNamespace && <span>ns {plugin.toolNamespace}</span>}
              {plugin.digest && <span className="font-mono">{plugin.digest.slice(0, 10)}</span>}
            </span>
          </div>

          {plugin.issues.length > 0 && (
            <ul className="m-0 mt-1.5 list-none space-y-1 p-0">
              {plugin.issues.map((issue) => (
                <li
                  key={issue}
                  className="flex items-start gap-1.5 text-[10.5px] leading-[1.4] text-[var(--vd-warn)]"
                >
                  <AlertTriangle size={10} strokeWidth={2} className="mt-0.5 shrink-0" />
                  {issue}
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
    </div>
  );
}
