import { useMemo } from 'react';
import {
  Brain,
  Check,
  ChevronRight,
  CircleDashed,
  FileText,
  Globe,
  Loader2,
  MoveRight,
  PencilLine,
  Repeat,
  Search,
  Terminal,
  Trash2,
  Wrench,
  X,
} from 'lucide-react';
import type { ToolCall, ToolCallContent, ToolKind } from '@/types/acp';
import { cn, basename } from '@/lib/utils';
import { useDisclosure } from '@/lib/disclosure';
import { KIND_LABEL } from '@/lib/tool-run';
import { imageFromNotice, imageFromText, toolImages } from '@/lib/tool-image';
import { DiffView } from '@/components/code/DiffView';
import { ToolImage } from '@/components/chat/ToolImage';

const KIND_ICON: Record<ToolKind, typeof FileText> = {
  read: FileText,
  edit: PencilLine,
  delete: Trash2,
  move: MoveRight,
  search: Search,
  execute: Terminal,
  think: Brain,
  fetch: Globe,
  switch_mode: Repeat,
  other: Wrench,
};

interface ToolCallCardProps {
  call: ToolCall;
}

export function ToolCallCard({ call }: ToolCallCardProps) {
  const { open, mounted, armed, toggle } = useDisclosure();
  const kind = (call.kind ?? 'other') as ToolKind;
  const Icon = KIND_ICON[kind] ?? Wrench;
  const status = call.status ?? 'pending';

  const { diffs, texts, terminals } = useMemo(
    () => splitContent(call.content ?? []),
    [call.content],
  );

  const images = useMemo(() => toolImages(call), [call.content, call.rawOutput]);

  // A reader that hands the bytes to the model and the path to the client
  // returns a notice, not content. The picture is above; printing the notice as
  // JSON underneath it is the raw dump the image was supposed to replace.
  const rawIsImage = useMemo(() => Boolean(imageFromNotice(call.rawOutput)), [call.rawOutput]);

  const summary = useMemo(() => {
    const input = call.rawInput as Record<string, unknown> | null | undefined;
    if (input && typeof input === 'object') {
      if (typeof input.command === 'string') return input.command;
      if (typeof input.path === 'string') return input.path;
      if (typeof input.pattern === 'string') return input.pattern;
      if (typeof input.query === 'string') return input.query;
      if (typeof input.url === 'string') return input.url;
    }
    if (call.locations?.length) return call.locations[0]?.path ?? '';
    return '';
  }, [call.rawInput, call.locations]);

  const hasBody =
    diffs.length > 0 ||
    texts.length > 0 ||
    terminals.length > 0 ||
    images.length > 0 ||
    Boolean(call.rawOutput);

  return (
    <div
      data-vd-tool={kind}
      data-vd-tool-status={status}
      className={cn(
        'vd-card overflow-hidden',
        status === 'failed' && 'border-[color-mix(in_oklab,var(--vd-danger)_34%,transparent)]',
      )}
    >
      <button
        type="button"
        onClick={() => hasBody && toggle()}
        className={cn(
          'flex w-full items-center gap-2.5 px-3 py-2 text-left',
          hasBody ? 'cursor-pointer' : 'cursor-default',
        )}
        aria-expanded={open}
      >
        <span
          className={cn(
            'grid size-6 shrink-0 place-items-center rounded-[var(--radius-xs)] border',
            status === 'failed'
              ? 'border-[color-mix(in_oklab,var(--vd-danger)_30%,transparent)] bg-[var(--vd-danger-wash)] text-[var(--vd-danger)]'
              : status === 'completed'
                ? 'border-[var(--vd-line)] bg-[var(--vd-surface-sunken)] text-[var(--vd-accent)]'
                : 'border-[var(--vd-line)] bg-[var(--vd-surface-sunken)] text-[var(--vd-text-secondary)]',
          )}
        >
          <Icon size={13} strokeWidth={1.9} />
        </span>

        <span className="min-w-0 flex-1">
          <span className="flex items-baseline gap-2">
            <span className="truncate text-[12.5px] font-medium text-[var(--vd-text)]">
              {call.title ?? KIND_LABEL[kind]}
            </span>
            <span className="vd-label shrink-0 !text-[9.5px] !tracking-[0.1em]">
              {KIND_LABEL[kind]}
            </span>
          </span>
          {summary && (
            <span className="mt-0.5 block truncate font-mono text-[11px] text-[var(--vd-text-tertiary)]">
              {summary}
            </span>
          )}
        </span>

        <StatusGlyph status={status} />

        {hasBody && (
          <ChevronRight
            size={13}
            strokeWidth={2}
            className={cn(
              'shrink-0 text-[var(--vd-text-tertiary)] transition-transform',
              open && 'rotate-90',
            )}
          />
        )}
      </button>

      {mounted && hasBody && (
        <div className="vd-expand" data-open={open && armed}>
          <div className="space-y-2 border-t border-[var(--vd-line-soft)] bg-[var(--vd-surface-sunken)] p-2.5">
            {images.map((image, index) => (
              <ToolImage key={`image-${index}`} image={image} />
            ))}

            {diffs.map((diff, index) => (
              <DiffView
                key={`diff-${index}`}
                path={diff.path}
                oldText={diff.oldText ?? ''}
                newText={diff.newText}
              />
            ))}

            {texts.map((text, index) => (
              <pre
                key={`text-${index}`}
                className="vd-code m-0 max-h-[320px] overflow-auto whitespace-pre-wrap break-words rounded-[var(--radius-sm)] border border-[var(--vd-line-soft)] bg-[var(--vd-surface)] px-2.5 py-2 text-[11.5px] text-[var(--vd-text-secondary)]"
              >
                {text}
              </pre>
            ))}

            {terminals.map((terminal) => (
              <div
                key={terminal.terminalId}
                className="flex items-center gap-2 rounded-[var(--radius-sm)] border border-[var(--vd-line-soft)] bg-[var(--vd-surface)] px-2.5 py-1.5 font-mono text-[11px] text-[var(--vd-text-tertiary)]"
              >
                <Terminal size={12} strokeWidth={1.9} />
                {terminal.terminalId}
              </div>
            ))}

            {call.rawOutput !== undefined && call.rawOutput !== null && !rawIsImage && (
              <details className="group">
                <summary className="vd-label cursor-pointer list-none !text-[9.5px] hover:text-[var(--vd-text-secondary)]">
                  raw output
                </summary>
                <pre className="vd-code m-0 mt-1.5 max-h-56 overflow-auto rounded-[var(--radius-sm)] border border-[var(--vd-line-soft)] bg-[var(--vd-surface)] px-2.5 py-2 text-[11px] text-[var(--vd-text-tertiary)]">
                  {JSON.stringify(call.rawOutput, null, 2)}
                </pre>
              </details>
            )}

            {call.locations && call.locations.length > 0 && (
              <div className="flex flex-wrap gap-1.5 pt-0.5">
                {call.locations.slice(0, 6).map((loc) => (
                  <span key={`${loc.path}:${loc.line ?? 0}`} className="vd-chip font-mono">
                    {basename(loc.path)}
                    {loc.line ? `:${loc.line}` : ''}
                  </span>
                ))}
              </div>
            )}
          </div>
        </div>
      )}
    </div>
  );
}

function StatusGlyph({ status }: { status: string }) {
  if (status === 'in_progress') {
    return (
      <Loader2 size={13} strokeWidth={2} className="vd-spin shrink-0 text-[var(--vd-accent)]" />
    );
  }
  if (status === 'completed') {
    return <Check size={13} strokeWidth={2.4} className="shrink-0 text-[var(--vd-accent)]" />;
  }
  if (status === 'failed') {
    return <X size={13} strokeWidth={2.4} className="shrink-0 text-[var(--vd-danger)]" />;
  }
  return (
    <CircleDashed size={13} strokeWidth={1.9} className="shrink-0 text-[var(--vd-text-tertiary)]" />
  );
}

function splitContent(content: ToolCallContent[]) {
  const diffs: Array<{ path: string; oldText?: string | null; newText: string }> = [];
  const texts: string[] = [];
  const terminals: Array<{ terminalId: string }> = [];

  for (const item of content) {
    if (!item || typeof item !== 'object') continue;
    if (item.type === 'diff') {
      diffs.push({ path: item.path, oldText: item.oldText, newText: item.newText });
    } else if (item.type === 'terminal') {
      terminals.push({ terminalId: item.terminalId });
    } else if (item.type === 'content') {
      const block = item.content as { type?: string; text?: string };
      if (block?.type === 'text' && typeof block.text === 'string' && block.text.trim()) {
        // A notice that names an image is rendered as the image, not as the
        // JSON it happens to be written in.
        if (imageFromText(block.text)) continue;
        texts.push(block.text);
      }
    }
  }

  return { diffs, texts, terminals };
}
