import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import {
  AtSign,
  Clock,
  CornerDownLeft,
  File,
  Folder,
  ImagePlus,
  Loader2,
  Square,
  Terminal,
  X,
  Zap,
} from 'lucide-react';
import { open } from '@tauri-apps/plugin-dialog';
import type { FileEntry } from '@/types/acp';
import { cn } from '@/lib/utils';
import { filterCommands, GROUP_LABEL, slashEnterAction, type SlashCommand } from '@/lib/commands';
import { workspace } from '@/lib/bridge';
import { cancelTurn, sendPrompt } from '@/lib/actions';
import { formatClock, formatSpan } from '@/lib/time';
import { useMinute } from '@/lib/clock';
import {
  attachmentFromFile,
  attachmentFromPromptImage,
  baseName,
  formatSize,
  readAsDataUrl,
  rejectReason,
  type Attachment,
} from '@/lib/attachments';
import { selectAgentLabel, selectModelLabel, useApp } from '@/store/app';

type PaletteState =
  | { kind: 'slash'; query: string; start: number }
  | { kind: 'mention'; query: string; start: number }
  | null;

const SLASH_RE = /(^|\s)\/([\w-]*)$/;
const MENTION_RE = /(^|\s)@([\w./\\-]*)$/;

export function Composer() {
  const draft = useApp((s) => s.draft);
  const setDraft = useApp((s) => s.setDraft);
  const busy = useApp((s) => s.busy);
  const commands = useApp((s) => s.commands);
  const model = useApp(selectModelLabel);
  const agentName = useApp(selectAgentLabel);
  const status = useApp((s) => s.status);
  // The agent blocks its turn until an approval or an elicitation is answered.
  // Saying "working" there is wrong: nothing moves until the user acts.
  const awaitingInput = useApp((s) => Boolean(s.permission || s.elicitation));
  const sessionId = useApp((s) => s.sessionId);
  const openedAt = useApp((s) => s.openedAt);
  const now = useMinute();

  const textarea = useRef<HTMLTextAreaElement | null>(null);
  const [palette, setPalette] = useState<PaletteState>(null);
  const [activeIndex, setActiveIndex] = useState(0);
  const [files, setFiles] = useState<FileEntry[]>([]);
  const [loadingFiles, setLoadingFiles] = useState(false);
  const [attachments, setAttachments] = useState<Attachment[]>([]);
  const [attachError, setAttachError] = useState<string | null>(null);
  const [dragging, setDragging] = useState(false);
  const [reading, setReading] = useState(false);

  const extraCommands = useMemo<SlashCommand[]>(
    () =>
      commands.map((c) => ({
        name: c.name,
        description: c.description,
        group: 'workspace' as const,
        transport: 'agent' as const,
        inputHint: c.input?.hint,
      })),
    [commands],
  );

  const slashResults = useMemo(
    () => (palette?.kind === 'slash' ? filterCommands(palette.query, extraCommands) : []),
    [palette, extraCommands],
  );

  const mentionResults = useMemo(
    () => (palette?.kind === 'mention' ? files : []),
    [palette, files],
  );

  const results = palette?.kind === 'slash' ? slashResults : mentionResults;

  /* auto-grow */
  useEffect(() => {
    const node = textarea.current;
    if (!node) return;
    node.style.height = 'auto';
    node.style.height = `${Math.min(node.scrollHeight, 260)}px`;
  }, [draft]);

  /* focus on mount */
  useEffect(() => {
    textarea.current?.focus();
  }, []);

  /* file index for @mentions */
  useEffect(() => {
    if (palette?.kind !== 'mention') return;
    let cancelled = false;
    setLoadingFiles(true);
    const handle = window.setTimeout(async () => {
      try {
        const entries = await workspace.files(palette.query, 40);
        if (!cancelled) setFiles(entries);
      } catch {
        if (!cancelled) setFiles([]);
      } finally {
        if (!cancelled) setLoadingFiles(false);
      }
    }, 110);
    return () => {
      cancelled = true;
      window.clearTimeout(handle);
    };
  }, [palette]);

  useEffect(() => {
    setActiveIndex(0);
  }, [palette?.kind, palette?.query]);

  const detectPalette = useCallback((value: string, caret: number) => {
    const before = value.slice(0, caret);
    const slash = SLASH_RE.exec(before);
    if (slash && (slash.index === 0 || slash[1] === '' || /\s/.test(slash[1] ?? ''))) {
      const start = before.length - (slash[2]?.length ?? 0) - 1;
      setPalette({ kind: 'slash', query: slash[2] ?? '', start });
      return;
    }
    const mention = MENTION_RE.exec(before);
    if (mention) {
      const start = before.length - (mention[2]?.length ?? 0) - 1;
      setPalette({ kind: 'mention', query: mention[2] ?? '', start });
      return;
    }
    setPalette(null);
  }, []);

  function onChange(event: React.ChangeEvent<HTMLTextAreaElement>) {
    const value = event.target.value;
    setDraft(value);
    detectPalette(value, event.target.selectionStart ?? value.length);
  }

  function accept(index: number) {
    if (!palette) return;
    const caret = textarea.current?.selectionStart ?? draft.length;
    const before = draft.slice(0, palette.start);
    const after = draft.slice(caret);

    if (palette.kind === 'slash') {
      const command = slashResults[index];
      if (!command) return;
      const needsInput = Boolean(command.inputHint);
      const next = `${before}/${command.name}${needsInput ? ' ' : ''}${after}`;
      setDraft(next);
      setPalette(null);
      requestAnimationFrame(() => {
        const node = textarea.current;
        if (!node) return;
        const pos = before.length + command.name.length + (needsInput ? 2 : 1);
        node.focus();
        node.setSelectionRange(pos, pos);
      });
      return;
    }

    const entry = mentionResults[index];
    if (!entry) return;
    const next = `${before}@${entry.path} ${after}`;
    setDraft(next);
    setPalette(null);
    requestAnimationFrame(() => {
      const node = textarea.current;
      if (!node) return;
      const pos = before.length + entry.path.length + 2;
      node.focus();
      node.setSelectionRange(pos, pos);
    });
  }

  /**
   * Take files from a paste or a drop.
   *
   * A screenshot pasted from the clipboard arrives as a `File` with no name,
   * so it is labelled by where it came from rather than left blank. A file
   * that cannot be sent is refused here, with the reason on screen, because a
   * silent drop is the failure the user cannot see.
   */
  const addFiles = useCallback(async (incoming: File[]) => {
    if (incoming.length === 0) return;
    setAttachError(null);
    const accepted: Attachment[] = [];
    const problems: string[] = [];

    for (const file of incoming) {
      const reason = rejectReason(file);
      if (reason) {
        problems.push(reason);
        continue;
      }
      try {
        const dataUrl = await readAsDataUrl(file);
        accepted.push(attachmentFromFile(file, dataUrl));
      } catch {
        problems.push(`${file.name || 'that image'} could not be read`);
      }
    }

    if (accepted.length) setAttachments((current) => [...current, ...accepted]);
    if (problems.length) setAttachError(problems.join(' · '));
  }, []);

  /** The attach button: the native picker, then the backend reads the bytes. */
  const pickImages = useCallback(async () => {
    setAttachError(null);
    let picked: string | string[] | null = null;
    try {
      picked = await open({
        multiple: true,
        directory: false,
        title: 'Attach images',
        filters: [
          { name: 'Images', extensions: ['png', 'jpg', 'jpeg', 'gif', 'webp'] },
        ],
      });
    } catch {
      return;
    }
    if (!picked) return;
    const paths = Array.isArray(picked) ? picked : [picked];
    if (paths.length === 0) return;

    setReading(true);
    const accepted: Attachment[] = [];
    const problems: string[] = [];
    for (const path of paths) {
      try {
        const image = await workspace.imageRead(path);
        accepted.push(attachmentFromPromptImage(image, baseName(path)));
      } catch (error) {
        problems.push(error instanceof Error ? error.message : String(error));
      }
    }
    setReading(false);
    if (accepted.length) setAttachments((current) => [...current, ...accepted]);
    if (problems.length) setAttachError(problems.join(' · '));
  }, []);

  function onPaste(event: React.ClipboardEvent<HTMLTextAreaElement>) {
    const pasted = [...event.clipboardData.files].filter((file) => file.type.startsWith('image/'));
    if (pasted.length === 0) return;
    // Only swallow the paste when it is an image: a paste that also carries
    // text must still put the text in the box.
    if (!event.clipboardData.getData('text/plain')) event.preventDefault();
    void addFiles(pasted);
  }

  function onDrop(event: React.DragEvent<HTMLDivElement>) {
    const dropped = [...event.dataTransfer.files];
    if (dropped.length === 0) return;
    event.preventDefault();
    setDragging(false);
    void addFiles(dropped);
  }

  async function submit() {
    const value = draft;
    const images = attachments.map((entry) => entry.dataUrl);
    if (!value.trim() && images.length === 0) return;
    setPalette(null);
    setAttachments([]);
    setAttachError(null);
    await sendPrompt(value, images);
  }

  /**
   * Run a command the palette has already completed.
   *
   * The draft is written first because `sendPrompt` only clears the composer
   * when the text it was handed is the text still in it. A user who typed `/sta`
   * and pressed Enter on `status` would otherwise be left with `/sta` behind the
   * panel the command just opened.
   */
  async function runCommand(command: SlashCommand) {
    const text = `/${command.name}`;
    setPalette(null);
    setDraft(text);
    await sendPrompt(text);
  }

  function onKeyDown(event: React.KeyboardEvent<HTMLTextAreaElement>) {
    if (palette) {
      if (event.key === 'Escape') {
        event.preventDefault();
        setPalette(null);
        return;
      }
      if (results.length > 0) {
        if (event.key === 'ArrowDown') {
          event.preventDefault();
          setActiveIndex((i) => (i + 1) % results.length);
          return;
        }
        if (event.key === 'ArrowUp') {
          event.preventDefault();
          setActiveIndex((i) => (i - 1 + results.length) % results.length);
          return;
        }
        if (event.key === 'Tab') {
          event.preventDefault();
          accept(activeIndex);
          return;
        }
        if (event.key === 'Enter') {
          event.preventDefault();
          if (palette.kind === 'slash') {
            const action = slashEnterAction(slashResults, activeIndex);
            if (action.kind === 'run') {
              void runCommand(action.command);
              return;
            }
          }
          accept(activeIndex);
          return;
        }
      }
    }

    if (event.key === 'Enter' && !event.shiftKey) {
      event.preventDefault();
      void submit();
      return;
    }

    if (event.key === 'Escape' && busy) {
      event.preventDefault();
      void cancelTurn();
    }
  }

  const connected = status?.state === 'ready' || status?.state === 'busy';

  return (
    <div
      data-vd-protected="composer"
      className="relative mx-auto w-full max-w-[var(--vd-composer-max)] px-6 pb-4"
    >
      {palette && (
        <Palette
          kind={palette.kind}
          query={palette.query}
          commands={slashResults}
          files={mentionResults}
          activeIndex={activeIndex}
          loading={loadingFiles}
          onHover={setActiveIndex}
          onPick={accept}
        />
      )}

      {/* The chat's own clock, at the bottom of a long transcript.
          Every message carries its time, but the chat's start is only visible
          at the top of the transcript — which is exactly where you are not
          once you have been scrolling. This is the same fact the status bar
          shows, with the wall clock the status bar keeps in a tooltip. */}
      {sessionId && (
        <div
          data-vd-chat-clock
          className="mb-1 flex items-center gap-1.5 px-1 text-[10px] text-[var(--vd-text-tertiary)]"
          title={`This chat opened at ${formatClock(openedAt, now)}`}
        >
          <Clock size={9} strokeWidth={2} className="shrink-0 opacity-70" />
          <span className="vd-numeric">{formatClock(openedAt, now)}</span>
          <span className="opacity-40">·</span>
          <span className="vd-numeric">open {formatSpan(Math.max(0, now - openedAt))}</span>
        </div>
      )}

      <div
        onDragOver={(event) => {
          if (![...event.dataTransfer.types].includes('Files')) return;
          event.preventDefault();
          setDragging(true);
        }}
        onDragLeave={(event) => {
          // `dragleave` also fires when the pointer crosses into a child, so
          // the flag only clears when it has left the shell entirely.
          if (event.currentTarget.contains(event.relatedTarget as Node | null)) return;
          setDragging(false);
        }}
        onDrop={onDrop}
        className={cn(
          'vd-composer-shell @container relative rounded-[var(--radius-lg)] border bg-[var(--vd-surface)] ',
          'border-[var(--vd-line)] focus-within:border-[var(--vd-line-strong)]',
          dragging && 'border-[var(--vd-accent)]',
        )}
      >
        {dragging && (
          <div className="pointer-events-none absolute inset-0 z-10 grid place-items-center rounded-[var(--radius-lg)] bg-[var(--vd-surface)]/85 text-[11.5px] font-medium text-[var(--vd-accent)]">
            Drop to attach
          </div>
        )}

        {(attachments.length > 0 || attachError) && (
          <div className="flex flex-wrap items-center gap-1.5 px-2.5 pt-2">
            {attachments.map((entry) => (
              <span
                key={entry.id}
                className="group relative flex items-center gap-1.5 rounded-[var(--radius-xs)] border border-[var(--vd-line)] bg-[var(--vd-surface-sunken)] py-0.5 pl-0.5 pr-1.5"
                title={`${entry.name} · ${formatSize(entry.bytes)}`}
              >
                <img
                  src={entry.dataUrl}
                  alt=""
                  className="size-6 rounded-[2px] object-cover"
                  draggable={false}
                />
                <span className="max-w-[9rem] truncate text-[10.5px] text-[var(--vd-text-secondary)]">
                  {entry.name}
                </span>
                <span className="vd-numeric shrink-0 text-[9.5px] text-[var(--vd-text-tertiary)]">
                  {formatSize(entry.bytes)}
                </span>
                <button
                  type="button"
                  onClick={() =>
                    setAttachments((current) => current.filter((item) => item.id !== entry.id))
                  }
                  aria-label={`Remove ${entry.name}`}
                  className="shrink-0 text-[var(--vd-text-tertiary)] hover:text-[var(--vd-danger)]"
                >
                  <X size={10} strokeWidth={2.6} />
                </button>
              </span>
            ))}
            {attachError && (
              <span className="text-[10.5px] text-[var(--vd-danger)]">{attachError}</span>
            )}
          </div>
        )}

        <div className="flex items-start gap-2 px-3 pt-2.5">
          <span className="mt-[7px] shrink-0 text-[var(--vd-text-tertiary)]">
            <Terminal size={13} strokeWidth={1.9} />
          </span>
          <textarea
            ref={textarea}
            value={draft}
            onChange={onChange}
            onKeyDown={onKeyDown}
            onPaste={onPaste}
            onBlur={() => setPalette(null)}
            rows={1}
            spellCheck={false}
            role="combobox"
            aria-expanded={Boolean(palette)}
            aria-controls={palette ? 'vd-palette' : undefined}
            aria-autocomplete="list"
            aria-activedescendant={palette ? `vd-palette-option-${activeIndex}` : undefined}
            placeholder={
              awaitingInput
                ? 'Waiting on your answer above — the turn is paused'
                : busy
                  ? 'Agent is working — Enter queues your message'
                  : 'Ask Vibe to build something…'
            }
            className="vd-input max-h-[260px] min-h-[26px] flex-1 py-0.5"
          />
        </div>

        {/* The chips shrink first (`min-w-0`), but they bottom out at their
            padding, and at the window's own 940px minimum with the drawer open
            the shell is 158px — narrower than the chips plus one button, so
            Send was clipped off the right edge. `flex-wrap` fixes the clip but
            is the wrong tool: wrapping and shrinking are mutually exclusive on
            one axis, so the row would break onto a second line at 590px where
            shrinking alone fits. Instead the row keeps `nowrap` and the labels
            give way below `17rem` — see the `@min-[17rem]` spans below.

            The threshold is the busy row, not the idle one, because busy is the
            widest this row ever gets: three chips bottom out at 84px, Stop and
            Queue at 146px, and the gaps at 18px, so a labelled row needs 248px.
            Below 272px the labels drop and the same row needs 142px. Measuring
            against the idle row instead left Stop and Queue clipped at 218px. */}
        <div className="flex items-center gap-1.5 px-2.5 pb-2 pt-1.5">
          {/* Attach sits first in the row, before the chips, because it is the
              one control here that is about the message rather than about the
              agent. It keeps its icon at every width — the label is the only
              thing that gives way. */}
          <button
            type="button"
            onClick={() => void pickImages()}
            disabled={reading}
            className="vd-btn !h-[20px] !px-1.5 shrink-0"
            title="Attach an image (or paste / drop one)"
            aria-label="Attach an image"
          >
            {reading ? (
              <Loader2 size={11} strokeWidth={2.2} className="vd-spin" />
            ) : (
              <ImagePlus size={11} strokeWidth={2.2} />
            )}
          </button>
          {/* The model name is the one chip long enough to overflow the row, so
              it is the one that gives way. Without `min-w-0` a flex item refuses
              to shrink below its content, and the row pushed Stop and Queue past
              the composer's right edge — the buttons were there, just clipped. */}
          <span className="vd-chip !h-[20px] min-w-0 !text-[10px]" title={model}>
            <Zap size={9} strokeWidth={2.4} className="shrink-0 text-[var(--vd-accent)]" />
            <span className="truncate">{model}</span>
          </span>
          <span className="vd-chip !h-[20px] min-w-0 !text-[10px]" title={agentName}>
            <span className="truncate">{agentName}</span>
          </span>
          <span
            className={cn(
              'vd-chip !h-[20px] shrink-0 !text-[10px]',
              connected ? 'vd-chip-accent' : 'vd-chip-warn',
            )}
            title={status?.error ?? 'Agent process state'}
          >
            <span
              className={cn(
                'size-1 shrink-0 rounded-full',
                connected ? 'bg-[var(--vd-accent)]' : 'bg-[var(--vd-warn)]',
              )}
            />
            {/* The dot already carries the state in colour, and the top bar
                names it in words, so the word is the first thing to go when the
                composer is too narrow to hold the row. */}
            <span className="hidden @min-[17rem]:inline">{status?.state ?? 'offline'}</span>
          </span>

          <span className="ml-auto flex shrink-0 items-center gap-1.5">
            {/* Gated on the composer's own width, not the window's. The drawer
                narrows the composer while the window stays wide, so a `sm:`
                breakpoint kept this hint visible in a box too small to hold it
                and pushed Send past the right edge. */}
            <span className="hidden items-center gap-1 text-[10.5px] text-[var(--vd-text-tertiary)] @min-[24rem]:flex">
              <kbd className="vd-kbd">/</kbd>
              commands
              <kbd className="vd-kbd ml-1">@</kbd>
              files
            </span>
            {busy && (
              <button
                type="button"
                onClick={() => void cancelTurn()}
                className="vd-btn vd-btn-danger !h-[26px]"
                title="Interrupt the running turn (Esc)"
                aria-label="Stop the running turn"
              >
                <Square size={10} strokeWidth={2.6} fill="currentColor" />
                <span className="hidden @min-[17rem]:inline">Stop</span>
              </button>
            )}
            <button
              type="button"
              onClick={() => void submit()}
              disabled={!draft.trim() && attachments.length === 0}
              className="vd-btn vd-btn-primary !h-[26px] !px-2.5"
              title={busy ? 'Queue for when this turn ends (Enter)' : 'Send (Enter)'}
              aria-label={busy ? 'Queue this message' : 'Send this message'}
            >
              <span className="hidden @min-[17rem]:inline">{busy ? 'Queue' : 'Send'}</span>
              <CornerDownLeft size={11} strokeWidth={2.4} />
            </button>
          </span>
        </div>
      </div>
    </div>
  );
}

interface PaletteProps {
  kind: 'slash' | 'mention';
  query: string;
  commands: SlashCommand[];
  files: FileEntry[];
  activeIndex: number;
  loading: boolean;
  onHover: (index: number) => void;
  onPick: (index: number) => void;
}

function Palette({
  kind,
  query,
  commands,
  files,
  activeIndex,
  loading,
  onHover,
  onPick,
}: PaletteProps) {
  const listRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    const node = listRef.current?.querySelector<HTMLElement>(`[data-index="${activeIndex}"]`);
    node?.scrollIntoView({ block: 'nearest' });
  }, [activeIndex]);

  const empty = kind === 'slash' ? commands.length === 0 : files.length === 0;

  return (
    <div
      className="vd-pop absolute bottom-[calc(100%-6px)] left-6 right-6 z-30 overflow-hidden rounded-[var(--radius-md)] border border-[var(--vd-line-strong)] bg-[var(--vd-overlay)] shadow-[var(--vd-shadow-pop)]"
      onMouseDown={(event) => event.preventDefault()}
    >
      <div className="flex h-8 items-center gap-2 border-b border-[var(--vd-line-soft)] px-2.5">
        {kind === 'slash' ? (
          <Terminal size={11} strokeWidth={2} className="text-[var(--vd-text-tertiary)]" />
        ) : (
          <AtSign size={11} strokeWidth={2} className="text-[var(--vd-text-tertiary)]" />
        )}
        <span className="vd-label !text-[9.5px]">
          {kind === 'slash' ? 'commands' : 'workspace files'}
        </span>
        {query && (
          <span className="font-mono text-[11px] text-[var(--vd-text-secondary)]">{query}</span>
        )}
        <span className="ml-auto flex items-center gap-1 text-[10px] text-[var(--vd-text-tertiary)]">
          {loading && <Loader2 size={10} className="vd-spin" />}
          <kbd className="vd-kbd">↑↓</kbd>
          <kbd className="vd-kbd">↵</kbd>
        </span>
      </div>

      <div
        id="vd-palette"
        ref={listRef}
        role="listbox"
        aria-label={kind === 'slash' ? 'Commands' : 'Workspace files'}
        className="max-h-[280px] overflow-y-auto py-1"
      >
        {empty && (
          <p className="m-0 px-3 py-3 text-[12px] text-[var(--vd-text-tertiary)]">
            {kind === 'slash' ? 'No command matches that.' : 'No file matches that.'}
          </p>
        )}

        {kind === 'slash'
          ? commands.map((command, index) => (
              <button
                key={`${command.transport}:${command.name}`}
                id={`vd-palette-option-${index}`}
                type="button"
                role="option"
                aria-selected={index === activeIndex}
                data-index={index}
                onMouseEnter={() => onHover(index)}
                onClick={() => onPick(index)}
                className={cn(
                  'flex w-full items-baseline gap-2.5 px-2.5 py-1.5 text-left ',
                  index === activeIndex
                    ? 'bg-[var(--vd-surface-hover)]'
                    : 'hover:bg-[var(--vd-surface-hover)]',
                )}
              >
                <span className="w-[104px] shrink-0 truncate font-mono text-[11.5px] text-[var(--vd-text)]">
                  /{command.name}
                </span>
                <span className="min-w-0 flex-1 truncate text-[11.5px] text-[var(--vd-text-secondary)]">
                  {command.description}
                </span>
                <span className="vd-label shrink-0 !text-[9px]">{GROUP_LABEL[command.group]}</span>
              </button>
            ))
          : files.map((file, index) => (
              <button
                key={file.path}
                id={`vd-palette-option-${index}`}
                type="button"
                role="option"
                aria-selected={index === activeIndex}
                data-index={index}
                onMouseEnter={() => onHover(index)}
                onClick={() => onPick(index)}
                className={cn(
                  'flex w-full items-center gap-2 px-2.5 py-1.5 text-left ',
                  index === activeIndex
                    ? 'bg-[var(--vd-surface-hover)]'
                    : 'hover:bg-[var(--vd-surface-hover)]',
                )}
              >
                {file.isDir ? (
                  <Folder
                    size={11}
                    strokeWidth={1.9}
                    className="shrink-0 text-[var(--vd-text-tertiary)]"
                  />
                ) : (
                  <File
                    size={11}
                    strokeWidth={1.9}
                    className="shrink-0 text-[var(--vd-text-tertiary)]"
                  />
                )}
                <span className="truncate font-mono text-[11.5px] text-[var(--vd-text)]">
                  {file.path}
                </span>
              </button>
            ))}
      </div>
    </div>
  );
}

export function useComposerFocus() {
  return useCallback(() => {
    document.querySelector<HTMLTextAreaElement>('.vd-input')?.focus();
  }, []);
}
