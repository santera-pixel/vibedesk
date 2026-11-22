import { useMemo, useState } from 'react';
import { ChevronRight } from 'lucide-react';
import { cn } from '@/lib/utils';
import { diffFromTexts, parseDiff, type DiffRow } from '@/lib/diff';

interface DiffViewProps {
  patch?: string;
  oldText?: string;
  newText?: string;
  path?: string;
  className?: string;
  collapsedByDefault?: boolean;
}

export function DiffView({
  patch,
  oldText,
  newText,
  path,
  className,
  collapsedByDefault = false,
}: DiffViewProps) {
  const parsed = useMemo(() => {
    if (patch) return parseDiff(patch);
    if (oldText !== undefined && newText !== undefined) {
      return diffFromTexts(oldText, newText);
    }
    return { rows: [], additions: 0, deletions: 0, isUnified: false };
  }, [patch, oldText, newText]);

  const [open, setOpen] = useState(!collapsedByDefault);
  const visible = open ? parsed.rows : parsed.rows.slice(0, 8);

  return (
    <div
      className={cn(
        'overflow-hidden rounded-[var(--radius-sm)] border border-[var(--vd-line-soft)]',
        className,
      )}
    >
      <div className="flex h-8 items-center gap-2 border-b border-[var(--vd-line-soft)] bg-[var(--vd-surface-sunken)] px-2.5">
        <span className="vd-label !text-[10px]">diff</span>
        {path && (
          <span className="truncate font-mono text-[11px] text-[var(--vd-text-secondary)]">
            {path}
          </span>
        )}
        <span className="ml-auto flex items-center gap-2 font-mono text-[10.5px]">
          <span className="text-[var(--vd-accent)]">+{parsed.additions}</span>
          <span className="text-[var(--vd-danger)]">−{parsed.deletions}</span>
        </span>
      </div>

      <div className="max-h-[380px] overflow-auto bg-[var(--vd-surface)]">
        {visible.map((row, index) => (
          <DiffLine key={index} row={row} />
        ))}
        {parsed.rows.length === 0 && (
          <p className="m-0 px-3 py-2 text-[12px] text-[var(--vd-text-tertiary)]">
            No textual changes.
          </p>
        )}
      </div>

      {parsed.rows.length > 8 && (
        <button
          type="button"
          onClick={() => setOpen((v) => !v)}
          className="flex w-full items-center justify-center gap-1 border-t border-[var(--vd-line-soft)] bg-[var(--vd-surface-sunken)] py-1.5 text-[11px] text-[var(--vd-text-tertiary)] hover:text-[var(--vd-text)]"
        >
          <ChevronRight
            size={11}
            strokeWidth={2}
            className={cn('transition-transform', open && 'rotate-90')}
          />
          {open ? 'Collapse diff' : `Show all ${parsed.rows.length} lines`}
        </button>
      )}
    </div>
  );
}

function DiffLine({ row }: { row: DiffRow }) {
  if (row.kind === 'hunk') {
    return (
      <div className="vd-diff-line bg-[var(--vd-surface-sunken)] text-[var(--vd-text-tertiary)]">
        <span className="vd-diff-gutter" />
        <span className="px-2 py-0.5">{row.text}</span>
      </div>
    );
  }
  if (row.kind === 'meta') {
    return (
      <div className="vd-diff-line text-[var(--vd-text-tertiary)]">
        <span className="vd-diff-gutter" />
        <span className="px-2 py-0.5">{row.text}</span>
      </div>
    );
  }
  const marker = row.kind === 'add' ? '+' : row.kind === 'del' ? '−' : ' ';
  return (
    <div
      className={cn(
        'vd-diff-line',
        row.kind === 'add' && 'vd-diff-add',
        row.kind === 'del' && 'vd-diff-del',
      )}
    >
      <span className="vd-diff-gutter">{row.newLine ?? row.oldLine ?? ''}</span>
      <span className="whitespace-pre-wrap break-words px-2 py-0.5">
        <span
          className={cn(
            'mr-1.5 select-none',
            row.kind === 'add' && 'text-[var(--vd-accent)]',
            row.kind === 'del' && 'text-[var(--vd-danger)]',
            row.kind === 'context' && 'text-[var(--vd-text-tertiary)]',
          )}
        >
          {marker}
        </span>
        {row.text || ' '}
      </span>
    </div>
  );
}
