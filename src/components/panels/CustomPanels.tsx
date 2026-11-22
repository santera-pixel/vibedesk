import { X } from 'lucide-react';
import { useCallback } from 'react';
import { visible } from '@/lib/data';
import { panelSources } from '@/lib/customize';
import { decideConsent } from '@/lib/actions';
import { useApp } from '@/store/app';
import { usePanelData, usePanelModel } from '@/store/panelData';
import { ACTION_ICONS } from '@/components/chrome/ActionStrip';
import { useEscapeToClose } from '@/components/chrome/Drawer';
import { BlockView } from '@/components/panels/BlockView';
import { LayoutView } from '@/components/panels/LayoutView';

export function CustomPanels() {
  const panelsConfig = useApp((s) => s.panelsConfig);
  const customPanel = useApp((s) => s.customPanel);

  const panel = panelsConfig?.panels.find((entry) => entry.id === customPanel) ?? null;
  if (!panel) return null;

  // Keyed on the panel id so switching panels drops the previous panel's
  // subscription state instead of briefly rendering the old model.
  return <PanelBody key={panel.id} panelId={panel.id} />;
}

function PanelBody({ panelId }: { panelId: string }) {
  const panel = useApp((s) => s.panelsConfig?.panels.find((entry) => entry.id === panelId) ?? null);
  const setCustomPanel = useApp((s) => s.setCustomPanel);
  const points = usePanelData((s) => s.points[panelId]);
  const paused = usePanelData((s) => s.paused);
  const plane = usePanelData((s) => s.status);

  // Hooks are called before the null check so the hook order is stable across a
  // panel disappearing from the config while it is open.
  const model = usePanelModel(panel);
  useEscapeToClose(useCallback(() => setCustomPanel(null), [setCustomPanel]));

  if (!panel) return null;

  const Icon = panel.icon ? ACTION_ICONS[panel.icon] : null;
  const sources = panelSources(panel);
  const external = sources.filter((source) => source.type !== 'builtin');
  const failed = external.filter((source) => points?.[source.id]?.error).length;
  const answered = external.filter((source) => points?.[source.id]?.value != null).length;

  // What Rust is actually running, which is not the same as what this panel
  // declares. A rejected `panels.json` stops every source while the frontend
  // keeps the last good config, so without this cross-check the header would
  // claim "live" over a value that has stopped moving.
  const running = plane
    ? plane.sources.filter((source) => source.panelId === panelId && source.running).length
    : null;

  // An origin the user has not answered for, or has refused. Grouped by origin
  // because that is the unit of the decision: one answer covers every path on
  // that service and every panel that reads it.
  const waiting = plane
    ? [
        ...new Map(
          plane.sources
            .filter(
              (source) =>
                source.panelId === panelId &&
                (source.state === 'awaiting-consent' || source.state === 'denied'),
            )
            .map((source) => [source.origin, source.state] as const),
        ),
      ].map(([origin, state]) => ({ origin, state }))
    : [];

  const health = describeHealth({
    paused,
    external: external.length,
    failed,
    answered,
    running,
    waiting: waiting.length,
  });

  return (
    <aside
      className="vd-panel-surface vd-enter flex shrink-0 flex-col border-l border-[var(--vd-line)] bg-[var(--vd-canvas-raised)]"
      style={{ width: panel.width }}
    >
      <header className="flex h-11 shrink-0 items-center gap-2 border-b border-[var(--vd-line)] px-3">
        {Icon && (
          <Icon size={13} strokeWidth={2} className="shrink-0 text-[var(--vd-text-tertiary)]" />
        )}
        <h2 className="m-0 min-w-0 flex-1 truncate text-[12.5px] font-semibold text-[var(--vd-text)]">
          {panel.title}
        </h2>
        {health && (
          <span
            className="flex shrink-0 items-center gap-1 text-[10px] text-[var(--vd-text-tertiary)]"
            title={health.detail}
          >
            <span
              className="h-[5px] w-[5px] rounded-full"
              style={{ background: health.color }}
              aria-hidden
            />
            {health.label}
          </span>
        )}
        <button
          type="button"
          onClick={() => setCustomPanel(null)}
          className="vd-icon-btn"
          title="Close panel"
        >
          <X size={14} strokeWidth={2} />
        </button>
      </header>

      <div className="min-h-0 flex-1 overflow-y-auto">
        {waiting.map((entry) => (
          <ConsentBar key={entry.origin} origin={entry.origin} state={entry.state} />
        ))}
        {panel.layout ? (
          // A v2 panel: the tree is the source of truth, and it is rendered
          // whole so a data tick cannot remount a subtree.
          <LayoutView layout={panel.layout} model={model} />
        ) : (
          panel.blocks.map((block, index) => {
            // A guarded block whose condition is not a real `true` is hidden. That
            // is what lets a panel say nothing instead of saying something false.
            if (!visible(block.when, model)) return null;
            return (
              <div
                key={index}
                className="border-b border-[var(--vd-line-soft)] px-3 py-2 last:border-b-0"
              >
                <BlockView block={block} model={model} />
              </div>
            );
          })
        )}
      </div>
    </aside>
  );
}

/**
 * The question a declared source is asking.
 *
 * A panel may only address this machine, but a loopback service is usually
 * unauthenticated *because* it assumes only local processes can reach it — so
 * "this machine" still includes a database admin UI and a dev server with a
 * debug endpoint. The origin is shown in full, because the decision is about
 * that origin and nothing else.
 */
function ConsentBar({ origin, state }: { origin: string; state: string }) {
  const denied = state === 'denied';

  return (
    <div className="border-b border-[var(--vd-line-soft)] bg-[var(--vd-canvas)] px-3 py-2">
      <p className="m-0 text-[11px] leading-[1.45] text-[var(--vd-text-secondary)]">
        {denied ? 'Refused access to ' : 'This panel wants to read '}
        <span className="vd-tabular text-[var(--vd-text)]">{origin}</span>
        {denied ? '.' : ' — a service on this machine.'}
      </p>
      <div className="mt-1.5 flex items-center gap-1.5">
        {denied ? (
          <button
            type="button"
            className="vd-btn text-[11px]"
            onClick={() => void decideConsent(origin, null)}
          >
            Ask again
          </button>
        ) : (
          <>
            <button
              type="button"
              className="vd-btn text-[11px]"
              onClick={() => void decideConsent(origin, 'allow')}
            >
              Allow
            </button>
            <button
              type="button"
              className="vd-btn text-[11px]"
              onClick={() => void decideConsent(origin, 'deny')}
            >
              Deny
            </button>
          </>
        )}
      </div>
    </div>
  );
}

function describeHealth(input: {
  paused: boolean;
  external: number;
  failed: number;
  answered: number;
  /** Sources Rust reports as running for this panel, or null if unknown. */
  running: number | null;
  /** Origins this panel reads that the user has not allowed. */
  waiting: number;
}): { label: string; detail: string; color: string } | null {
  if (input.external === 0) return null;
  if (input.paused) {
    return {
      label: 'paused',
      detail: 'The data plane is paused. Values below are the last ones fetched.',
      color: 'var(--vd-text-faint)',
    };
  }
  // Checked before "stopped": a source waiting on the user is not a config
  // error, and reporting it as one sends them looking for a fault that is not
  // there.
  if (input.waiting > 0) {
    return {
      label: 'needs permission',
      detail: `${input.waiting} origin${input.waiting === 1 ? '' : 's'} waiting on you.`,
      color: 'var(--vd-text-faint)',
    };
  }
  // Checked before "live" because a stopped plane keeps the last value in the
  // store: the number looks current and is not.
  if (input.running === 0) {
    return {
      label: 'stopped',
      detail:
        'The data plane is not running this panel. The config file may have failed to load — see Customize for the reason.',
      color: 'var(--vd-danger)',
    };
  }
  if (input.failed > 0) {
    return {
      label: 'stale',
      detail: `${input.failed} of ${input.external} sources failed their last fetch.`,
      color: 'var(--vd-danger)',
    };
  }
  if (input.answered === 0) {
    return {
      label: 'waiting',
      detail: 'No source has answered yet.',
      color: 'var(--vd-text-faint)',
    };
  }
  return {
    label: 'live',
    detail: `${input.answered} of ${input.external} sources answering.`,
    color: 'var(--vd-accent)',
  };
}

