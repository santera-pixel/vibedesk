import { ChevronRight } from 'lucide-react';
import { cn, formatCost, formatTokens } from '@/lib/utils';
import { SETTINGS_GROUPS, SETTINGS_SECTIONS, type SettingsSectionId } from '@/lib/settings';
import { stagger } from '@/lib/motion';
import { useApp } from '@/store/app';
import { Drawer } from '@/components/chrome/Drawer';

/**
 * The settings directory.
 *
 * A menu of eleven rows is only worth having if the rows say something. Each
 * one carries the live number that would make you open it — the model in force,
 * how much of the window is full, how many chats are archived, whether an
 * update is waiting — so the directory doubles as a status readout and the
 * common case ("is anything wrong?") is answered without opening anything.
 *
 * A row with nothing to report shows no badge rather than a zero. An empty
 * badge is noise; a missing one is information.
 */
/**
 * The position of each section in the whole directory, not in its group. The
 * cascade has to read as one list, so a section in the third group starts where
 * the second group left off.
 */
const ORDER = new Map(SETTINGS_SECTIONS.map((section, index) => [section.id, index]));

export function SettingsPanel() {
  const setPanel = useApp((s) => s.setPanel);
  const status = useApp((s) => s.status);
  const usage = useApp((s) => s.usage);
  const account = useApp((s) => s.account);
  const plugins = useApp((s) => s.plugins);
  const chats = useApp((s) => s.chats);
  const update = useApp((s) => s.update);
  const pending = useApp((s) => s.pending);
  const actionsConfig = useApp((s) => s.actionsConfig);
  const panelsConfig = useApp((s) => s.panelsConfig);
  const configOptions = useApp((s) => s.configOptions);

  const proposals = pending.proposals.length;

  function badge(id: SettingsSectionId): string | null {
    switch (id) {
      case 'config':
        return (
          status?.model ?? (configOptions.length > 0 ? `${configOptions.length} options` : null)
        );
      case 'plugins':
        return plugins.length > 0 ? `${plugins.length} installed` : null;
      case 'context':
        return usage && usage.size > 0
          ? `${Math.round((usage.used / usage.size) * 100)}% of ${formatTokens(usage.size)}`
          : null;
      case 'customize': {
        const actions = actionsConfig?.actions.length ?? 0;
        const panels = panelsConfig?.panels.length ?? 0;
        if (proposals > 0) return `${proposals} awaiting approval`;
        if (actions === 0 && panels === 0) return null;
        const plural = (n: number, one: string) => `${n} ${one}${n === 1 ? '' : 's'}`;
        return `${plural(actions, 'action')} · ${plural(panels, 'panel')}`;
      }
      case 'sessions':
        return chats.length > 0 ? `${chats.length} chats` : null;
      case 'billing':
        return usage?.cost ? formatCost(usage.cost.amount, usage.cost.currency) : null;
      case 'account':
        return account?.plan.name ?? account?.plan.title ?? null;
      case 'status':
        return status?.state ?? null;
      case 'release-notes':
        return update?.updateAvailable
          ? `v${update.latestVersion ?? '?'} available`
          : (update?.currentVersion ?? null);
      default:
        return null;
    }
  }

  return (
    <Drawer title="Settings" subtitle={`${SETTINGS_SECTIONS.length} sections`}>
      <div className="py-1">
        {SETTINGS_GROUPS.map((group) => (
          <section key={group.label} className="px-2 py-2">
            <h3 className="vd-label m-0 px-1 pb-1">{group.label}</h3>
            <div className="vd-stagger flex flex-col">
              {group.sections.map(({ id, label, hint, icon: Icon }) => {
                const value = badge(id);
                return (
                  <button
                    key={id}
                    type="button"
                    onClick={() => setPanel(id)}
                    style={stagger(ORDER.get(id) ?? 0)}
                    className={cn(
                      'vd-enter group flex w-full items-center gap-2.5 rounded-[var(--radius-sm)] px-2 py-1.5 text-left ',
                      'hover:bg-[var(--vd-surface-hover)]',
                    )}
                  >
                    <Icon
                      size={14}
                      strokeWidth={1.9}
                      className="shrink-0 text-[var(--vd-text-tertiary)] group-hover:text-[var(--vd-text)]"
                    />
                    <span className="min-w-0 flex-1">
                      <span className="block truncate text-[12px] text-[var(--vd-text)]">
                        {label}
                      </span>
                      <span className="block truncate text-[10.5px] text-[var(--vd-text-tertiary)]">
                        {hint}
                      </span>
                    </span>
                    {value && (
                      <span className="shrink-0 font-mono text-[10px] text-[var(--vd-text-secondary)]">
                        {value}
                      </span>
                    )}
                    <ChevronRight
                      size={13}
                      strokeWidth={2}
                      className="shrink-0 text-[var(--vd-text-tertiary)] opacity-0 transition-opacity group-hover:opacity-100"
                    />
                  </button>
                );
              })}
            </div>
          </section>
        ))}
      </div>
    </Drawer>
  );
}
