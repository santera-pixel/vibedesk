/**
 * Images the composer attaches to a prompt.
 *
 * Three ways in — paste, drop, and the file picker — and they all end up in
 * the same shape: a data URL. That is what `<img>` renders, what `FileReader`
 * produces, and what survives a round trip through the transcript. The ACP
 * block is derived from it at send time by `toPromptImage`.
 *
 * The limits here mirror the backend's: `image_read` refuses anything over
 * 8 MB or that is not one of these four formats, so the composer refuses it
 * first and says why, instead of letting the send fail.
 */
import type { PromptImage } from '@/types/acp';

/** The formats ACP agents accept, and the ones `image_read` sniffs for. */
export const IMAGE_MIME_TYPES = [
  'image/png',
  'image/jpeg',
  'image/gif',
  'image/webp',
] as const;

/** Matches `MAX_IMAGE_BYTES` in `src-tauri/src/commands/workspace.rs`. */
export const MAX_IMAGE_BYTES = 8 * 1024 * 1024;

export interface Attachment {
  id: string;
  /** `data:<mime>;base64,<bytes>` — the one representation everything shares. */
  dataUrl: string;
  mimeType: string;
  name: string;
  bytes: number;
}

export function isImageMime(mime: string): boolean {
  return (IMAGE_MIME_TYPES as readonly string[]).includes(mime.toLowerCase());
}

/** Base64 length back to bytes, without decoding it. */
export function dataUrlBytes(dataUrl: string): number {
  const comma = dataUrl.indexOf(',');
  if (comma < 0) return 0;
  const body = dataUrl.slice(comma + 1);
  const padding = body.endsWith('==') ? 2 : body.endsWith('=') ? 1 : 0;
  return Math.max(0, Math.floor((body.length * 3) / 4) - padding);
}

export function dataUrlMime(dataUrl: string): string {
  const match = /^data:([^;,]+)[;,]/.exec(dataUrl);
  return match?.[1]?.toLowerCase() ?? '';
}

/**
 * Why this file cannot be attached, or `null` when it can.
 *
 * Returned rather than thrown so the composer can show the reason next to the
 * composer instead of in a console nobody reads.
 */
export function rejectReason(file: { type: string; size: number; name: string }): string | null {
  if (!isImageMime(file.type)) {
    return `${file.name} is not a PNG, JPEG, GIF or WebP image`;
  }
  if (file.size > MAX_IMAGE_BYTES) {
    return `${file.name} is ${formatSize(file.size)}; the limit is 8 MB`;
  }
  if (file.size === 0) return `${file.name} is empty`;
  return null;
}

export function formatSize(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return '0 B';
  if (bytes < 1024) return `${Math.round(bytes)} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(bytes < 10 * 1024 ? 1 : 0)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

let counter = 0;

function nextId(): string {
  counter += 1;
  return `att-${Date.now().toString(36)}-${counter}`;
}

/** A `File` from a paste or a drop, as an attachment. */
export function attachmentFromFile(file: File, dataUrl: string): Attachment {
  return {
    id: nextId(),
    dataUrl,
    mimeType: file.type || dataUrlMime(dataUrl) || 'image/png',
    name: file.name || 'pasted image',
    bytes: file.size || dataUrlBytes(dataUrl),
  };
}

/** A file the picker read through the backend, as an attachment. */
export function attachmentFromPromptImage(image: PromptImage, name: string): Attachment {
  const dataUrl = `data:${image.mimeType};base64,${image.data}`;
  return {
    id: nextId(),
    dataUrl,
    mimeType: image.mimeType,
    name,
    bytes: dataUrlBytes(dataUrl),
  };
}

/** The file name out of a path, for a chip label. */
export function baseName(path: string): string {
  const parts = path.split(/[\\/]/);
  return parts[parts.length - 1] || path;
}

/** A `File` from a paste or a drop, as a data URL. */
export function readAsDataUrl(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => {
      const result = reader.result;
      if (typeof result === 'string') resolve(result);
      else reject(new Error('the file could not be read as a data URL'));
    };
    reader.onerror = () => reject(reader.error ?? new Error('the file could not be read'));
    reader.readAsDataURL(file);
  });
}
