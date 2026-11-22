import {
  useEffect,
  useMemo,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type MouseEvent,
} from 'react';
import {
  Archive,
  ArchiveRestore,
  CheckSquare,
  ClipboardCopy,
  Eye,
  EyeOff,
  Folder,
  FolderOpen,
  FolderPlus,
  ListChecks,
  Loader2,
  MessageSquare,
  RefreshCw,
  Square,
  SquarePen,
  Trash2,
  X,
} from 'lucide-react';
import { cn, basename, chatTitle, relativeTime, truncateMiddle } from '@/lib/utils';
import {
  addWorkstation,
  archiveChat,
  archiveChats,
  deleteChat,
  deleteChats,
  errorMessage,
  loadChatState,
  newChat,
  openChat,
  refreshChats,
  removeWorkstation,
  selectWorkstation,
} from '@/lib/actions';
import { workspace } from '@/lib/bridge';
import { useApp } from '@/store/app';
import { ResizeHandle } from '@/components/chrome/ResizeHandle';
import {
  ContextMenu,
  MenuItem,
  MenuSeparator,
  anchorAt,
  keyboardAnchor,
  type MenuAnchor,
} from '@/components/ui/ContextMenu';

type RailMenu =
  | { kind: 'workstation'; path: string; anchor: MenuAnchor }
  | { kind: 'chat'; sessionId: string; anchor: MenuAnchor };

/**
 * Right-hand rail: pinned project folders on top, that folder's chats below.
 *
 * A chat is an ACP session, so the list is `session/list({ cwd })` — the agent
 * owns the storage and VibeDesk only renders it. Switching a workstation moves
 * the bridge's cwd and reopens the folder's most recent chat.
 */
export function WorkstationRail() {
  const workstations = useApp((s) => s.workstations);
  const active = useApp((s) => s.activeWorkstation);
  const chats = useApp((s) => s.chats);
  const chatsLoading = useApp((s) => s.chatsLoading);
  const sessionId = useApp((s) => s.sessionId);
  const cwd = useApp((s) => s.cwd);
  const busy = useApp((s) => s.busy);
  const archivedChats = useApp((s) => s.archivedChats);
  const deletedChats = useApp((s) => s.deletedChats);
  const showArchived = useApp((s) => s.showArchived);
  const setShowArchived = useApp((s) => s.setShowArchived);

  const [adding, setAdding] = useState(false);
  const [switching, setSwitching] = useState<string | null>(null);
  const [menu, setMenu] = useState<RailMenu | null>(null);

  /**
   * Selection lives here rather than in the store: the rail is its only reader,
   * and a workstation switch has to clear it anyway. Keeping it local means no
   * migration and no chance of a stale id surviving into another folder.
   */
  const [selecting, setSelecting] = useState(false);
  const [selected, setSelected] = useState<string[]>([]);
  const [anchor, setAnchor] = useState<string | null>(null);
  const [bulkBusy, setBulkBusy] = useState(false);

  useEffect(() => {
    if (!active) return;
    void refreshChats();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [active]);

  useEffect(() => {
    void loadChatState();
  }, []);

  /**
   * The chats the rail shows.
   *
   * Filtered here rather than in `refreshChats` so the "show archived" toggle is
   * a render instead of a round trip, and so a chat archived in another window
   * cannot leave a stale row behind. The open chat is always kept: hiding what
   * you are currently looking at is worse than showing one extra row.
   */
  const visibleChats = useMemo(() => {
    const gone = new Set(deletedChats);
    const away = new Set(archivedChats);
    return chats.filter((chat) => {
      if (gone.has(chat.sessionId)) return false;
      if (!away.has(chat.sessionId)) return true;
      return showArchived || chat.sessionId === sessionId;
    });
  }, [chats, deletedChats, archivedChats, showArchived, sessionId]);

  const archivedHere = useMemo(() => {
    const away = new Set(archivedChats);
    return chats.filter((chat) => away.has(chat.sessionId)).length;
  }, [chats, archivedChats]);

  /**
   * The chats a bulk action can act on.
   *
   * The open chat is excluded: the agent is writing to it, and moving its
   * directory out from under the process is how the app dies. Archiving it is
   * still available from its own menu, which is why this only gates selection.
   */
  const selectable = useMemo(
    () => visibleChats.filter((chat) => chat.sessionId !== sessionId),
    [visibleChats, sessionId],
  );

  // A different folder means a different list; carrying ids across would act on
  // chats the user can no longer see.
  useEffect(() => {
    setSelected([]);
    setAnchor(null);
  }, [active]);

  // Drop ids that left the list — deleted in another window, or archived while
  // "show archived" is off. Returning `prev` unchanged keeps this from looping.
  useEffect(() => {
    const live = new Set(selectable.map((chat) => chat.sessionId));
    setSelected((prev) => {
      const next = prev.filter((id) => live.has(id));
      return next.length === prev.length ? prev : next;
    });
  }, [selectable]);

  useEffect(() => {
    if (!selecting) return;
    function onKey(event: KeyboardEvent) {
      if (event.key === 'Escape') {
        setSelecting(false);
        setSelected([]);
        setAnchor(null);
      }
    }
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [selecting]);

  function exitSelection() {
    setSelecting(false);
    setSelected([]);
    setAnchor(null);
  }

  /**
   * Click a row in selection mode.
   *
   * Shift extends from the anchor and replaces the selection, which is what a
   * file manager does and therefore what the hand expects. Plain click toggles
   * one row and moves the anchor.
   */
  function toggleSelect(sessionId: string, event: MouseEvent) {
    if (event.shiftKey && anchor) {
      const ids = selectable.map((chat) => chat.sessionId);
      const from = ids.indexOf(anchor);
      const to = ids.indexOf(sessionId);
      if (from !== -1 && to !== -1) {
        const [lo, hi] = from < to ? [from, to] : [to, from];
        setSelected(ids.slice(lo, hi + 1));
        return;
      }
    }
    setAnchor(sessionId);
    setSelected((prev) =>
      prev.includes(sessionId) ? prev.filter((id) => id !== sessionId) : [...prev, sessionId],
    );
  }

  async function runBulk(action: 'archive' | 'restore' | 'delete') {
    if (selected.length === 0 || bulkBusy) return;
    setBulkBusy(true);
    try {
      if (action === 'delete') {
        await deleteChats(selected);
      } else {
        await archiveChats(selected, action === 'archive');
      }
    } finally {
      setBulkBusy(false);
      exitSelection();
    }
  }

  async function onAdd() {
    setAdding(true);
    try {
      await addWorkstation();
    } finally {
      setAdding(false);
    }
  }

  async function onSelect(path: string) {
    if (path === active) return;
    setSwitching(path);
    try {
      await selectWorkstation(path);
    } finally {
      setSwitching(null);
    }
  }

  function openWorkstationMenu(event: MouseEvent, path: string) {
    event.preventDefault();
    event.stopPropagation();
    setMenu({ kind: 'workstation', path, anchor: anchorAt(event, 6) });
  }

  function openChatMenu(event: MouseEvent, sessionId: string) {
    event.preventDefault();
    event.stopPropagation();
    // Five rows: open, copy, separator, archive/restore, delete. Passing the
    // item count rather than the row count is what keeps the menu inside the
    // window when the row is near the bottom edge.
    setMenu({ kind: 'chat', sessionId, anchor: anchorAt(event, 5) });
  }

  // Shift+F10 and the Menu key open the same menus from the keyboard. Both rows
  // are buttons, so they are already reachable with Tab; this is the second half
  // that makes the menu itself reachable.
  function workstationMenuKey(event: ReactKeyboardEvent<HTMLElement>, path: string) {
    const anchor = keyboardAnchor(event, 6);
    if (anchor) setMenu({ kind: 'workstation', path, anchor });
  }

  function chatMenuKey(event: ReactKeyboardEvent<HTMLElement>, sessionId: string) {
    const anchor = keyboardAnchor(event, 5);
    if (anchor) setMenu({ kind: 'chat', sessionId, anchor });
  }

  /** Switch first, then create — `open: false` keeps the switch from opening a chat. */
  async function newChatHere(path: string) {
    setMenu(null);
    if (path !== active) await selectWorkstation(path, false);
    if (useApp.getState().cwd !== path) return;
    await newChat();
  }

  async function reveal(path: string) {
    setMenu(null);
    try {
      await workspace.reveal(path);
    } catch (failure) {
      useApp.getState().addNotice(errorMessage(failure), 'error');
    }
  }

  async function copy(text: string) {
    setMenu(null);
    try {
      await navigator.clipboard.writeText(text);
    } catch (failure) {
      useApp.getState().addNotice(errorMessage(failure), 'error');
    }
  }

  return (
    <aside className="vd-sidebar">
      <ResizeHandle />
      <header className="flex items-center justify-between px-3 pb-1.5 pt-3">
        <span className="vd-label">Workstations</span>
        <button
          type="button"
          onClick={() => void onAdd()}
          disabled={adding}
          className="grid size-5 place-items-center rounded-[var(--radius-xs)] text-[var(--vd-text-tertiary)] hover:bg-[var(--vd-surface-hover)] hover:text-[var(--vd-text)] disabled:opacity-50"
          title="Add a folder as a workstation"
        >
          {adding ? (
            <Loader2 size={12} strokeWidth={2} className="vd-spin" />
          ) : (
            <FolderPlus size={12} strokeWidth={2} />
          )}
        </button>
      </header>

      <div className="max-h-[38%] overflow-y-auto px-1.5 pb-2">
        {workstations.length === 0 ? (
          <p className="m-0 px-1.5 py-1 text-[11px] leading-[1.5] text-[var(--vd-text-tertiary)]">
            No projects yet. Add a folder to keep its chats together.
          </p>
        ) : (
          <div className="flex flex-col gap-0.5">
            {workstations.map((entry) => {
              const isActive = entry.path === active;
              return (
                <div key={entry.path} className="group relative">
                  <button
                    type="button"
                    onClick={() => void onSelect(entry.path)}
                    onContextMenu={(event) => openWorkstationMenu(event, entry.path)}
                    onKeyDown={(event) => workstationMenuKey(event, entry.path)}
                    className={cn(
                      'relative flex w-full items-start gap-2 rounded-[var(--radius-sm)] py-1.5 pl-2 pr-6 text-left ',
                      isActive
                        ? 'bg-[var(--vd-surface-hover)]'
                        : 'hover:bg-[var(--vd-surface-hover)]',
                    )}
                    title={entry.path}
                  >
                    {isActive && (
                      <span className="absolute left-0 top-1/2 h-4 w-[2px] -translate-y-1/2 rounded-full bg-[var(--vd-accent)]" />
                    )}
                    <Folder
                      size={12}
                      strokeWidth={1.9}
                      className={cn(
                        'mt-[3px] shrink-0',
                        isActive ? 'text-[var(--vd-accent)]' : 'text-[var(--vd-text-tertiary)]',
                      )}
                    />
                    <span className="min-w-0 flex-1">
                      <span className="block truncate text-[12.5px] font-medium leading-[1.35] text-[var(--vd-text)]">
                        {entry.name}
                      </span>
                      <span className="mt-0.5 block truncate font-mono text-[9.5px] leading-[1.4] text-[var(--vd-text-tertiary)]">
                        {truncateMiddle(entry.path, 30)}
                      </span>
                    </span>
                    {switching === entry.path && (
                      <Loader2
                        size={11}
                        strokeWidth={2}
                        className="vd-spin mt-[3px] shrink-0 text-[var(--vd-text-tertiary)]"
                      />
                    )}
                  </button>
                  <button
                    type="button"
                    onClick={() => void removeWorkstation(entry.path)}
                    className="absolute right-1 top-1.5 grid size-4 place-items-center rounded-[var(--radius-xs)] text-[var(--vd-text-tertiary)] opacity-0 transition-opacity hover:bg-[var(--vd-surface)] hover:text-[var(--vd-danger)] focus-visible:opacity-100 group-hover:opacity-100"
                    title="Remove from workstations"
                  >
                    <X size={10} strokeWidth={2.4} />
                  </button>
                </div>
              );
            })}
          </div>
        )}
      </div>

      <div className="vd-hairline mx-3" />

      <header className="flex items-center justify-between px-3 pb-1.5 pt-2.5">
        <span className="vd-label">{selecting ? `${selected.length} selected` : 'Chats'}</span>
        <div className="flex items-center gap-0.5">
          <button
            type="button"
            onClick={() => (selecting ? exitSelection() : setSelecting(true))}
            disabled={selectable.length === 0}
            className={cn(
              'grid size-5 place-items-center rounded-[var(--radius-xs)] hover:bg-[var(--vd-surface-hover)] disabled:opacity-40',
              selecting
                ? 'text-[var(--vd-accent)]'
                : 'text-[var(--vd-text-tertiary)] hover:text-[var(--vd-text)]',
            )}
            title={selecting ? 'Leave selection mode' : 'Select several chats to archive or delete'}
          >
            <ListChecks size={12} strokeWidth={2} />
          </button>
          {archivedHere > 0 && (
            <button
              type="button"
              onClick={() => setShowArchived(!showArchived)}
              className={cn(
                'grid size-5 place-items-center rounded-[var(--radius-xs)] hover:bg-[var(--vd-surface-hover)]',
                showArchived
                  ? 'text-[var(--vd-accent)]'
                  : 'text-[var(--vd-text-tertiary)] hover:text-[var(--vd-text)]',
              )}
              title={
                showArchived
                  ? `Hide ${archivedHere} archived ${archivedHere === 1 ? 'chat' : 'chats'}`
                  : `Show ${archivedHere} archived ${archivedHere === 1 ? 'chat' : 'chats'}`
              }
            >
              {showArchived ? (
                <Eye size={11} strokeWidth={2} />
              ) : (
                <EyeOff size={11} strokeWidth={2} />
              )}
            </button>
          )}
          <button
            type="button"
            onClick={() => void refreshChats()}
            className="grid size-5 place-items-center rounded-[var(--radius-xs)] text-[var(--vd-text-tertiary)] hover:bg-[var(--vd-surface-hover)] hover:text-[var(--vd-text)]"
            title="Refresh chats"
          >
            <RefreshCw size={11} strokeWidth={2} className={cn(chatsLoading && 'vd-spin')} />
          </button>
          <button
            type="button"
            onClick={() => void newChat()}
            disabled={!cwd || busy}
            className="grid size-5 place-items-center rounded-[var(--radius-xs)] text-[var(--vd-text-tertiary)] hover:bg-[var(--vd-surface-hover)] hover:text-[var(--vd-text)] disabled:opacity-40"
            title="New chat in this workstation"
          >
            <SquarePen size={12} strokeWidth={2} />
          </button>
        </div>
      </header>

      <div className="vd-scroll-fade min-h-0 flex-1 overflow-y-auto px-1.5 pb-2">
        {!cwd ? (
          <p className="m-0 px-1.5 py-1 text-[11px] leading-[1.5] text-[var(--vd-text-tertiary)]">
            Select a workstation to see its chats.
          </p>
        ) : visibleChats.length === 0 ? (
          <p className="m-0 px-1.5 py-1 text-[11px] leading-[1.5] text-[var(--vd-text-tertiary)]">
            {chatsLoading
              ? 'Reading chat history…'
              : archivedHere > 0
                ? 'Every chat here is archived.'
                : 'No chats in this folder yet.'}
          </p>
        ) : (
          <div className="flex flex-col gap-0.5">
            {visibleChats.map((chat) => {
              const isOpen = chat.sessionId === sessionId;
              const isArchived = archivedChats.includes(chat.sessionId);
              const isSelected = selected.includes(chat.sessionId);
              return (
                <button
                  key={chat.sessionId}
                  type="button"
                  onClick={(event) =>
                    selecting ? toggleSelect(chat.sessionId, event) : void openChat(chat.sessionId)
                  }
                  onContextMenu={(event) => openChatMenu(event, chat.sessionId)}
                  onKeyDown={(event) => chatMenuKey(event, chat.sessionId)}
                  disabled={selecting ? isOpen : busy && !isOpen}
                  className={cn(
                    'relative flex w-full items-start gap-2 rounded-[var(--radius-sm)] py-1.5 pl-2 pr-2 text-left disabled:opacity-50',
                    isSelected
                      ? 'bg-[var(--vd-accent-wash)]'
                      : isOpen
                        ? 'bg-[var(--vd-surface-hover)]'
                        : 'hover:bg-[var(--vd-surface-hover)]',
                    isArchived && !isOpen && !isSelected && 'opacity-55',
                  )}
                  title={
                    selecting && isOpen
                      ? 'This chat is open. Switch to another chat to select it.'
                      : chatTitle(chat.title)
                  }
                >
                  {isOpen && !selecting && (
                    <span className="absolute left-0 top-1/2 h-4 w-[2px] -translate-y-1/2 rounded-full bg-[var(--vd-accent)]" />
                  )}
                  {selecting ? (
                    isSelected ? (
                      <CheckSquare
                        size={12}
                        strokeWidth={2}
                        className="mt-[3px] shrink-0 text-[var(--vd-accent)]"
                      />
                    ) : (
                      <Square
                        size={12}
                        strokeWidth={1.9}
                        className="mt-[3px] shrink-0 text-[var(--vd-text-tertiary)]"
                      />
                    )
                  ) : isArchived ? (
                    <Archive
                      size={12}
                      strokeWidth={1.9}
                      className={cn(
                        'mt-[3px] shrink-0',
                        isOpen ? 'text-[var(--vd-accent)]' : 'text-[var(--vd-text-tertiary)]',
                      )}
                    />
                  ) : (
                    <MessageSquare
                      size={12}
                      strokeWidth={1.9}
                      className={cn(
                        'mt-[3px] shrink-0',
                        isOpen ? 'text-[var(--vd-accent)]' : 'text-[var(--vd-text-tertiary)]',
                      )}
                    />
                  )}
                  <span className="min-w-0 flex-1">
                    <span
                      className={cn(
                        'block truncate text-[12px] leading-[1.35]',
                        isOpen ? 'text-[var(--vd-text)]' : 'text-[var(--vd-text-secondary)]',
                      )}
                    >
                      {chatTitle(chat.title)}
                    </span>
                    <span className="mt-0.5 flex items-center gap-1.5 text-[9.5px] text-[var(--vd-text-tertiary)]">
                      <span className="font-mono">{chat.sessionId.slice(0, 6)}</span>
                      <span className="opacity-50">·</span>
                      <span>{relativeTime(chat.updatedAt)}</span>
                      {isArchived && (
                        <>
                          <span className="opacity-50">·</span>
                          <span>archived</span>
                        </>
                      )}
                    </span>
                  </span>
                </button>
              );
            })}
          </div>
        )}
      </div>

      {selecting ? (
        <footer className="flex items-center justify-between gap-1 border-t border-[var(--vd-line)] px-2 py-1.5">
          <button
            type="button"
            onClick={() =>
              setSelected(
                selected.length === selectable.length
                  ? []
                  : selectable.map((chat) => chat.sessionId),
              )
            }
            disabled={selectable.length === 0}
            className="rounded-[var(--radius-xs)] px-1.5 py-0.5 text-[9.5px] font-medium text-[var(--vd-text-tertiary)] hover:bg-[var(--vd-surface-hover)] hover:text-[var(--vd-text)] disabled:opacity-40"
            title={
              selected.length === selectable.length
                ? 'Clear the selection'
                : 'Select every chat in this folder'
            }
          >
            {selected.length === selectable.length && selectable.length > 0 ? 'None' : 'All'}
          </button>
          <div className="flex items-center gap-0.5">
            <button
              type="button"
              onClick={() => void runBulk('archive')}
              disabled={selected.length === 0 || bulkBusy}
              className="grid size-5 place-items-center rounded-[var(--radius-xs)] text-[var(--vd-text-tertiary)] hover:bg-[var(--vd-surface-hover)] hover:text-[var(--vd-text)] disabled:opacity-40"
              title={`Archive ${selected.length === 1 ? 'this chat' : `${selected.length} chats`}`}
            >
              {bulkBusy ? (
                <Loader2 size={11} strokeWidth={2} className="vd-spin" />
              ) : (
                <Archive size={11} strokeWidth={2} />
              )}
            </button>
            <button
              type="button"
              onClick={() => void runBulk('restore')}
              disabled={selected.length === 0 || bulkBusy}
              className="grid size-5 place-items-center rounded-[var(--radius-xs)] text-[var(--vd-text-tertiary)] hover:bg-[var(--vd-surface-hover)] hover:text-[var(--vd-text)] disabled:opacity-40"
              title={`Restore ${selected.length === 1 ? 'this chat' : `${selected.length} chats`}`}
            >
              <ArchiveRestore size={11} strokeWidth={2} />
            </button>
            <button
              type="button"
              onClick={() => void runBulk('delete')}
              disabled={selected.length === 0 || bulkBusy}
              className="grid size-5 place-items-center rounded-[var(--radius-xs)] text-[var(--vd-text-tertiary)] hover:bg-[var(--vd-surface-hover)] hover:text-[var(--vd-danger)] disabled:opacity-40"
              title={`Delete ${selected.length === 1 ? 'this chat' : `${selected.length} chats`}`}
            >
              <Trash2 size={11} strokeWidth={2} />
            </button>
            <button
              type="button"
              onClick={exitSelection}
              className="grid size-5 place-items-center rounded-[var(--radius-xs)] text-[var(--vd-text-tertiary)] hover:bg-[var(--vd-surface-hover)] hover:text-[var(--vd-text)]"
              title="Leave selection mode"
            >
              <X size={11} strokeWidth={2.4} />
            </button>
          </div>
        </footer>
      ) : (
        <footer className="flex items-center justify-between border-t border-[var(--vd-line)] px-3 py-1.5">
          <span className="truncate text-[9.5px] text-[var(--vd-text-tertiary)]">
            {cwd ? basename(cwd) : 'no folder'}
          </span>
          <span className="vd-numeric shrink-0 text-[9.5px] text-[var(--vd-text-tertiary)]">
            {visibleChats.length} {visibleChats.length === 1 ? 'chat' : 'chats'}
          </span>
        </footer>
      )}

      {menu?.kind === 'workstation' && (
        <ContextMenu anchor={menu.anchor} onClose={() => setMenu(null)}>
          <MenuItem
            icon={Folder}
            label="Open"
            disabled={menu.path === active}
            onSelect={() => {
              setMenu(null);
              void onSelect(menu.path);
            }}
          />
          <MenuItem
            icon={SquarePen}
            label="New chat here"
            disabled={busy}
            onSelect={() => void newChatHere(menu.path)}
          />
          <MenuSeparator />
          <MenuItem
            icon={FolderOpen}
            label="Reveal in file manager"
            onSelect={() => void reveal(menu.path)}
          />
          <MenuItem icon={ClipboardCopy} label="Copy path" onSelect={() => void copy(menu.path)} />
          <MenuSeparator />
          <MenuItem
            icon={X}
            label="Remove from workstations"
            danger
            onSelect={() => {
              setMenu(null);
              void removeWorkstation(menu.path);
            }}
          />
        </ContextMenu>
      )}

      {menu?.kind === 'chat' && (
        <ContextMenu anchor={menu.anchor} onClose={() => setMenu(null)}>
          <MenuItem
            icon={MessageSquare}
            label="Open chat"
            disabled={menu.sessionId === sessionId}
            onSelect={() => {
              setMenu(null);
              void openChat(menu.sessionId);
            }}
          />
          <MenuItem
            icon={ClipboardCopy}
            label="Copy session id"
            onSelect={() => void copy(menu.sessionId)}
          />
          <MenuSeparator />
          {archivedChats.includes(menu.sessionId) ? (
            <MenuItem
              icon={ArchiveRestore}
              label="Restore from archive"
              onSelect={() => {
                setMenu(null);
                void archiveChat(menu.sessionId, false);
              }}
            />
          ) : (
            <MenuItem
              icon={Archive}
              label="Archive chat"
              onSelect={() => {
                setMenu(null);
                void archiveChat(menu.sessionId, true);
              }}
            />
          )}
          <MenuItem
            icon={Trash2}
            label="Delete chat"
            danger
            disabled={menu.sessionId === sessionId}
            onSelect={() => {
              const chat = chats.find((entry) => entry.sessionId === menu.sessionId);
              setMenu(null);
              void deleteChat(menu.sessionId, chatTitle(chat?.title ?? 'This chat'));
            }}
          />
        </ContextMenu>
      )}
    </aside>
  );
}
