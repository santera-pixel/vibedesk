import { useCallback, useEffect, useRef, useState } from 'react';
import {
  AlertTriangle,
  ArrowRight,
  Check,
  Circle,
  Loader2,
  MinusCircle,
  RefreshCw,
} from 'lucide-react';
import type { ProvisionRow, ProvisionStatus } from '@/types/acp';
import { provision as provisionApi, setup as setupApi } from '@/lib/bridge';
import { errorMessage } from '@/lib/actions';
import { useApp } from '@/store/app';

/**
 * The two steps without which the app does not work at all.
 *
 * `cli` is the agent itself. `swarm-agents` is the payload that gives the lead
 * and the workers their own prompts — without it the lead runs on Vibe's coding
 * prompt and implements the goal instead of planning it, which looks like a
 * working swarm and is not one.
 *
 * The rest are reported but do not hold the app: a machine with no network
 * cannot clone the method pack, and refusing to open the window over that would
 * be a worse failure than the missing pack.
 */
const REQUIRED = ['cli', 'swarm-agents'];

/** Whether the setup screen still has work to do. */
export function needsProvisioning(provision: ProvisionStatus | null): boolean {
  if (!provision) return false;
  return provision.rows.some(
    (row) => REQUIRED.includes(row.id) && row.state !== 'ready' && row.state !== 'skipped',
  );
}

/**
 * First-run screen: everything a fresh machine needs, in one list.
 *
 * VibeDesk deliberately does not bundle the CLI — it wraps whatever the user has
 * installed, so the agent always matches their `~/.vibe` config. That makes
 * "not installed" a state the app has to handle rather than a crash, and it is
 * the first of seven steps rather than the only one.
 */
export function SetupScreen({ onReady }: { onReady: () => void }) {
  const setup = useApp((s) => s.setup);
  const setSetup = useApp((s) => s.setSetup);
  const setProvision = useApp((s) => s.setProvision);

  const [status, setStatus] = useState<ProvisionStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [retrying, setRetrying] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const mounted = useRef(true);

  const refresh = useCallback(async () => {
    try {
      const next = await provisionApi.status();
      if (!mounted.current) return;
      setStatus(next);
      setProvision(next);
    } catch (cause) {
      if (mounted.current) setError(errorMessage(cause));
    }
  }, [setProvision]);

  useEffect(() => {
    mounted.current = true;
    void refresh();
    return () => {
      mounted.current = false;
    };
  }, [refresh]);

  // While a run is in flight the ledger is the only progress signal, so poll it.
  // `provision_run` resolves once every step has finished; without this the
  // screen would sit on one spinner for the whole install.
  useEffect(() => {
    if (!busy) return;
    const timer = window.setInterval(() => void refresh(), 700);
    return () => window.clearInterval(timer);
  }, [busy, refresh]);

  async function runAll() {
    setBusy(true);
    setError(null);
    try {
      await provisionApi.run();
      await refresh();
      const next = await setupApi.status().catch(() => null);
      if (next) setSetup(next);
    } catch (cause) {
      setError(errorMessage(cause));
    } finally {
      setBusy(false);
    }
  }

  async function retry(id: string) {
    setRetrying(id);
    setError(null);
    try {
      await provisionApi.runStep(id);
      await refresh();
    } catch (cause) {
      setError(errorMessage(cause));
    } finally {
      setRetrying(null);
    }
  }

  const rows = status?.rows ?? [];
  const ready = rows.filter((row) => row.state === 'ready').length;
  const blocked = needsProvisioning(status);
  const cliMissing = setup ? !setup.binaryFound : false;

  return (
    <div className="vd-enter flex flex-1 items-center justify-center overflow-y-auto p-8">
      <div className="w-full max-w-[62ch]">
        <div className="mb-1 flex items-center gap-2">
          <span className="vd-label">first run</span>
          <span className="h-px flex-1 bg-[var(--vd-line-soft)]" />
          <span className="vd-meta font-mono !text-[11px]">
            {rows.length > 0 ? `${ready} of ${rows.length}` : 'checking'}
          </span>
        </div>

        <h1 className="vd-display mt-5 text-[2.1rem] leading-[1.15] text-[var(--vd-text)]">
          {cliMissing ? 'Set up this machine.' : 'Finish setting up.'}
        </h1>
        <p className="mt-2.5 text-[var(--body)] leading-[1.6] text-[var(--vd-text-secondary)]">
          VibeDesk drives the official Mistral Vibe agent rather than shipping its own copy, so the
          agent always matches your <span className="font-mono text-[0.9em]">~/.vibe</span> config,
          plugins and keys. These are the pieces a swarm run needs on top of it.
        </p>

        <ul className="mt-6 divide-y divide-[var(--vd-line-soft)] border-y border-[var(--vd-line-soft)]">
          {rows.map((row) => (
            <Step
              key={row.id}
              row={row}
              busy={busy || retrying === row.id}
              onRetry={() => void retry(row.id)}
            />
          ))}
          {rows.length === 0 && (
            <li className="flex items-center gap-2 py-3 text-[12px] text-[var(--vd-text-tertiary)]">
              <Loader2 size={12} strokeWidth={2.2} className="vd-spin" />
              Reading what this machine already has…
            </li>
          )}
        </ul>

        {status?.note && (
          <p className="mt-2 text-[10.5px] leading-[1.5] text-[var(--vd-text-tertiary)]">
            {status.note}
          </p>
        )}

        {error && (
          <div className="mt-4 rounded-[var(--radius-md)] border border-[color-mix(in_oklab,var(--vd-danger)_34%,transparent)] bg-[var(--vd-danger-wash)] p-3">
            <div className="flex items-center gap-2">
              <AlertTriangle size={13} strokeWidth={2} className="text-[var(--vd-danger)]" />
              <span className="text-[12px] font-medium text-[var(--vd-text)]">
                That step didn&rsquo;t finish
              </span>
            </div>
            <pre className="m-0 mt-2 max-h-40 overflow-auto whitespace-pre-wrap break-words rounded-[var(--radius-xs)] bg-[var(--vd-surface-sunken)] p-2 font-mono text-[10.5px] leading-[1.5] text-[var(--vd-text-secondary)]">
              {error}
            </pre>
          </div>
        )}

        <div className="mt-5 flex items-center gap-2">
          {blocked ? (
            <button
              type="button"
              onClick={() => void runAll()}
              disabled={busy}
              className="vd-btn vd-btn-primary !h-[30px]"
            >
              {busy ? (
                <Loader2 size={12} strokeWidth={2.2} className="vd-spin" />
              ) : (
                <ArrowRight size={12} strokeWidth={2.2} />
              )}
              {busy ? 'Setting up…' : ready > 0 ? 'Finish setup' : 'Set up'}
            </button>
          ) : (
            <button type="button" onClick={onReady} className="vd-btn vd-btn-primary !h-[30px]">
              Start the agent
              <ArrowRight size={12} strokeWidth={2.2} />
            </button>
          )}
          <button
            type="button"
            onClick={() => void refresh()}
            disabled={busy}
            className="vd-btn vd-btn-ghost !h-[30px]"
          >
            <RefreshCw size={11} strokeWidth={2} />
            Check again
          </button>
          {busy && <span className="vd-meta ml-1">this can take a few minutes</span>}
        </div>

        <p className="mt-6 border-t border-[var(--vd-line-soft)] pt-3 text-[10.5px] leading-[1.55] text-[var(--vd-text-tertiary)]">
          Already installed somewhere unusual? Point{' '}
          <span className="font-mono">VIBEDESK_ACP_BIN</span> at the{' '}
          <span className="font-mono">vibe-acp</span> binary and restart.
        </p>
      </div>
    </div>
  );
}

function Step({
  row,
  busy,
  onRetry,
}: {
  row: ProvisionRow;
  busy: boolean;
  onRetry: () => void;
}) {
  return (
    <li className="flex items-start gap-2.5 py-2.5">
      <span className="mt-[3px] shrink-0">
        <StateIcon state={row.state} busy={busy} />
      </span>
      <div className="min-w-0 flex-1">
        <div className="flex items-baseline gap-2">
          <span className="text-[12.5px] font-medium text-[var(--vd-text)]">{row.title}</span>
          {row.version && (
            <span className="font-mono text-[10px] text-[var(--vd-text-tertiary)]">
              {row.version}
            </span>
          )}
        </div>
        <p className="m-0 mt-0.5 break-words text-[11px] leading-[1.5] text-[var(--vd-text-secondary)]">
          {row.blocked ?? row.detail}
        </p>
      </div>
      {row.retryable && (
        <button
          type="button"
          onClick={onRetry}
          disabled={busy}
          className="vd-btn vd-btn-ghost mt-0.5 !h-[24px] shrink-0"
        >
          {busy ? (
            <Loader2 size={10} strokeWidth={2.2} className="vd-spin" />
          ) : (
            <RefreshCw size={10} strokeWidth={2} />
          )}
          Retry
        </button>
      )}
    </li>
  );
}

function StateIcon({ state, busy }: { state: ProvisionRow['state']; busy: boolean }) {
  if (busy || state === 'installing') {
    return <Loader2 size={13} strokeWidth={2.2} className="vd-spin text-[var(--vd-accent)]" />;
  }
  switch (state) {
    case 'ready':
      return <Check size={13} strokeWidth={2.6} className="text-[var(--vd-accent)]" />;
    case 'failed':
      return <AlertTriangle size={13} strokeWidth={2} className="text-[var(--vd-danger)]" />;
    case 'skipped':
      return <MinusCircle size={13} strokeWidth={2} className="text-[var(--vd-text-tertiary)]" />;
    default:
      return <Circle size={13} strokeWidth={2} className="text-[var(--vd-text-tertiary)]" />;
  }
}
