import { MessageSquare, PanelRightClose, PanelRightOpen, Settings2 } from 'lucide-react';
import { cn } from '@/lib/utils';
import { customize } from '@/lib/bridge';
import { applyLayout, defaultLayout } from '@/lib/customize';
import { isSettingsSection } from '@/lib/settings';
import { useApp } from '@/store/app';
import { ACTION_ICONS } from '@/components/chrome/ActionStrip';

/**
 * The rail is navigation, not a directory of features.
 *
 * It used to carry eight icons — Plugins, Configuration, Sessions, Context,
 * Billing, Skills, Customize, Help — all the same size and weight, so nothing
 * in it read as more important than anything else and the list grew every time
 * a surface was added. Those eight are all settings, so they are one entry now
 * and `SettingsPanel` is the directory. What stays in the rail is what is not
 * settings: the conversation, and the panels the user authored.
 */
export function Rail() {
  const panel = useApp((s) => s.panel);
  const setPanel = useApp((s) => s.setPanel);
  const layout = useApp((s) => s.layoutConfig);
  const setLayoutConfig = useApp((s) => s.setLayoutConfig);
  const panelsConfig = useApp((s) => s.panelsConfig);
  const customPanel = useApp((s) => s.customPanel);
  const setCustomPanel = useApp((s) => s.setCustomPanel);
  const proposals = useApp((s) => s.pending.proposals.length);

  const collapsed = layout?.sidebarCollapsed ?? false;
  const customPanels = panelsConfig?.panels ?? [];

  // A section is inside settings, so the button stays lit while one is open.
  // Without this the rail looks like nothing is selected the moment you pick a
  // section, which reads as a bug rather than as depth.
  const settingsActive = panel === 'settings' || isSettingsSection(panel);

  async function toggleSidebar() {
    const base = layout ?? defaultLayout();
    const next = { ...base, sidebarCollapsed: !base.sidebarCollapsed };
    try {
      const saved = await customize.setLayout(next);
      setLayoutConfig(saved);
      applyLayout(saved);
    } catch {
      /* the rail keeps its current state if the write fails */
    }
  }

  return (
    <nav className="flex w-[var(--vd-rail-w)] shrink-0 flex-col items-center gap-1 border-r border-[var(--vd-line)] bg-[var(--vd-canvas-raised)] py-2">
      <button
        type="button"
        onClick={() => setPanel(null)}
        className={cn(
          'grid size-8 place-items-center rounded-[var(--radius-sm)] ',
          // A custom panel is a panel too: without the second half of this the
          // Conversation button reads as active while a plugin's panel is open.
          panel === null && customPanel === null
            ? 'bg-[var(--vd-surface-hover)] text-[var(--vd-text)]'
            : 'text-[var(--vd-text-tertiary)] hover:bg-[var(--vd-surface-hover)] hover:text-[var(--vd-text)]',
        )}
        title="Conversation"
        aria-pressed={panel === null && customPanel === null}
      >
        <MessageSquare size={15} strokeWidth={1.9} />
      </button>

      {customPanels.length > 0 && <span className="my-1 h-px w-5 bg-[var(--vd-line)]" />}

      {customPanels.map((custom) => {
        const Icon = custom.icon ? ACTION_ICONS[custom.icon] : MessageSquare;
        const active = customPanel === custom.id;
        return (
          <button
            key={custom.id}
            type="button"
            onClick={() => setCustomPanel(active ? null : custom.id)}
            className={cn(
              'relative grid size-8 place-items-center rounded-[var(--radius-sm)] ',
              active
                ? 'bg-[var(--vd-surface-hover)] text-[var(--vd-text)]'
                : 'text-[var(--vd-text-tertiary)] hover:bg-[var(--vd-surface-hover)] hover:text-[var(--vd-text)]',
            )}
            title={
              custom.plugin ? `${custom.title} — from the ${custom.plugin} plugin` : custom.title
            }
            aria-pressed={active}
          >
            <Icon size={15} strokeWidth={1.9} />
            {custom.plugin && (
              <span
                className="absolute bottom-[3px] right-[3px] h-[3px] w-[3px] rounded-full bg-[var(--vd-text-tertiary)]"
                aria-hidden
              />
            )}
            {active && (
              <span className="absolute -left-2 top-1/2 h-4 w-[2px] -translate-y-1/2 rounded-full bg-[var(--vd-accent)]" />
            )}
          </button>
        );
      })}

      <button
        type="button"
        onClick={() => setPanel(panel === 'settings' ? null : 'settings')}
        className={cn(
          'relative mt-auto grid size-8 place-items-center rounded-[var(--radius-sm)] ',
          settingsActive
            ? 'bg-[var(--vd-surface-hover)] text-[var(--vd-text)]'
            : 'text-[var(--vd-text-tertiary)] hover:bg-[var(--vd-surface-hover)] hover:text-[var(--vd-text)]',
        )}
        title="Settings"
        aria-pressed={settingsActive}
      >
        <Settings2 size={15} strokeWidth={1.9} />
        {proposals > 0 && (
          <span
            className="absolute right-[5px] top-[5px] h-[4px] w-[4px] rounded-full bg-[var(--vd-accent)]"
            aria-hidden
          />
        )}
        {settingsActive && (
          <span className="absolute -left-2 top-1/2 h-4 w-[2px] -translate-y-1/2 rounded-full bg-[var(--vd-accent)]" />
        )}
      </button>

      <button
        type="button"
        onClick={() => void toggleSidebar()}
        className="grid size-8 place-items-center rounded-[var(--radius-sm)] text-[var(--vd-text-tertiary)] hover:bg-[var(--vd-surface-hover)] hover:text-[var(--vd-text)]"
        title={collapsed ? 'Show the workstation rail' : 'Hide the workstation rail'}
        aria-pressed={collapsed}
      >
        {collapsed ? (
          <PanelRightOpen size={15} strokeWidth={1.9} />
        ) : (
          <PanelRightClose size={15} strokeWidth={1.9} />
        )}
      </button>
    </nav>
  );
}
