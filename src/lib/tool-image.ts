/**
 * Images that arrive inside a tool result.
 *
 * A tool result reaches the transcript in one of two shapes, and neither one
 * used to render. An agent that returns an image as a content block sends
 * `{ type: 'image', data, mimeType }`, which the card dropped because it only
 * looked for text. A `read_file` on an image sends a notice instead of the
 * bytes — `{ path, content: "Image file (image/png, 186091 bytes)." }` — because
 * the bytes go to the model out of band, so the card had nothing to show and
 * printed the JSON instead.
 *
 * Both are recognised here rather than in the component, because the rule is
 * about the protocol and the component is about layout. The notice is matched
 * on its text rather than on the tool's name: the same notice comes back from
 * any reader that hands the bytes to the model and the path to the client.
 */

export interface ToolImage {
  /** A data URL, when the bytes are already in hand. */
  dataUrl?: string;
  /** A path to read, when the result only names the file. */
  path?: string;
  mime: string;
  bytes?: number;
}

/**
 * `Image file (image/png, 186091 bytes).`
 *
 * The size is optional because a reader that does not stat the file still
 * knows what it is. The mime is not: a notice with no image type is not an
 * image, and rendering it would mean guessing at bytes we have not seen.
 */
const NOTICE = /^Image file \(([^,)]+?)(?:,\s*(\d+)\s*bytes)?\)\.?$/i;

/** An image notice, from the object a reader returned. */
export function imageFromNotice(value: unknown): ToolImage | null {
  if (!value || typeof value !== 'object') return null;
  const record = value as Record<string, unknown>;
  const path = typeof record.path === 'string' ? record.path.trim() : '';
  const content = typeof record.content === 'string' ? record.content.trim() : '';
  if (!path || !content) return null;

  const match = NOTICE.exec(content);
  if (!match) return null;

  const mime = (match[1] ?? '').trim().toLowerCase();
  if (!mime.startsWith('image/')) return null;

  const bytes = match[2] ? Number(match[2]) : undefined;
  return { path, mime, bytes: Number.isFinite(bytes) ? bytes : undefined };
}

/**
 * An image notice that arrived as a JSON string rather than as an object.
 *
 * Some agents stringify a tool result before putting it in a text block, so the
 * notice is a string that happens to parse. Anything that does not parse is
 * left alone: a text block is the common case and must not be swallowed.
 */
export function imageFromText(text: string): ToolImage | null {
  const trimmed = text.trim();
  if (!trimmed.startsWith('{')) return null;
  try {
    return imageFromNotice(JSON.parse(trimmed));
  } catch {
    return null;
  }
}

/** An image content block, already carrying its bytes. */
export function imageFromBlock(block: unknown): ToolImage | null {
  if (!block || typeof block !== 'object') return null;
  const record = block as Record<string, unknown>;
  if (record.type !== 'image') return null;

  const data = typeof record.data === 'string' ? record.data : '';
  if (!data) return null;

  const mime =
    typeof record.mimeType === 'string' && record.mimeType ? record.mimeType : 'image/png';
  // Some agents echo the data URL back whole rather than as bare base64.
  const dataUrl = data.startsWith('data:') ? data : `data:${mime};base64,${data}`;
  return { dataUrl, mime };
}

/**
 * Every image a tool call carries, in the order it carries them.
 *
 * Deduplicated by source: a call that reports the same file in both its content
 * blocks and its raw output would otherwise render the picture twice.
 */
export function toolImages(call: {
  content?: unknown;
  rawOutput?: unknown;
}): ToolImage[] {
  const found: ToolImage[] = [];
  const seen = new Set<string>();

  const add = (image: ToolImage | null) => {
    if (!image) return;
    const key = image.dataUrl ?? image.path ?? '';
    if (!key || seen.has(key)) return;
    seen.add(key);
    found.push(image);
  };

  const content = Array.isArray(call.content) ? call.content : [];
  for (const item of content) {
    if (!item || typeof item !== 'object') continue;
    const record = item as Record<string, unknown>;
    if (record.type !== 'content') continue;

    const block = record.content;
    add(imageFromBlock(block));
    if (block && typeof block === 'object') {
      const text = (block as { text?: unknown }).text;
      if (typeof text === 'string') add(imageFromText(text));
    }
  }

  add(imageFromNotice(call.rawOutput));
  return found;
}
