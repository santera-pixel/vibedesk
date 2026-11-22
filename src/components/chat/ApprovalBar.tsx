import { useState } from 'react';
import { AlertTriangle, Check, ChevronDown, ShieldQuestion, X } from 'lucide-react';
import type { PermissionOption } from '@/types/acp';
import { cn } from '@/lib/utils';
import { interaction } from '@/lib/bridge';
import { useApp } from '@/store/app';
import { errorMessage } from '@/lib/actions';
import { useDisclosure } from '@/lib/disclosure';

export function ApprovalBar() {
  const request = useApp((s) => s.permission);
  const setPermission = useApp((s) => s.setPermission);
  const addNotice = useApp((s) => s.addNotice);
  const [pending, setPending] = useState<string | null>(null);
  const { open: expanded, mounted, armed, toggle } = useDisclosure();

  if (!request) return null;

  const tool = request.toolCall;
  const input = tool.rawInput as Record<string, unknown> | null | undefined;
  const detail =
    input && typeof input === 'object'
      ? ((input.command as string | undefined) ??
        (input.path as string | undefined) ??
        (input.url as string | undefined) ??
        null)
      : null;

  async function respond(option: PermissionOption | null) {
    setPending(option?.optionId ?? 'reject');
    try {
      await interaction.respondPermission(request!.requestId, option?.optionId ?? null);
      setPermission(null);
    } catch (error) {
      addNotice(errorMessage(error), 'error');
    } finally {
      setPending(null);
    }
  }

  const allow = request.options.filter((o) => o.kind.startsWith('allow'));
  const deny = request.options.filter((o) => o.kind.startsWith('reject'));
  // An agent that reports options with kinds this build does not recognise
  // would otherwise leave the bar with no buttons at all — and the agent is
  // paused until it gets an answer, so the session would be stuck with no way
  // out. A plain reject is always a legal answer.
  const stranded = allow.length === 0 && deny.length === 0;

  return (
    <div
      data-vd-protected="approval"
      className="vd-enter mx-auto mb-2 w-full max-w-[var(--vd-composer-max)] px-6"
    >
      <div className="overflow-hidden rounded-[var(--radius-md)] border border-[color-mix(in_oklab,var(--vd-warn)_38%,transparent)] bg-[var(--vd-warn-wash)]">
        <div className="flex items-start gap-2.5 px-3 py-2.5">
          <span className="mt-0.5 grid size-6 shrink-0 place-items-center rounded-[var(--radius-xs)] border border-[color-mix(in_oklab,var(--vd-warn)_34%,transparent)] text-[var(--vd-warn)]">
            <ShieldQuestion size={13} strokeWidth={2} />
          </span>
          <div className="min-w-0 flex-1">
            <p className="m-0 text-[12.5px] font-medium text-[var(--vd-text)]">
              Approval needed · {tool.title ?? 'tool call'}
            </p>
            {detail && (
              <p className="m-0 mt-0.5 truncate font-mono text-[11px] text-[var(--vd-text-secondary)]">
                {detail}
              </p>
            )}
            {tool.locations && tool.locations.length > 0 && (
              <p className="m-0 mt-0.5 truncate font-mono text-[10.5px] text-[var(--vd-text-tertiary)]">
                {tool.locations.map((l) => l.path).join(' · ')}
              </p>
            )}
          </div>
          <button
            type="button"
            onClick={toggle}
            aria-expanded={expanded}
            className="vd-icon-btn !h-6 !w-6"
            title="Show raw request"
          >
            <ChevronDown
              size={13}
              strokeWidth={2}
              className={cn('transition-transform', expanded && 'rotate-180')}
            />
          </button>
        </div>

        {mounted && (
          <div className="vd-expand" data-open={expanded && armed}>
            <pre className="vd-code m-0 max-h-40 overflow-auto border-t border-[color-mix(in_oklab,var(--vd-warn)_24%,transparent)] px-3 py-2 text-[11px] text-[var(--vd-text-secondary)]">
              {JSON.stringify(tool.rawInput ?? {}, null, 2)}
            </pre>
          </div>
        )}

        <div className="flex flex-wrap items-center gap-1.5 border-t border-[color-mix(in_oklab,var(--vd-warn)_24%,transparent)] px-3 py-2">
          {allow.map((option, index) => (
            <button
              key={option.optionId}
              type="button"
              onClick={() => void respond(option)}
              disabled={pending !== null}
              className={cn('vd-btn !h-[26px]', index === 0 && 'vd-btn-primary')}
            >
              {pending === option.optionId ? (
                <Check size={11} strokeWidth={2.6} />
              ) : (
                <Check size={11} strokeWidth={2.4} />
              )}
              {option.name}
            </button>
          ))}
          {deny.map((option) => (
            <button
              key={option.optionId}
              type="button"
              onClick={() => void respond(option)}
              disabled={pending !== null}
              className="vd-btn vd-btn-danger !h-[26px]"
            >
              <X size={11} strokeWidth={2.6} />
              {option.name}
            </button>
          ))}
          {stranded && (
            <button
              type="button"
              onClick={() => void respond(null)}
              disabled={pending !== null}
              className="vd-btn vd-btn-danger !h-[26px]"
              title="This build does not recognise the options the agent offered"
            >
              <X size={11} strokeWidth={2.6} />
              Reject
            </button>
          )}
          <span className="ml-auto flex items-center gap-1 text-[10.5px] text-[var(--vd-text-tertiary)]">
            <AlertTriangle size={10} strokeWidth={2} />
            the agent is paused until you decide
          </span>
        </div>
      </div>
    </div>
  );
}
