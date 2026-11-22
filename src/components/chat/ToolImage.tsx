import { useEffect, useState } from 'react';
import { ImageOff, Loader2 } from 'lucide-react';
import { workspace } from '@/lib/bridge';
import type { ToolImage as ToolImageData } from '@/lib/tool-image';
import { basename } from '@/lib/utils';

/**
 * One image a tool call produced.
 *
 * The bytes are not always in the result. A `read_file` on an image hands the
 * bytes to the model and the client only a notice naming the path, so the file
 * has to be read back off disk before it can be shown. That read is why this is
 * a component with its own state rather than an `<img src>` in the card: it can
 * fail, it can be slow, and a card that renders nothing while it waits looks
 * like the bug it is fixing.
 */
export function ToolImage({ image }: { image: ToolImageData }) {
  const [src, setSrc] = useState<string | null>(image.dataUrl ?? null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (image.dataUrl || !image.path) return;
    let cancelled = false;

    workspace
      .imageRead(image.path)
      .then((result) => {
        if (cancelled) return;
        setSrc(`data:${result.mimeType};base64,${result.data}`);
      })
      .catch((cause: unknown) => {
        if (cancelled) return;
        setError(cause instanceof Error ? cause.message : String(cause));
      });

    return () => {
      cancelled = true;
    };
  }, [image.dataUrl, image.path]);

  const label = image.path ? basename(image.path) : 'image';
  const size = image.bytes ? ` · ${formatBytes(image.bytes)}` : '';

  if (error) {
    return (
      <div className="flex items-center gap-2 rounded-[var(--radius-sm)] border border-[var(--vd-line-soft)] bg-[var(--vd-surface)] px-2.5 py-2 text-[11px] text-[var(--vd-text-tertiary)]">
        <ImageOff size={12} strokeWidth={1.9} className="shrink-0" />
        <span className="min-w-0 flex-1 truncate" title={error}>
          {label} could not be shown — {error}
        </span>
      </div>
    );
  }

  if (!src) {
    return (
      <div className="flex items-center gap-2 rounded-[var(--radius-sm)] border border-[var(--vd-line-soft)] bg-[var(--vd-surface)] px-2.5 py-2 text-[11px] text-[var(--vd-text-tertiary)]">
        <Loader2 size={12} strokeWidth={2} className="vd-spin shrink-0" />
        <span className="truncate">reading {label}</span>
      </div>
    );
  }

  return (
    <figure className="m-0 overflow-hidden rounded-[var(--radius-sm)] border border-[var(--vd-line-soft)] bg-[var(--vd-surface)]">
      <img
        src={src}
        alt={label}
        data-vd-tool-image={image.mime}
        className="block max-h-[420px] w-full object-contain"
        loading="lazy"
        decoding="async"
      />
      <figcaption className="flex items-center justify-between gap-2 border-t border-[var(--vd-line-soft)] px-2.5 py-1 font-mono text-[10.5px] text-[var(--vd-text-tertiary)]">
        <span className="min-w-0 truncate" title={image.path ?? label}>
          {label}
        </span>
        <span className="shrink-0">
          {image.mime}
          {size}
        </span>
      </figcaption>
    </figure>
  );
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}
