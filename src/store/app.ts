import { create } from 'zustand';
import type {
  AgentLogLine,
  AgentStatus,
  AvailableCommand,
  ConfigOption,
  ElicitationRequest,
  JsonValue,
  PermissionRequest,
  PlanEntry,
  PluginRecord,
  SessionModeState,
  SessionUpdate,
  ToolCall,
  TrustState,
  UpdateStatus,
  UsageCost,
  UsageMeta,
  AccountSnapshot,
  VibeEnvironment,
  InstallStatus,
  ProvisionStatus,
  SessionSummary,
  Workstation,
} from '@/types/acp';
import type {
  ActionsConfig,
  CustomizePaths,
  LayoutConfig,
  PanelsConfig,
  PendingState,
  PluginSource,
  ThemeConfig,
} from '@/lib/customize';
import { stripPreamble } from '@/lib/preamble';
import { isStartupDiagnostic } from '@/lib/harness';
import { skillLoads, type SkillUse } from '@/lib/skill-use';
import type { SettingsSectionId } from '@/lib/settings';
import type { CapabilityPanelId } from '@/lib/capabilities';
import { uid } from '@/lib/utils';
import { transitionTo } from '@/lib/motion';

/**
 * `settings` is the directory; every other id is a section inside it.
 *
 * The section ids stay in the union because a dozen call sites open one
 * directly — the status bar opens Configuration, an action opens Plugins, the
 * title bar opens Status. Those keep working and land on the section, with the
 * rail's Settings button lit, because `setPanel` routes a section id through
 * the settings surface rather than rendering it as a peer of it.
 */
/**
 * `CapabilityPanelId` is imported rather than declared here: the capability
 * layer owns the list, because the Rust registry is what decides which
 * capabilities have a panel at all. These are peers of `context` and `status`,
 * not settings sections — each renders data that only exists while the app is
 * running, so there is nothing to configure in a settings rail.
 */
/**
 * `'swarm'` is a peer of the capability panels but not a `CapabilityPanelId`:
 * the swarm is not a capability, it is what the six capabilities make possible.
 * Keeping it out of that union is what keeps it out of the ledger's totals and
 * out of the "N of 6 on" count, both of which would be wrong with a seventh
 * entry in the array.
 */
export type PanelId = SettingsSectionId | 'settings' | CapabilityPanelId | 'swarm' | null;

export type TranscriptItem =
  | {
      type: 'user';
      id: string;
      text: string;
      at: number;
      /** Data URLs, in the order they were attached. */
      images?: string[];
      /**
       * How long the turn this message started took, in ms.
       *
       * Stamped onto the row when the turn settles rather than kept in a
       * single "last turn" slot, because a chat has many turns and the one
       * the user is reading is rarely the newest.
       */
      elapsed?: number;
      pending?: boolean;
      echoed?: boolean;
    }
  | { type: 'agent'; id: string; text: string; streaming: boolean; at: number }
  | { type: 'reasoning'; id: string; text: string; streaming: boolean; at: number }
  | { type: 'tool'; id: string; call: ToolCall; at: number }
  | { type: 'plan'; id: string; entries: PlanEntry[]; at: number }
  | { type: 'context'; id: string; text: string; at: number }
  | {
      type: 'notice';
      id: string;
      level: 'info' | 'warn' | 'error' | 'success';
      text: string;
      at: number;
    };

export interface Toast {
  id: string;
  message: string;
  tone: 'neutral' | 'success' | 'danger';
}

/**
 * A prompt typed while a turn was already running.
 *
 * ACP has no steer method — the app server rejects a second turn outright with
 * `A turn is already running` — so the only safe thing to do with a message
 * sent mid-turn is hold it and dispatch it when the turn ends. It is never
 * dropped and never interleaved into a turn it does not belong to.
 */
export interface SkillUseRecord {
  name: string;
  bundle: string | null;
  /** How many times this chat loaded it. */
  count: number;
  /** The last load, so the panel can order by recency. */
  at: number;
}

export interface TurnClock {
  startedAt: number;
  endedAt: number | null;
}

export interface QueuedPrompt {
  id: string;
  text: string;
  /** Data URLs held with the message, so a queued image is not lost. */
  images?: string[];
  /** The chat it was typed in. A queued prompt never leaks into another chat. */
  sessionId: string | null;
  at: number;
}

interface UsageState {
  used: number;
  size: number;
  cost: UsageCost | null;
  /** The token ledger from `usage_update._meta`; null before the first call. */
  meta: UsageMeta | null;
}

interface AppState {
  /* agent */
  status: AgentStatus | null;
  logs: AgentLogLine[];

  /* session */
  sessionId: string | null;
  cwd: string | null;
  title: string | null;
  modes: SessionModeState | null;
  configOptions: ConfigOption[];
  commands: AvailableCommand[];
  trust: TrustState | null;
  usage: UsageState | null;
  /** Plan and identity from the agent; null until the billing window asks. */
  account: AccountSnapshot | null;
  accountLoading: boolean;

  /* transcript */
  items: TranscriptItem[];
  busy: boolean;
  /**
   * When the running turn started, and when it settled.
   *
   * `endedAt` stays null while the turn runs so one shape serves both the
   * ticking counter and the final figure — the number the user watched climb
   * is the number that stays on screen.
   */
  turn: TurnClock | null;
  /** When this chat was opened, so the header can say how long it has been up. */
  openedAt: number;
  /**
   * Skills this chat actually loaded, keyed by name.
   *
   * The skills panel lists what is installed; this is what was used. The only
   * evidence of use is the tool call the agent made to load one, so it is
   * collected from the transcript rather than guessed from the catalog.
   */
  usedSkills: Record<string, SkillUseRecord>;
  lastStopReason: string | null;
  /** Prompts typed while a turn was running, in the order they were typed. */
  queued: QueuedPrompt[];
  /** True once the VibeDesk preamble has been sent into the current session. */
  preambleSent: boolean;
  /** Rows the item cap dropped from the front of the transcript. */
  trimmed: number;
  /** True while `session/load` is replaying a chat into the transcript. */
  chatLoading: boolean;

  /* interaction */
  permission: PermissionRequest | null;
  elicitation: ElicitationRequest | null;

  /* workstations and chats */
  workstations: Workstation[];
  activeWorkstation: string | null;
  chats: SessionSummary[];
  chatsLoading: boolean;
  /** Chats the user put away. Hidden from the rail, reversible. */
  archivedChats: string[];
  /** Chats the user deleted. Their session directory is in the trash. */
  deletedChats: string[];
  /** Whether archived chats are shown in the rail. */
  showArchived: boolean;

  /* environment */
  environment: VibeEnvironment | null;
  plugins: PluginRecord[];
  update: UpdateStatus | null;
  setup: InstallStatus | null;
  /** Every provisioning step, as the setup screen renders it. */
  provision: ProvisionStatus | null;

  /* customization */
  themeConfig: ThemeConfig | null;
  themeError: string | null;
  layoutConfig: LayoutConfig | null;
  layoutError: string | null;
  actionsConfig: ActionsConfig | null;
  actionsError: string | null;
  panelsConfig: PanelsConfig | null;
  panelsError: string | null;
  customizePaths: CustomizePaths | null;
  pending: PendingState;
  pluginSources: PluginSource[];

  /* ui */
  theme: 'ink' | 'paper';
  /**
   * Creator mode. Not a theme: it changes what the agent is told it is doing,
   * so it lives in the store next to the session rather than in the theme
   * config, and it is persisted in `settings.json` so it survives a relaunch.
   */
  creatorMode: boolean;
  /**
   * Whether a swarm run is handed the installed skills.
   *
   * In the store rather than read per-panel because two panels show it: the
   * Skills tab owns the switch and the swarm panel says whether the run it is
   * describing was given the method. Persisted in `settings.json`.
   */
  swarmSkills: boolean;
  panel: PanelId;
  customPanel: string | null;
  toast: Toast | null;
  bootError: string | null;
  draft: string;

  /* actions */
  setStatus: (status: AgentStatus) => void;
  pushLog: (line: AgentLogLine) => void;
  setSession: (payload: {
    sessionId: string;
    modes?: SessionModeState | null;
    configOptions?: ConfigOption[];
    trust?: TrustState | null;
  }) => void;
  clearSession: () => void;
  setBusy: (busy: boolean) => void;
  setStopReason: (reason: string | null) => void;
  setUsage: (usage: UsageState) => void;
  setAccount: (account: AccountSnapshot | null) => void;
  setAccountLoading: (loading: boolean) => void;
  setTitle: (title: string) => void;
  setConfigOptions: (options: ConfigOption[]) => void;
  setCommands: (commands: AvailableCommand[]) => void;
  setWorkstations: (workstations: Workstation[]) => void;
  setActiveWorkstation: (path: string | null) => void;
  setChats: (chats: SessionSummary[]) => void;
  setChatsLoading: (loading: boolean) => void;
  setChatState: (archived: string[], deleted: string[]) => void;
  setShowArchived: (show: boolean) => void;
  setEnvironment: (env: VibeEnvironment) => void;
  setPlugins: (plugins: PluginRecord[]) => void;
  setUpdate: (update: UpdateStatus | null) => void;
  setSetup: (setup: InstallStatus | null) => void;
  setProvision: (provision: ProvisionStatus | null) => void;
  setPermission: (request: PermissionRequest | null) => void;
  setElicitation: (request: ElicitationRequest | null) => void;
  setPanel: (panel: PanelId) => void;
  /** A panel declared in `panels.json`. Mutually exclusive with `panel`. */
  setCustomPanel: (id: string | null) => void;
  setTheme: (theme: 'ink' | 'paper') => void;
  setCreatorMode: (creatorMode: boolean) => void;
  setSwarmSkills: (swarmSkills: boolean) => void;
  setThemeConfig: (theme: ThemeConfig | null) => void;
  setThemeError: (error: string | null) => void;
  setLayoutConfig: (layout: LayoutConfig | null) => void;
  setLayoutError: (error: string | null) => void;
  setActionsConfig: (actions: ActionsConfig | null) => void;
  setActionsError: (error: string | null) => void;
  setPanelsConfig: (panels: PanelsConfig | null) => void;
  setPanelsError: (error: string | null) => void;
  setCustomizePaths: (paths: CustomizePaths | null) => void;
  setPending: (pending: PendingState) => void;
  setPluginSources: (sources: PluginSource[]) => void;
  setBootError: (message: string | null) => void;
  setDraft: (draft: string) => void;
  notify: (message: string, tone?: Toast['tone']) => void;
  dismissToast: () => void;

  addUserMessage: (text: string, images?: string[]) => string;
  addContextChip: (text: string) => void;
  markPreambleSent: (sent?: boolean) => void;
  addNotice: (text: string, level?: 'info' | 'warn' | 'error' | 'success') => void;
  /** Hold a prompt until the running turn ends. */
  enqueuePrompt: (text: string, images?: string[]) => void;
  cancelQueued: (id: string) => void;
  /** Remove and return the next prompt for this chat, if there is one. */
  takeQueued: (sessionId: string | null) => QueuedPrompt | null;
  /** Drop a user row that never reached the agent. */
  dropUserMessage: (id: string) => void;
  /** Apply a batch in one store write, so a replay costs one render, not one per chunk. */
  applyUpdates: (updates: SessionUpdate[]) => void;
  /** Buffer an update and flush it with the rest of this frame's batch. */
  enqueueUpdate: (update: SessionUpdate) => void;
  setChatLoading: (loading: boolean) => void;
  finishStreaming: () => void;
}

function textOf(content: unknown): string {
  if (!content || typeof content !== 'object') return '';
  const block = content as { type?: string; text?: string };
  if (block.type === 'text' && typeof block.text === 'string') return block.text;
  return '';
}

/**
 * An image block as a data URL, or nothing.
 *
 * ACP sends each image as its own content block, so a prompt with two images
 * arrives as three chunks under one message id. `textOf` returns `''` for all
 * of them, which is why an attached image used to disappear: the reducer
 * dropped every chunk that carried no text.
 */
function imagesOf(content: unknown): string[] {
  if (!content || typeof content !== 'object') return [];
  const block = content as { type?: string; data?: string; mimeType?: string };
  if (block.type !== 'image' || typeof block.data !== 'string' || !block.data) return [];
  // Some agents echo the data URL back whole rather than as bare base64.
  if (block.data.startsWith('data:')) return [block.data];
  const mime = typeof block.mimeType === 'string' && block.mimeType ? block.mimeType : 'image/png';
  return [`data:${mime};base64,${block.data}`];
}

function replaceAt<T>(list: T[], index: number, value: T): T[] {
  const next = list.slice();
  next[index] = value;
  return next;
}

/* ------------------------------------------------------------------ *
 * Transcript reduction
 *
 * `session/load` replays an entire chat as a burst of `session/update`
 * notifications, one Tauri event each. Applying them one at a time meant one
 * store write and one React render per chunk, which is what made a long chat
 * crawl while it opened. Updates are now buffered and reduced in a single pass
 * per animation frame, and the transcript keeps only the newest rows.
 * ------------------------------------------------------------------ */

/** Rows kept in the transcript. Older rows are dropped from the front. */
export const MAX_TRANSCRIPT_ITEMS = 400;

/** A batch is flushed early past this size, so a replay cannot grow the buffer
 *  without bound while the window is hidden and frames are not being served. */
const MAX_PENDING_UPDATES = 1024;

/** Non-transcript state a batch of updates produces. */
interface Effects {
  usage?: UsageState;
  title?: string;
  configOptions?: ConfigOption[];
  commands?: AvailableCommand[];
  /** The ledger in force before this batch, so a bare update keeps it. */
  previousMeta: UsageMeta | null;
  logs: string[];
  /** Skills this batch loaded, in the order the calls arrived. */
  skills: SkillUse[];
}

/**
 * Fold one update into the transcript. Pure: the caller decides when to write.
 * Every branch returns the same array when nothing changed, so an unchanged
 * transcript keeps its identity and memoised rows stay put.
 *
 * `live` is whether a turn is running. Replayed history arrives through the
 * same chunks as a live turn, so without it every restored message would keep
 * a streaming caret and read as still being written.
 */
function reduceItems(
  items: TranscriptItem[],
  update: SessionUpdate,
  effects: Effects,
  live: boolean,
): TranscriptItem[] {
  switch (update.sessionUpdate) {
    case 'user_message_chunk': {
      // Replayed history carries the VibeDesk preamble inside the first user
      // message; strip it so a resumed chat shows what the user typed.
      const text = stripPreamble(textOf(update.content));
      const images = imagesOf(update.content);
      if (!text && images.length === 0) return items;
      const messageId = (update as { messageId?: string }).messageId;
      const index = messageId
        ? items.findIndex((i) => i.type === 'user' && i.id === messageId)
        : -1;
      if (index >= 0) {
        const existing = items[index];
        if (existing?.type === 'user') {
          // The row was adopted from an optimistic append and already holds the
          // whole message; the echo was only good for its id.
          if (existing.echoed) return items;
          return replaceAt(items, index, {
            ...existing,
            text: stripPreamble(existing.text + text),
            images: images.length ? [...(existing.images ?? []), ...images] : existing.images,
          });
        }
      }

      // The backend echoes the prompt back as a chunk. `addUserMessage` already
      // put it on screen with a local id, so appending here would show the same
      // message twice. Adopt the backend's id instead — that keeps the row
      // stable across a replay, when the echo is the only copy there is.
      if (messageId) {
        const lastUser = [...items].reverse().find((item) => item.type === 'user');
        if (
          lastUser?.type === 'user' &&
          lastUser.pending &&
          lastUser.text.startsWith(text)
        ) {
          return replaceAt(items, items.indexOf(lastUser), {
            ...lastUser,
            id: messageId,
            pending: false,
            echoed: true,
            images: images.length ? [...(lastUser.images ?? []), ...images] : lastUser.images,
          });
        }
      }

      return [
        ...items,
        {
          type: 'user',
          id: messageId ?? uid('user'),
          text,
          at: Date.now(),
          ...(images.length ? { images } : {}),
        },
      ];
    }

    case 'agent_message_chunk':
    case 'agent_thought_chunk': {
      const text = textOf(update.content);
      if (!text) return items;
      // The agent speaks before the session has a prompt only to report on
      // itself — a connector manifest that failed to fetch, say. That is a
      // diagnostic, not a reply, and it belongs in the bridge log rather than
      // in the transcript as the assistant's opening line.
      if (update.sessionUpdate === 'agent_message_chunk' && isStartupDiagnostic(items)) {
        effects.logs.push(`agent startup: ${text}`);
        return items;
      }
      const kind = update.sessionUpdate === 'agent_message_chunk' ? 'agent' : 'reasoning';
      const messageId = (update as { messageId?: string }).messageId;
      const index = messageId
        ? items.findIndex((i) => i.type === kind && i.id === messageId)
        : -1;
      if (index >= 0) {
        const existing = items[index];
        if (existing && (existing.type === 'agent' || existing.type === 'reasoning')) {
          return replaceAt(items, index, {
            ...existing,
            text: existing.text + text,
            streaming: live,
          });
        }
      }
      return [
        ...items,
        {
          type: kind,
          id: messageId ?? uid(kind),
          text,
          streaming: live,
          at: Date.now(),
        },
      ];
    }

    case 'tool_call': {
      const call = update as unknown as ToolCall;
      // A skill load is only visible here, on the way in. The row keeps the
      // call, but the panel needs the name, and re-walking the transcript on
      // every render to find it would be work per frame for a fact that never
      // changes once the call has arrived.
      for (const use of skillLoads(call)) effects.skills.push(use);
      const index = items.findIndex(
        (i) => i.type === 'tool' && i.call.toolCallId === call.toolCallId,
      );
      if (index >= 0) {
        const existing = items[index];
        if (existing?.type === 'tool') {
          return replaceAt(items, index, {
            ...existing,
            call: { ...existing.call, ...call },
          });
        }
      }
      return [...items, { type: 'tool', id: uid('tool'), call, at: Date.now() }];
    }

    case 'tool_call_update': {
      const patch = update as unknown as Partial<ToolCall> & { toolCallId: string };
      const index = items.findIndex(
        (i) => i.type === 'tool' && i.call.toolCallId === patch.toolCallId,
      );
      if (index < 0) {
        return [
          ...items,
          {
            type: 'tool',
            id: uid('tool'),
            call: { ...patch, toolCallId: patch.toolCallId } as ToolCall,
            at: Date.now(),
          },
        ];
      }
      const existing = items[index];
      if (existing?.type !== 'tool') return items;
      const merged: ToolCall = { ...existing.call };
      if (patch.title !== undefined && patch.title !== null) merged.title = patch.title;
      if (patch.kind !== undefined && patch.kind !== null) merged.kind = patch.kind;
      if (patch.status !== undefined && patch.status !== null) merged.status = patch.status;
      if (patch.locations !== undefined && patch.locations !== null) {
        merged.locations = patch.locations;
      }
      if (patch.rawInput !== undefined && patch.rawInput !== null) {
        merged.rawInput = patch.rawInput;
      }
      if (patch.rawOutput !== undefined && patch.rawOutput !== null) {
        merged.rawOutput = patch.rawOutput;
      }
      if (patch.content && patch.content.length > 0) {
        merged.content = [...(existing.call.content ?? []), ...patch.content];
      }
      return replaceAt(items, index, { ...existing, call: merged });
    }

    case 'plan': {
      const entries = (update as { entries: PlanEntry[] }).entries ?? [];
      const index = items.findIndex((i) => i.type === 'plan');
      if (index >= 0) {
        const existing = items[index];
        if (existing?.type === 'plan') {
          return replaceAt(items, index, { ...existing, entries });
        }
      }
      return [...items, { type: 'plan', id: uid('plan'), entries, at: Date.now() }];
    }

    case 'usage_update': {
      const u = update as unknown as {
        used: number;
        size: number;
        cost?: UsageCost | null;
        _meta?: UsageMeta | null;
      };
      // `_meta` is the token ledger and is absent before the first model call.
      // Keeping the previous value when it is missing stops a bare update from
      // wiping the cache figures the Context panel is showing.
      effects.usage = {
        used: u.used,
        size: u.size,
        cost: u.cost ?? null,
        meta: u._meta ?? effects.previousMeta,
      };
      return items;
    }

    case 'session_info_update': {
      const info = update as unknown as { title?: string };
      if (info.title) effects.title = info.title;
      return items;
    }

    case 'config_option_update': {
      const options = (update as unknown as { configOptions: ConfigOption[] }).configOptions;
      if (Array.isArray(options)) effects.configOptions = options;
      return items;
    }

    case 'available_commands_update': {
      const commands = (update as unknown as { availableCommands: AvailableCommand[] })
        .availableCommands;
      if (Array.isArray(commands)) effects.commands = commands;
      return items;
    }

    default: {
      const method = (update as { sessionUpdate?: string }).sessionUpdate;
      // Unknown update kinds are surfaced in the log drawer rather than
      // silently dropped, so protocol drift is visible.
      if (method) effects.logs.push(`unhandled session update: ${method}`);
      return items;
    }
  }
}

/** Keep the newest rows. A chat is read from the bottom, so the front goes. */
function trimItems(items: TranscriptItem[]): { items: TranscriptItem[]; dropped: number } {
  if (items.length <= MAX_TRANSCRIPT_ITEMS) return { items, dropped: 0 };
  const dropped = items.length - MAX_TRANSCRIPT_ITEMS;
  return { items: items.slice(dropped), dropped };
}

/* The frame buffer. Module scope, so `clearSession` can drop a replay that is
 * still in flight when the user switches chats. */
let pendingUpdates: SessionUpdate[] = [];
let flushHandle: number | null = null;

function cancelFlush(): void {
  if (flushHandle === null) return;
  if (typeof cancelAnimationFrame === 'function') cancelAnimationFrame(flushHandle);
  clearTimeout(flushHandle);
  flushHandle = null;
}

function scheduleFlush(): void {
  if (flushHandle !== null) return;
  const run = () => {
    flushHandle = null;
    const batch = pendingUpdates;
    pendingUpdates = [];
    if (batch.length > 0) useApp.getState().applyUpdates(batch);
  };
  flushHandle =
    typeof requestAnimationFrame === 'function'
      ? requestAnimationFrame(run)
      : (setTimeout(run, 16) as unknown as number);
}

/** Drop anything buffered. Called when the transcript is replaced wholesale. */
function discardPending(): void {
  cancelFlush();
  pendingUpdates = [];
}

export const useApp = create<AppState>((set, get) => ({
  status: null,
  logs: [],
  sessionId: null,
  cwd: null,
  title: null,
  modes: null,
  configOptions: [],
  commands: [],
  trust: null,
  usage: null,
  account: null,
  accountLoading: false,
  items: [],
  busy: false,
  turn: null,
  openedAt: Date.now(),
  usedSkills: {},
  lastStopReason: null,
  queued: [],
  preambleSent: false,
  trimmed: 0,
  chatLoading: false,
  permission: null,
  elicitation: null,
  workstations: [],
  activeWorkstation: null,
  chats: [],
  chatsLoading: false,
  archivedChats: [],
  deletedChats: [],
  showArchived: false,
  environment: null,
  plugins: [],
  update: null,
  setup: null,
  provision: null,
  themeConfig: null,
  themeError: null,
  layoutConfig: null,
  layoutError: null,
  actionsConfig: null,
  actionsError: null,
  panelsConfig: null,
  panelsError: null,
  customizePaths: null,
  pending: { proposals: [], issues: [], receipts: [] },
  pluginSources: [],
  theme: 'ink',
  creatorMode: false,
  swarmSkills: true,
  panel: null,
  customPanel: null,
  toast: null,
  bootError: null,
  draft: '',

  /**
   * A `ready` status is not evidence that the current turn ended: the status
   * for the turn that just finished can arrive after the next one has already
   * started, and clearing `busy` on it would tear down a live turn — including
   * the `pending` flag the echo needs to fold into the right row.
   *
   * Only a dead agent is unambiguous. A stalled turn that finally answers is
   * reported through `agent://turn-ended` instead.
   */
  setStatus: (status) => {
    set({ status });
    const state = get();
    if (state.busy && (status.state === 'failed' || status.state === 'stopped')) {
      state.setBusy(false);
      state.finishStreaming();
    }
  },
  pushLog: (line) =>
    set((state) => ({ logs: [...state.logs.slice(-400), line] })),

  setSession: ({ sessionId, modes, configOptions, trust }) =>
    set((state) => ({
      sessionId,
      modes: modes ?? state.modes,
      configOptions: configOptions ?? state.configOptions,
      trust: trust ?? state.trust,
      // A different chat is a different clock. Re-stamping on the same id
      // would reset the span every time the session was re-read.
      openedAt: state.sessionId === sessionId ? state.openedAt : Date.now(),
    })),

  clearSession: () => {
    // A replay still in flight belongs to the chat being left behind; letting
    // it land would pour the old transcript into the new one.
    discardPending();
    set({
      sessionId: null,
      title: null,
      items: [],
      usage: null,
      busy: false,
      turn: null,
      openedAt: Date.now(),
      usedSkills: {},
      permission: null,
      elicitation: null,
      lastStopReason: null,
      preambleSent: false,
      trimmed: 0,
      chatLoading: false,
    });
  },

  /**
   * The turn clock is driven from here rather than from `sendPrompt`, because
   * `busy` is the one flag every path agrees on: a normal end, a cancel, a
   * stalled turn the backend finally reaped, and a dead agent all clear it.
   * Stamping the start anywhere else would leave turns that never end.
   */
  setBusy: (busy) =>
    set((state) => {
      if (busy) {
        return state.busy ? { busy } : { busy, turn: { startedAt: Date.now(), endedAt: null } };
      }
      const turn = state.turn;
      if (!turn || turn.endedAt !== null) return { busy, turn };

      const endedAt = Date.now();
      const elapsed = Math.max(0, endedAt - turn.startedAt);

      // The duration belongs to the message that started the turn, so it is
      // written onto that row. A single "last turn" slot would lose it the
      // moment the next prompt was sent.
      let items = state.items;
      for (let index = items.length - 1; index >= 0; index -= 1) {
        const item = items[index];
        if (item?.type !== 'user') continue;
        if (item.elapsed === undefined) items = replaceAt(items, index, { ...item, elapsed });
        break;
      }

      return { busy, turn: { ...turn, endedAt }, items };
    }),
  setStopReason: (lastStopReason) => set({ lastStopReason }),
  setUsage: (usage) => set({ usage }),
  setAccount: (account) => set({ account }),
  setAccountLoading: (accountLoading) => set({ accountLoading }),
  setTitle: (title) => set({ title }),
  setConfigOptions: (configOptions) => set({ configOptions }),
  setCommands: (commands) => set({ commands }),
  setWorkstations: (workstations) => set({ workstations }),
  setActiveWorkstation: (activeWorkstation) => set({ activeWorkstation }),
  setChats: (chats) => set({ chats }),
  setChatsLoading: (chatsLoading) => set({ chatsLoading }),
  setChatState: (archivedChats, deletedChats) => set({ archivedChats, deletedChats }),
  setShowArchived: (showArchived) => set({ showArchived }),
  setEnvironment: (environment) => set({ environment }),
  setPlugins: (plugins) => set({ plugins }),
  setUpdate: (update) => set({ update }),
  setSetup: (setup) => set({ setup }),
  setProvision: (provision) => set({ provision }),
  setPermission: (permission) => set({ permission }),
  setElicitation: (elicitation) => set({ elicitation }),
  // Opening or closing a panel cross-fades the panel column, not the window.
  // The snapshot is bounded to the one element that changed, so the cost is a
  // capture of one column rather than of the whole surface.
  setPanel: (panel) => transitionTo('panel', () => set({ panel, customPanel: null })),
  setCustomPanel: (customPanel) =>
    transitionTo('panel', () => set({ customPanel, panel: null })),
  setTheme: (theme) => set({ theme }),
  setCreatorMode: (creatorMode) => set({ creatorMode }),
  setSwarmSkills: (swarmSkills) => set({ swarmSkills }),
  setThemeConfig: (themeConfig) => set({ themeConfig }),
  setThemeError: (themeError) => set({ themeError }),
  setLayoutConfig: (layoutConfig) => set({ layoutConfig }),
  setLayoutError: (layoutError) => set({ layoutError }),
  setActionsConfig: (actionsConfig) => set({ actionsConfig }),
  setActionsError: (actionsError) => set({ actionsError }),
  setPanelsConfig: (panelsConfig) => set({ panelsConfig }),
  setPanelsError: (panelsError) => set({ panelsError }),
  setCustomizePaths: (customizePaths) => set({ customizePaths }),
  setPending: (pending) => set({ pending }),
  setPluginSources: (pluginSources) => set({ pluginSources }),
  setBootError: (bootError) => set({ bootError }),
  setDraft: (draft) => set({ draft }),

  notify: (message, tone = 'neutral') =>
    set({ toast: { id: uid('toast'), message, tone } }),
  dismissToast: () => set({ toast: null }),

  addUserMessage: (text, images) => {
    const id = uid('user');
    set((state) => ({
      items: [
        ...state.items,
        {
          type: 'user',
          id,
          text,
          at: Date.now(),
          pending: true,
          ...(images?.length ? { images } : {}),
        },
      ],
    }));
    return id;
  },

  dropUserMessage: (id) =>
    set((state) => ({
      items: state.items.filter((item) => !(item.type === 'user' && item.id === id)),
    })),

  enqueuePrompt: (text, images) =>
    set((state) => ({
      queued: [
        ...state.queued,
        {
          id: uid('queued'),
          text,
          sessionId: state.sessionId,
          at: Date.now(),
          ...(images?.length ? { images } : {}),
        },
      ],
    })),

  cancelQueued: (id) =>
    set((state) => ({ queued: state.queued.filter((entry) => entry.id !== id) })),

  takeQueued: (sessionId) => {
    const next = get().queued.find((entry) => entry.sessionId === sessionId);
    if (!next) return null;
    set((state) => ({ queued: state.queued.filter((entry) => entry.id !== next.id) }));
    return next;
  },

  addContextChip: (text) =>
    set((state) => ({
      items: [...state.items, { type: 'context', id: uid('ctx'), text, at: Date.now() }],
    })),

  markPreambleSent: (sent = true) => set({ preambleSent: sent }),

  addNotice: (text, level = 'info') =>
    set((state) => {
      // A retry loop fires the same notice once per attempt, so a provider
      // having a bad minute used to fill the transcript with a dozen identical
      // lines. A repeat of the notice already on screen refreshes it instead of
      // stacking another copy.
      const last = state.items[state.items.length - 1];
      if (last?.type === 'notice' && last.level === level && last.text === text) {
        return { items: replaceAt(state.items, state.items.length - 1, { ...last, at: Date.now() }) };
      }
      return {
        items: [...state.items, { type: 'notice', id: uid('notice'), level, text, at: Date.now() }],
      };
    }),

  finishStreaming: () =>
    set((state) => ({
      items: state.items.map((item) => {
        if (item.type === 'agent' || item.type === 'reasoning') {
          return item.streaming ? { ...item, streaming: false } : item;
        }
        // A user row that never got its echo must not stay matchable, or a later
        // turn's echo could be folded into it.
        if (item.type === 'user' && item.pending) {
          return { ...item, pending: false };
        }
        return item;
      }),
    })),

  applyUpdates: (updates) => {
    if (updates.length === 0) return;
    set((state) => {
      const effects: Effects = {
        logs: [],
        skills: [],
        previousMeta: state.usage?.meta ?? null,
      };
      let items = state.items;
      for (const update of updates) items = reduceItems(items, update, effects, state.busy);
      const trimmed = trimItems(items);

      const patch: Partial<AppState> = { items: trimmed.items };
      if (trimmed.dropped > 0) patch.trimmed = state.trimmed + trimmed.dropped;
      if (effects.usage) patch.usage = effects.usage;
      if (effects.title) patch.title = effects.title;
      if (effects.configOptions) patch.configOptions = effects.configOptions;
      if (effects.commands) patch.commands = effects.commands;
      if (effects.logs.length > 0) {
        const at = new Date().toISOString();
        patch.logs = [
          ...state.logs,
          ...effects.logs.map((message) => ({ level: 'debug' as const, message, at })),
        ].slice(-400);
      }
      if (effects.skills.length > 0) {
        const at = Date.now();
        const usedSkills = { ...state.usedSkills };
        for (const use of effects.skills) {
          const previous = usedSkills[use.name];
          usedSkills[use.name] = {
            name: use.name,
            bundle: use.bundle ?? previous?.bundle ?? null,
            count: (previous?.count ?? 0) + 1,
            at,
          };
        }
        patch.usedSkills = usedSkills;
      }
      return patch;
    });
  },

  enqueueUpdate: (update) => {
    pendingUpdates.push(update);
    if (pendingUpdates.length >= MAX_PENDING_UPDATES) {
      // Frames are not being served (hidden window, very long replay): drain
      // now so the buffer cannot grow without bound.
      const batch = pendingUpdates;
      pendingUpdates = [];
      cancelFlush();
      get().applyUpdates(batch);
      return;
    }
    scheduleFlush();
  },

  setChatLoading: (chatLoading) => set({ chatLoading }),
}));

export function selectModelLabel(state: AppState): string {
  const option = state.configOptions.find((o) => o.id === 'model');
  if (option && typeof option.currentValue === 'string') {
    const match = option.options?.find((v) => v.value === option.currentValue);
    return match?.name ?? option.currentValue;
  }
  return state.environment?.activeModel ?? 'default model';
}

export function selectAgentLabel(state: AppState): string {
  const option = state.configOptions.find((o) => o.id === 'mode' || o.id === 'agent');
  if (option && typeof option.currentValue === 'string') {
    const match = option.options?.find((v) => v.value === option.currentValue);
    return match?.name ?? option.currentValue;
  }
  return state.modes?.currentModeId ?? state.environment?.defaultAgent ?? 'accept-edits';
}

export type { AppState, UsageState, JsonValue };
