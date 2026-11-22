import { useEffect, useMemo, useRef, useState } from 'react';
import { Check, Copy, CornerDownLeft, FileDown, WrapText } from 'lucide-react';
import { cn } from '@/lib/utils';
import { highlight } from '@/lib/highlight';
import { workspace } from '@/lib/bridge';
import { useApp } from '@/store/app';

interface CodeBlockProps {
  code: string;
  language?: string;
  filename?: string;
  showLineNumbers?: boolean;
  className?: string;
  /** Rendered inside a tool card: no outer border, tighter chrome. */
  embedded?: boolean;
}

export function CodeBlock({
  code,
  language,
  filename,
  showLineNumbers = false,
  className,
  embedded = false,
}: CodeBlockProps) {
  const [copied, setCopied] = useState(false);
  const [wrap, setWrap] = useState(false);
  const [expanded, setExpanded] = useState(false);
  const [applying, setApplying] = useState(false);
  const notify = useApp((s) => s.notify);
  const timer = useRef<number | null>(null);

  const html = useMemo(() => highlight(code, language), [code, language]);
  const lines = useMemo(() => code.split('\n'), [code]);
  const long = lines.length > 22;

  useEffect(() => {
    return () => {
      if (timer.current) window.clearTimeout(timer.current);
    };
  }, []);

  async function copy() {
    try {
      await navigator.clipboard.writeText(code);
      setCopied(true);
      if (timer.current) window.clearTimeout(timer.current);
      timer.current = window.setTimeout(() => setCopied(false), 1600);
    } catch {
      notify('Clipboard is unavailable in this context', 'danger');
    }
  }

  async function applyToFile() {
    setApplying(true);
    try {
      const { save } = await import('@tauri-apps/plugin-dialog');
      const target = await save({
        title: 'Apply snippet to file',
        defaultPath: filename ?? undefined,
      });
      if (!target) return;
      await workspace.applyEdit(target, code);
      notify(`Wrote ${code.length} bytes to ${target.split(/[\\/]/).pop()}`, 'success');
    } catch (error) {
      notify(error instanceof Error ? error.message : 'Could not write the file', 'danger');
    } finally {
      setApplying(false);
    }
  }

  return (
    <figure
      className={cn(
        'group/code relative overflow-hidden',
        embedded
          ? 'rounded-[var(--radius-sm)] border border-[var(--vd-line-soft)] bg-[var(--vd-surface-sunken)]'
          : 'vd-card my-3',
        className,
      )}
    >
      <figcaption
        className={cn(
          'flex items-center gap-2 border-b border-[var(--vd-line-soft)] px-3',
          embedded ? 'h-8' : 'h-9',
        )}
      >
        <span className="vd-label !text-[10px] !tracking-[0.11em]">{language ?? 'text'}</span>
        {filename && (
          <>
            <span className="text-[var(--vd-line-strong)]">/</span>
            <span className="truncate font-mono text-[11px] text-[var(--vd-text-secondary)]">
              {filename}
            </span>
          </>
        )}
        <span className="ml-auto flex items-center gap-0.5 opacity-0 transition-opacity group-hover/code:opacity-100 focus-within:opacity-100">
          <button
            type="button"
            onClick={() => setWrap((w) => !w)}
            className={cn('vd-icon-btn !h-6 !w-6', wrap && 'text-[var(--vd-accent)]')}
            title={wrap ? 'Disable soft wrap' : 'Soft wrap'}
            aria-pressed={wrap}
          >
            <WrapText size={13} strokeWidth={1.75} />
          </button>
          <button
            type="button"
            onClick={applyToFile}
            disabled={applying}
            className="vd-icon-btn !h-6 !w-6"
            title="Apply to a file"
          >
            <FileDown size={13} strokeWidth={1.75} />
          </button>
          <button
            type="button"
            onClick={copy}
            className="vd-icon-btn !h-6 !w-6"
            title="Copy snippet"
          >
            {copied ? (
              <Check size={13} strokeWidth={2.25} className="text-[var(--vd-accent)]" />
            ) : (
              <Copy size={13} strokeWidth={1.75} />
            )}
          </button>
        </span>
      </figcaption>

      <div
        className={cn(
          'vd-code vd-code-scroll px-3 py-2.5',
          !expanded && long && 'max-h-[300px] overflow-hidden',
        )}
      >
        <pre className="m-0">
          <code
            className={cn('hljs block', wrap && 'whitespace-pre-wrap break-words')}
            dangerouslySetInnerHTML={{
              __html: showLineNumbers ? withLineNumbers(html) : html,
            }}
          />
        </pre>
      </div>

      {long && !expanded && (
        <button
          type="button"
          onClick={() => setExpanded(true)}
          className="flex w-full items-center justify-center gap-1.5 border-t border-[var(--vd-line-soft)] bg-[var(--vd-surface-sunken)] py-1.5 text-[11px] text-[var(--vd-text-tertiary)] hover:text-[var(--vd-text)]"
        >
          <CornerDownLeft size={11} strokeWidth={2} />
          {lines.length - 22} more lines
        </button>
      )}
    </figure>
  );
}

function withLineNumbers(html: string): string {
  const rows = html.split('\n');
  return rows
    .map(
      (row, i) =>
        `<span class="table-row"><span class="table-cell select-none pr-3 text-right text-[var(--vd-text-tertiary)] opacity-60">${i + 1}</span><span class="table-cell">${row || ' '}</span></span>`,
    )
    .join('');
}
