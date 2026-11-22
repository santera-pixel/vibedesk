import { useEffect, useMemo, useRef, useState } from 'react';
import { AlertTriangle, Check, Eye, EyeOff, FileCog, SendHorizontal, X } from 'lucide-react';
import type { Change, ConfigKind, Density, Motion, Proposal } from '@/lib/customize';
import { applyLayout, applyTheme, defaultLayout } from '@/lib/customize';
import { cn } from '@/lib/utils';
import { sources } from '@/lib/bridge';
import { useApp } from '@/store/app';
import { errorMessage, sendPrompt } from '@/lib/actions';

const KIND_LABEL: Record<ConfigKind, string> = {
  theme: 'Theme',
  layout: 'Layout',
  actions: 'Action buttons',
  panels: 'Panels',
};

const KIND_FILE: Record<ConfigKind, string> = {
  theme: 'theme.json',
  layout: 'layout.json',
  actions: 'actions.json',
  panels: 'panels.json',
};

/** Diff rows shown before the list folds. Four fits without pushing the transcript. */
const SHOWN_CHANGES = 4;

/**
 * The agent's proposals, waiting for a decision.
 *
 * This is the second gate. The agent's own file-write permission is about
 * touching the filesystem; this one is about changing the app's own chrome.
 * Both must pass, which is why a proposal can sit here while the agent is
 * perfectly free to have written the file.
 *
 * `data-vd-protected` is not decoration: a theme that could hide this bar could
 * approve itself. The stylesheet refuses to target it, and so does Rust.
 *
 * The bar shows the *diff*, not a count. "7 tokens" is not a decision anyone can
 * make; `--vd-accent #ff7a1a → #ff7414` is. And for the two surfaces that are
 * pure CSS variables, the diff can be worn for a moment before it is committed.
 */
export function ProposalBar() {
  const pending = useApp((s) => s.pending);
  const setPending = useApp((s) => s.setPending);
  const addNotice = useApp((s) => s.addNotice);
  const [busy, setBusy] = useState<string | null>(null);
  const [showIssues, setShowIssues] = useState(false);
  const [expanded, setExpanded] = useState<ConfigKind | null>(null);
  const [previewing, setPreviewing] = useState<ConfigKind | null>(null);
  const previewRef = useRef<ConfigKind | null>(null);

  const proposals = pending.proposals;
  const issues = pending.issues;
  const receipts = pending.receipts ?? [];

  /**
   * The value each changed field has *right now*, for the rows where the backend
   * could not supply one.
   *
   * A theme merges over the built-in stylesheet, so a token the proposal adds has
   * no "before" in any file — the before is whatever `globals.css` resolves to on
   * screen. Only the document knows that, and only until a preview repaints it,
   * which is why this is memoised on `proposals` and not recomputed on render: a
   * preview must not rewrite the column it is being compared against.
   */
  const liveValues = useMemo(() => {
    const values = new Map<string, string>();
    const style = getComputedStyle(document.documentElement);
    for (const proposal of proposals) {
      for (const change of proposal.changes ?? []) {
        if (change.from) continue;
        const value = style.getPropertyValue(change.field).trim();
        if (value) values.set(change.field, value);
      }
    }
    return values;
  }, [proposals]);

  /**
   * Put the live theme and layout back. Called on every exit path — toggle,
   * apply, discard, unmount — because a preview that outlives its proposal is
   * the app showing a config that does not exist.
   */
  function revert() {
    if (!previewRef.current) return;
    const state = useApp.getState();
    applyTheme(state.themeConfig);
    applyLayout(state.layoutConfig);
    previewRef.current = null;
    setPreviewing(null);
  }

  // A preview must not survive the bar. Unmounting happens when the last
  // proposal is decided, which is exactly when the previewed config stops being
  // a proposal and becomes either the live config or nothing.
  useEffect(
    () => () => {
      if (!previewRef.current) return;
      const state = useApp.getState();
      applyTheme(state.themeConfig);
      applyLayout(state.layoutConfig);
    },
    [],
  );

  if (proposals.length === 0 && issues.length === 0) return null;

  /**
   * Re-read rather than patch locally: the decision writes a receipt, and the
   * bar shows the user exactly what the agent is about to read. Guessing at the
   * new state here would drift from the file on disk.
   */
  async function refresh() {
    try {
      setPending(await sources.pending());
    } catch {
      // The watcher will deliver the same state on its next tick.
    }
  }

  /** Only the two surfaces that are pure CSS variables can be worn. */
  function canPreview(kind: ConfigKind): boolean {
    return kind === 'theme' || kind === 'layout';
  }

  function startPreview(proposal: Proposal) {
    const state = useApp.getState();
    if (proposal.kind === 'theme') {
      const base = state.themeConfig;
      const tokens = { ...(base?.tokens ?? {}) };
      for (const change of proposal.changes) {
        if (!change.field.startsWith('--vd-')) continue;
        if (change.to) tokens[change.field] = change.to;
        else delete tokens[change.field];
      }
      applyTheme({ tokens, rules: base?.rules ?? [] });
    } else if (proposal.kind === 'layout') {
      const base = state.layoutConfig ?? defaultLayout();
      const next = { ...base };
      for (const change of proposal.changes) {
        const value = change.to;
        switch (change.field) {
          case 'railWidth':
            next.railWidth = Number(value) || base.railWidth;
            break;
          case 'sidebarWidth':
            next.sidebarWidth = Number(value) || base.sidebarWidth;
            break;
          case 'sidebarSide':
            next.sidebarSide = value === 'left' ? 'left' : 'right';
            break;
          case 'sidebarCollapsed':
            next.sidebarCollapsed = value === 'true';
            break;
          case 'transcriptMax':
            next.transcriptMax = Number(value) || base.transcriptMax;
            break;
          case 'composerMax':
            next.composerMax = Number(value) || base.composerMax;
            break;
          case 'density':
            next.density = (value as Density) || base.density;
            break;
          case 'motion':
            next.motion = (value as Motion) || base.motion;
            break;
        }
      }
      applyLayout(next);
    }
    previewRef.current = proposal.kind;
    setPreviewing(proposal.kind);
  }

  function togglePreview(proposal: Proposal) {
    if (previewing === proposal.kind) {
      revert();
      return;
    }
    revert();
    startPreview(proposal);
  }

  async function apply(proposal: Proposal) {
    setBusy(`apply:${proposal.kind}`);
    revert();
    try {
      await sources.apply(proposal.kind, proposal.digest);
      await refresh();
      addNotice(`Applied the agent's ${KIND_LABEL[proposal.kind].toLowerCase()}.`, 'success');
    } catch (error) {
      // A digest mismatch lands here: the file moved after it was shown.
      addNotice(errorMessage(error), 'error');
    } finally {
      setBusy(null);
    }
  }

  /**
   * Apply, then hand the receipt back to the agent in the same gesture.
   *
   * The receipt file is the agent's only feedback channel, but it has to *read*
   * it, which normally costs a turn the user has to ask for. Sending it as a
   * follow-up prompt closes the loop in one click: the agent sees what landed
   * and can iterate without the user retyping "did that work?".
   *
   * The turn is not awaited. `sendPrompt` owns its own errors and its own busy
   * flag, and holding this button disabled for the length of a model turn would
   * make the bar look stuck.
   */
  async function applyAndTell(proposal: Proposal) {
    setBusy(`tell:${proposal.kind}`);
    revert();
    try {
      await sources.apply(proposal.kind, proposal.digest);
      const next = await sources.pending();
      setPending(next);
      const receipt = next.receipts?.find((entry) => entry.kind === proposal.kind);
      const body = receipt
        ? JSON.stringify({
            applied: receipt.applied,
            summary: receipt.summary,
            error: receipt.error,
          })
        : '{"applied":true}';
      const label = KIND_LABEL[proposal.kind].toLowerCase();
      void sendPrompt(`I applied the ${label} change. Receipt: ${body}. Read it and continue.`);
    } catch (error) {
      addNotice(errorMessage(error), 'error');
    } finally {
      setBusy(null);
    }
  }

  async function reject(proposal: Proposal) {
    setBusy(`reject:${proposal.kind}`);
    revert();
    try {
      await sources.reject(proposal.kind);
      await refresh();
    } catch (error) {
      addNotice(errorMessage(error), 'error');
    } finally {
      setBusy(null);
    }
  }

  return (
    <div
      data-vd-protected="approval"
      className="vd-enter mx-auto mb-2 w-full max-w-[var(--vd-composer-max)] px-6"
      // Escape leaves the preview. Scoped to the bar and to a preview being
      // active, so it can never throw away a proposal by accident.
      onKeyDown={(event) => {
        if (event.key === 'Escape' && previewing) {
          event.stopPropagation();
          revert();
        }
      }}
    >
      <div className="max-h-[46vh] overflow-y-auto rounded-[var(--radius-md)] border border-[color-mix(in_oklab,var(--vd-accent)_34%,transparent)] bg-[var(--vd-accent-wash)]">
        <div className="flex items-center gap-2 px-3 py-2">
          <FileCog size={13} strokeWidth={2} className="shrink-0 text-[var(--vd-accent)]" />
          <p className="m-0 text-[12.5px] font-medium text-[var(--vd-text)]">
            {proposals.length === 0
              ? 'The agent proposed something VibeDesk refused'
              : proposals.length === 1
                ? 'The agent proposed a change'
                : `The agent proposed ${proposals.length} changes`}
          </p>
          <span className="ml-auto text-[10.5px] text-[var(--vd-text-tertiary)]">
            {previewing ? 'previewing — nothing is written' : 'nothing is applied until you say so'}
          </span>
        </div>

        <ul className="m-0 list-none border-t border-[color-mix(in_oklab,var(--vd-accent)_22%,transparent)] p-0">
          {proposals.map((proposal) => {
            const applying = busy === `apply:${proposal.kind}`;
            const telling = busy === `tell:${proposal.kind}`;
            const rejecting = busy === `reject:${proposal.kind}`;
            const changes = proposal.changes ?? [];
            const open = expanded === proposal.kind;
            const shown = open ? changes : changes.slice(0, SHOWN_CHANGES);
            const hidden = changes.length - shown.length;
            const preview = previewing === proposal.kind;

            return (
              <li
                key={proposal.kind}
                className={cn(
                  'border-b border-[color-mix(in_oklab,var(--vd-accent)_14%,transparent)] px-3 py-2 last:border-b-0',
                  preview && 'bg-[color-mix(in_oklab,var(--vd-accent)_8%,transparent)]',
                )}
              >
                <div className="flex items-baseline gap-2">
                  <span className="shrink-0 text-[11px] font-medium uppercase tracking-[0.06em] text-[var(--vd-text-tertiary)]">
                    {KIND_LABEL[proposal.kind]}
                  </span>
                  <span className="min-w-0 flex-1 truncate text-[12px] text-[var(--vd-text-secondary)]">
                    {proposal.summary}
                  </span>
                  <span className="shrink-0 font-mono text-[10px] text-[var(--vd-text-tertiary)]">
                    {KIND_FILE[proposal.kind]}
                  </span>
                </div>

                {changes.length > 0 && (
                  <div className="mt-1.5 overflow-hidden rounded-[var(--radius-xs)] border border-[color-mix(in_oklab,var(--vd-accent)_16%,transparent)]">
                    <table className="w-full border-collapse">
                      <tbody>
                        {shown.map((change) => (
                          <DiffRow
                            key={change.field}
                            change={change}
                            from={change.from || liveValues.get(change.field) || ''}
                          />
                        ))}
                      </tbody>
                    </table>
                    {hidden > 0 && (
                      <button
                        type="button"
                        onClick={() => setExpanded(proposal.kind)}
                        className="w-full border-t border-[color-mix(in_oklab,var(--vd-accent)_14%,transparent)] bg-transparent px-2 py-1 text-left text-[10.5px] text-[var(--vd-text-tertiary)] hover:text-[var(--vd-text-secondary)]"
                      >
                        show {hidden} more
                      </button>
                    )}
                    {open && changes.length > SHOWN_CHANGES && (
                      <button
                        type="button"
                        onClick={() => setExpanded(null)}
                        className="w-full border-t border-[color-mix(in_oklab,var(--vd-accent)_14%,transparent)] bg-transparent px-2 py-1 text-left text-[10.5px] text-[var(--vd-text-tertiary)] hover:text-[var(--vd-text-secondary)]"
                      >
                        show less
                      </button>
                    )}
                  </div>
                )}

                <div className="mt-1.5 flex flex-wrap items-center justify-end gap-1.5">
                  {canPreview(proposal.kind) && changes.length > 0 && (
                    <button
                      type="button"
                      onClick={() => togglePreview(proposal)}
                      disabled={busy !== null}
                      className={cn(
                        'vd-btn vd-btn-ghost !h-[26px] shrink-0',
                        preview && 'text-[var(--vd-accent)]',
                      )}
                      title={
                        preview
                          ? 'Stop previewing and put the live config back (Esc)'
                          : 'Wear this change without writing it'
                      }
                    >
                      {preview ? (
                        <EyeOff size={11} strokeWidth={2.4} />
                      ) : (
                        <Eye size={11} strokeWidth={2.4} />
                      )}
                      {preview ? 'Stop preview' : 'Preview'}
                    </button>
                  )}
                  <button
                    type="button"
                    onClick={() => void apply(proposal)}
                    disabled={busy !== null}
                    className={cn(
                      'vd-btn vd-btn-primary !h-[26px] shrink-0',
                      applying && 'opacity-70',
                    )}
                  >
                    <Check size={11} strokeWidth={2.6} />
                    Apply
                  </button>
                  <button
                    type="button"
                    onClick={() => void applyAndTell(proposal)}
                    disabled={busy !== null}
                    className={cn(
                      'vd-btn vd-btn-ghost !h-[26px] shrink-0',
                      telling && 'opacity-70',
                    )}
                    title="Apply, then send the receipt back to the agent so it can continue"
                  >
                    <SendHorizontal size={11} strokeWidth={2.4} />
                    Apply + tell
                  </button>
                  <button
                    type="button"
                    onClick={() => void reject(proposal)}
                    disabled={busy !== null}
                    className={cn('vd-btn !h-[26px] shrink-0', rejecting && 'opacity-70')}
                  >
                    <X size={11} strokeWidth={2.6} />
                    Discard
                  </button>
                </div>
              </li>
            );
          })}
        </ul>

        {issues.length > 0 && (
          <div className="border-t border-[color-mix(in_oklab,var(--vd-warn)_26%,transparent)] bg-[var(--vd-warn-wash)] px-3 py-2">
            <button
              type="button"
              onClick={() => setShowIssues((v) => !v)}
              className="flex w-full items-center gap-1.5 border-0 bg-transparent p-0 text-left text-[11.5px] font-medium text-[var(--vd-warn)]"
            >
              <AlertTriangle size={11} strokeWidth={2.2} />
              {issues.length} {issues.length === 1 ? 'proposal was' : 'proposals were'} refused
              <span className="ml-auto text-[10.5px] font-normal text-[var(--vd-text-tertiary)]">
                {showIssues ? 'hide' : 'why'}
              </span>
            </button>
            {showIssues && (
              <ul className="m-0 mt-1.5 list-none space-y-1 p-0">
                {issues.map((issue, index) => (
                  <li
                    key={index}
                    className="font-mono text-[10.5px] leading-relaxed text-[var(--vd-text-secondary)]"
                  >
                    {issue}
                  </li>
                ))}
              </ul>
            )}
          </div>
        )}

        {receipts.length > 0 && (
          <div className="border-t border-[color-mix(in_oklab,var(--vd-accent)_14%,transparent)] px-3 py-1.5">
            <p className="m-0 text-[10.5px] leading-relaxed text-[var(--vd-text-tertiary)]">
              The agent has been told:{' '}
              {receipts.map((receipt, index) => (
                <span key={receipt.kind}>
                  {index > 0 && <span className="opacity-50"> · </span>}
                  <span className="text-[var(--vd-text-secondary)]">
                    {KIND_LABEL[receipt.kind].toLowerCase()}
                  </span>{' '}
                  {receipt.applied ? 'applied' : 'refused'}
                  {receipt.applied && receipt.summary ? ` (${receipt.summary})` : ''}
                </span>
              ))}
            </p>
          </div>
        )}
      </div>
    </div>
  );
}

/**
 * One field, before and after.
 *
 * A removed field shows an em dash rather than an empty cell, so "this token
 * goes away" cannot be misread as "this token becomes nothing". `from` is passed
 * in because the caller may have had to resolve it from the live document.
 */
function DiffRow({ change, from }: { change: Change; from: string }) {
  const added = !from;
  const removed = !change.to;
  return (
    <tr className="border-b border-[color-mix(in_oklab,var(--vd-accent)_10%,transparent)] last:border-b-0">
      <td className="w-[38%] max-w-0 truncate px-2 py-[3px] font-mono text-[10.5px] text-[var(--vd-text-secondary)]">
        <span title={change.field}>{change.field}</span>
      </td>
      <td className="w-[31%] max-w-0 truncate px-1 py-[3px] font-mono text-[10.5px] text-[var(--vd-text-tertiary)] line-through decoration-[color-mix(in_oklab,var(--vd-text-tertiary)_50%,transparent)]">
        <span title={from}>{added ? '—' : from}</span>
      </td>
      <td className="w-[31%] max-w-0 truncate px-2 py-[3px] font-mono text-[10.5px] text-[var(--vd-accent)]">
        <span title={change.to}>{removed ? '—' : change.to}</span>
      </td>
    </tr>
  );
}
