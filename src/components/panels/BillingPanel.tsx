import { useCallback, useEffect, useState } from 'react';
import { CircleAlert, CreditCard, RefreshCw, ShieldCheck, Wallet } from 'lucide-react';
import { cn, formatCost, formatTokens } from '@/lib/utils';
import { vibe } from '@/lib/bridge';
import { errorMessage } from '@/lib/actions';
import { useApp } from '@/store/app';
import { Drawer, Empty, KeyValue, Section } from '@/components/chrome/Drawer';

/**
 * The billing window.
 *
 * Two ledgers, deliberately kept apart because they answer different questions:
 *
 * - **Subscription** is the plan's allowance — a percentage the server owns,
 *   with a reset date. VibeDesk never derives it, because only the server knows
 *   the size of the pool.
 * - **API usage** is the token ledger for the open session, from
 *   `usage_update._meta`: prompt, completion and cached tokens, plus the cost
 *   the agent reports.
 *
 * The plan half is fetched on open and on demand; the token half is live from
 * the store, so it updates as a turn streams without another round trip.
 */
export function BillingPanel() {
  const account = useApp((s) => s.account);
  const loading = useApp((s) => s.accountLoading);
  const usage = useApp((s) => s.usage);
  const environment = useApp((s) => s.environment);
  const status = useApp((s) => s.status);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    const state = useApp.getState();
    state.setAccountLoading(true);
    try {
      const next = await vibe.account();
      state.setAccount(next);
      setError(null);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      state.setAccountLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const plan = account?.plan;
  const identity = account?.identity;
  const meta = usage?.meta ?? null;
  const pct = plan?.usagePercentage ?? null;
  const ratio = pct === null ? 0 : Math.min(1, pct / 100);

  const prompt = meta?.promptTokens ?? 0;
  const cached = meta?.cachedTokens ?? 0;
  const hitRate = prompt > 0 ? cached / prompt : 0;

  return (
    <Drawer
      title="Billing"
      subtitle={plan?.title ? `${plan.title} plan` : 'Plan and token usage'}
      actions={
        <button
          type="button"
          onClick={() => void refresh()}
          disabled={loading}
          className="vd-btn vd-btn-ghost !h-[22px] !px-1.5"
          title="Ask the agent for the current plan and allowance"
        >
          <RefreshCw size={11} strokeWidth={2} className={cn(loading && 'vd-spin')} />
        </button>
      }
    >
      {error && (
        <Section label="Unavailable">
          <p className="m-0 flex items-start gap-1.5 text-[11px] leading-[1.5] text-[var(--vd-danger)]">
            <CircleAlert size={11} strokeWidth={2} className="mt-0.5 shrink-0" />
            {error}
          </p>
        </Section>
      )}

      <Section label="Subscription">
        {plan?.title || plan?.name ? (
          <>
            <div className="flex items-center gap-2">
              <span className="grid size-7 shrink-0 place-items-center rounded-[var(--radius-sm)] border border-[color-mix(in_oklab,var(--vd-accent)_28%,transparent)] bg-[var(--vd-accent-wash)]">
                <CreditCard size={13} strokeWidth={2} className="text-[var(--vd-accent)]" />
              </span>
              <div className="min-w-0">
                <p className="m-0 truncate text-[12.5px] font-semibold text-[var(--vd-text)]">
                  {plan.title ?? 'Plan'}
                </p>
                <p className="m-0 truncate text-[10.5px] text-[var(--vd-text-tertiary)]">
                  {[plan.name, plan.kind].filter(Boolean).join(' · ') || 'no tier reported'}
                </p>
              </div>
              {plan.status && (
                <span
                  className={cn(
                    'vd-chip ml-auto shrink-0 !h-[17px] !text-[8.5px]',
                    plan.status === 'ready' ? 'vd-chip-accent' : 'vd-chip-warn',
                  )}
                >
                  {plan.status}
                </span>
              )}
            </div>

            <div className="mt-3">
              <div className="flex items-baseline justify-between">
                <span className="vd-label !text-[9px]">allowance used</span>
                <span className="vd-numeric text-[11px] text-[var(--vd-text)]">
                  {pct === null ? 'not reported' : `${pct.toFixed(1)}%`}
                </span>
              </div>
              <div className="mt-1 flex h-2 overflow-hidden rounded-full bg-[var(--vd-surface-sunken)]">
                <span
                  className="h-full w-full origin-left bg-[var(--vd-accent)] transition-transform duration-[var(--vd-slow)] ease-[var(--vd-ease)]"
                  style={{ transform: `scaleX(${ratio})` }}
                />
              </div>
              <div className="mt-1.5 flex items-baseline justify-between">
                <span className="text-[10.5px] text-[var(--vd-text-tertiary)]">
                  {plan.resetAt ? `resets ${untilTime(plan.resetAt)}` : 'no reset date'}
                </span>
                <span className="text-[10.5px] text-[var(--vd-text-tertiary)]">
                  {plan.paygEnabled ? 'pay-as-you-go on' : 'pay-as-you-go off'}
                </span>
              </div>
            </div>

            <dl className="mt-3 grid grid-cols-2 gap-2">
              <Tile label="remaining" value={pct === null ? '—' : `${(100 - pct).toFixed(1)}%`} />
              <Tile label="reset" value={plan.resetAt ? shortDate(plan.resetAt) : '—'} />
            </dl>
          </>
        ) : (
          <Empty>
            {loading ? 'Reading the plan…' : 'The agent did not report a plan for this session.'}
          </Empty>
        )}
      </Section>

      <Section label="API usage">
        {meta ? (
          <>
            <dl className="grid grid-cols-2 gap-2">
              <Tile label="prompt" value={formatTokens(prompt)} />
              <Tile label="completion" value={formatTokens(meta.completionTokens ?? 0)} />
              <Tile label="cached" value={formatTokens(cached)} />
              <Tile label="total" value={formatTokens(meta.totalTokens ?? 0)} />
            </dl>

            <div className="mt-3">
              <div className="flex items-baseline justify-between">
                <span className="vd-label !text-[9px]">cache hit rate</span>
                <span className="vd-numeric text-[11px] text-[var(--vd-text)]">
                  {prompt > 0 ? `${Math.round(hitRate * 100)}%` : '—'}
                </span>
              </div>
              <div className="mt-1 flex h-1.5 overflow-hidden rounded-full bg-[var(--vd-surface-sunken)]">
                <span
                  className="h-full w-full origin-left bg-[var(--vd-info)] transition-transform duration-[var(--vd-slow)] ease-[var(--vd-ease)]"
                  style={{ transform: `scaleX(${hitRate})` }}
                />
              </div>
              <p className="m-0 mt-1.5 text-[10.5px] leading-[1.5] text-[var(--vd-text-tertiary)]">
                {prompt > 0
                  ? `${formatTokens(cached)} of ${formatTokens(prompt)} prompt tokens were served from the provider's cache.`
                  : 'No prompt tokens reported yet.'}
              </p>
            </div>

            <div className="mt-3">
              <KeyValue label="cost" value={usage?.cost ? formatCost(usage.cost.amount, usage.cost.currency) : 'not reported'} />
              <KeyValue label="steps" value={String(meta.steps ?? 0)} />
              <KeyValue
                label="last turn"
                value={
                  meta.lastTurnTotalTokens
                    ? `${formatTokens(meta.lastTurnTotalTokens)} tokens`
                    : '—'
                }
              />
              <KeyValue
                label="speed"
                value={meta.tokensPerSecond ? `${meta.tokensPerSecond.toFixed(1)} tok/s` : '—'}
              />
            </div>
          </>
        ) : (
          <Empty>Token counts appear after the first model call in this chat.</Empty>
        )}
      </Section>

      <Section label="Credentials">
        <KeyValue label="api key" value={plan?.keyPreview ?? 'not reported'} />
        <KeyValue label="scope" value={plan?.keyScope ?? '—'} />
        <KeyValue label="access" value={plan?.keyAccess ?? '—'} />
        <KeyValue
          label="auth"
          value={
            identity?.authenticated
              ? (identity.authState ?? 'authenticated')
              : 'not authenticated'
          }
        />
        <KeyValue label="model" value={environment?.activeModel ?? 'default'} />
        <p className="m-0 mt-1.5 flex items-start gap-1.5 text-[10.5px] leading-[1.5] text-[var(--vd-text-tertiary)]">
          <ShieldCheck size={11} strokeWidth={2} className="mt-0.5 shrink-0 text-[var(--vd-info)]" />
          The key is read from the agent and shown masked. VibeDesk never stores it, and the
          full value never crosses the bridge.
        </p>
      </Section>

      <Section label="Account">
        {identity?.email || identity?.workspace ? (
          <>
            <KeyValue label="email" value={identity.email ?? '—'} />
            <KeyValue label="name" value={identity.name ?? '—'} />
            <KeyValue label="workspace" value={identity.workspace ?? '—'} />
            <KeyValue label="organization" value={identity.organization ?? '—'} />
            <KeyValue label="session" value={status?.sessionId ?? '—'} />
          </>
        ) : (
          <Empty>{loading ? 'Reading identity…' : 'No identity reported.'}</Empty>
        )}
      </Section>

      <Section label="Notes">
        <p className="m-0 flex items-start gap-1.5 text-[11px] leading-[1.5] text-[var(--vd-text-tertiary)]">
          <Wallet size={11} strokeWidth={2} className="mt-0.5 shrink-0" />
          The allowance is the plan's own figure, reported by the agent. Token counts cover this
          chat only — a new chat starts a fresh ledger, while the allowance is shared across all
          of them.
        </p>
      </Section>
    </Drawer>
  );
}

function Tile({ label, value }: { label: string; value: string }) {
  return (
    <div className="rounded-[var(--radius-sm)] border border-[var(--vd-line-soft)] px-2 py-1.5">
      <dt className="vd-label !text-[9px]">{label}</dt>
      <dd className="m-0 mt-0.5 truncate font-mono text-[11px] text-[var(--vd-text-secondary)]">
        {value}
      </dd>
    </div>
  );
}

/** `2026-12-01T00:00:00Z` reads as `1 Dec 2026` — a date, not a countdown. */
function shortDate(iso: string): string {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return iso;
  return date.toLocaleDateString(undefined, { day: 'numeric', month: 'short', year: 'numeric' });
}

/**
 * A future timestamp as a countdown.
 *
 * `relativeTime` measures backwards, so a reset date — always in the future —
 * came out as "just now". This counts forwards instead.
 */
function untilTime(iso: string): string {
  const then = new Date(iso).getTime();
  if (Number.isNaN(then)) return 'unknown';
  const delta = then - Date.now();
  if (delta <= 0) return 'due now';
  const mins = Math.round(delta / 60_000);
  if (mins < 60) return `in ${mins}m`;
  const hours = Math.round(mins / 60);
  if (hours < 48) return `in ${hours}h`;
  return `in ${Math.round(hours / 24)} days`;
}
