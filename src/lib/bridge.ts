/**
 * Typed access to the Rust bridge.
 *
 * Every call maps 1:1 to a `#[tauri::command]` in `src-tauri/src/commands`.
 * The webview never speaks JSON-RPC directly: the Rust side owns the
 * `vibe-acp` child process, correlates requests, and re-emits agent
 * notifications as Tauri events.
 */
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import type {
  AccountSnapshot,
  AgentLogLine,
  AgentStatus,
  ElicitationRequest,
  FileEntry,
  InstallOutcome,
  InstallStatus,
  JsonValue,
  PermissionRequest,
  PluginRecord,
  PluginReloadReport,
  PromptImage,
  PromptOutcome,
  ProvisionStatus,
  ReleaseNote,
  SessionSnapshot,
  SessionSummary,
  SkillRecord,
  UpdateStatus,
  VibeEnvironment,
  Workstation,
} from '@/types/acp';
import type {
  ActionsConfig,
  ConfigPayload,
  CustomizePaths,
  LayoutConfig,
  ConfigKind,
  PanelsConfig,
  PendingState,
  PluginSource,
  ThemeConfig,
} from '@/lib/customize';
import type { ConsentEntry, DataEvent, DataPlaneStatus } from '@/lib/data';
import type {
  CapabilityRecord,
  CapabilitySettings,
  CodeGraphGraph,
  CodeGraphState,
  ChannelStatus,
  CompactionStatus,
  DreamResult,
  MemorySession,
  MemoryStatus,
  ReadingProbe,
  ServerStatus,
  TerminalRules,
} from '@/lib/capabilities';
import type {
  Plan,
  Precondition,
  RehearsalView,
  Catalog,
  CrewView,
  Skill,
  SkillMatch,
  ThinkingByRole,
  Run,
  SwarmSnapshot,
} from '@/lib/swarm';

export const EVENTS = {
  notification: 'acp://notification',
  request: 'acp://request',
  status: 'agent://status',
  log: 'agent://log',
  turnEnded: 'agent://turn-ended',
  config: 'config://changed',
  pending: 'config://pending',
  panelData: 'panel://data',
  swarm: 'swarm://changed',
} as const;

export function hasBackend(): boolean {
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
}

async function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  if (!hasBackend()) {
    throw new Error(
      'VibeDesk is running outside the Tauri shell. Start it with `npm run tauri:dev`.',
    );
  }
  return invoke<T>(command, args);
}

/* ---------------------------------------------------------------- *
 * Agent lifecycle
 * ---------------------------------------------------------------- */

export const agent = {
  status: () => call<AgentStatus>('agent_status'),
  start: (cwd?: string) => call<AgentStatus>('agent_start', { cwd: cwd ?? null }),
  stop: () => call<void>('agent_stop'),
  restart: (cwd?: string) => call<AgentStatus>('agent_restart', { cwd: cwd ?? null }),
  setWorkspace: (cwd: string) => call<void>('agent_set_workspace', { cwd }),
};

/* ---------------------------------------------------------------- *
 * Sessions
 * ---------------------------------------------------------------- */

export const session = {
  create: (cwd: string) => call<SessionSnapshot>('session_new', { cwd }),
  load: (cwd: string, sessionId: string) =>
    call<SessionSnapshot>('session_load', { cwd, sessionId }),
  prompt: (sessionId: string, text: string, images?: PromptImage[]) =>
    call<PromptOutcome>('session_prompt', { sessionId, text, images: images ?? null }),
  cancel: (sessionId: string) => call<void>('session_cancel', { sessionId }),
  list: (cwd?: string) => call<SessionSummary[]>('session_list', { cwd: cwd ?? null }),
  setMode: (sessionId: string, modeId: string) =>
    call<void>('session_set_mode', { sessionId, modeId }),
  setConfigOption: (sessionId: string, configId: string, value: JsonValue) =>
    call<void>('session_set_config_option', { sessionId, configId, value }),
  setTitle: (sessionId: string, title: string) =>
    call<void>('session_set_title', { sessionId, title }),
  fork: (sessionId: string) => call<SessionSnapshot>('session_fork', { sessionId }),
};

/* ---------------------------------------------------------------- *
 * Chats — archive and delete
 * ---------------------------------------------------------------- */

export interface ChatsState {
  archived: string[];
  deleted: string[];
}

/** One chat a bulk call could not move, and why. */
export interface BulkFailure {
  sessionId: string;
  error: string;
}

/**
 * What a bulk call did.
 *
 * Every id lands in exactly one of `done`, `skipped` or `failed`. A bulk call
 * that reported only success would be lying about the one case the user needs
 * to know about.
 */
export interface BulkDelete {
  state: ChatsState;
  done: string[];
  skipped: string[];
  failed: BulkFailure[];
}

export const chats = {
  state: () => call<ChatsState>('chats_state'),
  archive: (sessionId: string, archived: boolean) =>
    call<ChatsState>('chats_archive', { sessionId, archived }),
  remove: (sessionId: string) => call<ChatsState>('chats_delete', { sessionId }),
  archiveMany: (ids: string[], archived: boolean) =>
    call<BulkDelete>('chats_archive_many', { ids, archived }),
  removeMany: (ids: string[]) => call<BulkDelete>('chats_delete_many', { ids }),
};

/* ---------------------------------------------------------------- *
 * Workstations
 * ---------------------------------------------------------------- */

export const workstations = {
  list: () => call<Workstation[]>('workstations_list'),
  add: (path: string) => call<Workstation[]>('workstation_add', { path }),
  remove: (path: string) => call<Workstation[]>('workstation_remove', { path }),
  rename: (path: string, name: string) =>
    call<Workstation[]>('workstation_rename', { path, name }),
};

/* ---------------------------------------------------------------- *
 * Interactive requests raised by the agent
 * ---------------------------------------------------------------- */

export const interaction = {
  respondPermission: (requestId: string, optionId: string | null) =>
    call<void>('respond_permission', { requestId, optionId }),
  respondElicitation: (requestId: string, action: string, content?: JsonValue) =>
    call<void>('respond_elicitation', { requestId, action, content: content ?? null }),
};

/* ---------------------------------------------------------------- *
 * Vibe environment
 * ---------------------------------------------------------------- */

export const vibe = {
  environment: () => call<VibeEnvironment>('vibe_environment'),
  configSchema: () => call<JsonValue>('vibe_config_schema'),
  authStatus: () => call<JsonValue>('vibe_auth_status'),
  whoami: () => call<JsonValue>('vibe_whoami'),
  account: () => call<AccountSnapshot>('vibe_account'),
  logLevel: (sessionId: string, level: string) =>
    call<JsonValue>('vibe_log_level', { sessionId, level }),
  releaseNotes: () => call<ReleaseNote[]>('vibe_release_notes'),
  skills: () => call<SkillRecord[]>('vibe_skills'),
};

/* ---------------------------------------------------------------- *
 * Plugins
 * ---------------------------------------------------------------- */

export const plugins = {
  list: () => call<PluginRecord[]>('plugins_list'),
  reload: (repin = true) => call<PluginReloadReport>('plugins_reload', { repin }),
  setEnabled: (name: string, enabled: boolean) =>
    call<PluginRecord[]>('plugins_set_enabled', { name, enabled }),
};

/* ---------------------------------------------------------------- *
 * Workspace
 * ---------------------------------------------------------------- */

export const workspace = {
  files: (query: string, limit = 40) => call<FileEntry[]>('workspace_files', { query, limit }),
  read: (path: string) => call<string>('workspace_read', { path }),
  applyEdit: (path: string, content: string) =>
    call<void>('workspace_apply_edit', { path, content }),
  reveal: (path: string) => call<void>('workspace_reveal', { path }),
  /** Read an image off disk as base64, for the composer's attach button. */
  imageRead: (path: string) => call<PromptImage>('image_read', { path }),
};

/* ---------------------------------------------------------------- *
 * First-run setup
 * ---------------------------------------------------------------- */

export const setup = {
  status: () => call<InstallStatus>('vibe_install_status'),
  install: () => call<InstallOutcome>('vibe_install'),
};

/**
 * Provisioning: everything a fresh machine needs beyond the CLI itself.
 *
 * Separate from `setup` because the two answer different questions. `setup` is
 * "is the agent installed"; this is "will a swarm run work" — the agents the
 * lead and workers run as, the method pack, the capability servers. A machine
 * can pass the first and fail the second, which is exactly the state that made
 * the lead implement instead of plan.
 */
export const provision = {
  status: () => call<ProvisionStatus>('provision_status'),
  run: () => call<{ records: unknown[] }>('provision_run'),
  runStep: (id: string) => call<{ record: unknown }>('provision_run_step', { id }),
};

/**
 * The CDP endpoint, when the app was started with one.
 *
 * Reported so the app can say it is on. A debugging port lets any local process
 * drive this window, and a capability the user cannot see is one they cannot
 * reason about.
 */
export const debug = {
  endpoint: () => call<number | null>('debug_endpoint'),
};

/**
 * What this binary was built from.
 *
 * The frontend is embedded at compile time, so the source tree cannot say which
 * UI is running — only the binary can. `distStale` is true when `dist/` was
 * older than the frontend sources at build time, which is the state where the
 * app looks broken and nothing else reports it.
 */
export interface BuildInfo {
  version: string;
  bundle: string;
  distStale: boolean;
}

export const build = {
  info: () => call<BuildInfo>('build_info'),
};

/* ---------------------------------------------------------------- *
 * Runtime customization
 * ---------------------------------------------------------------- */

export const dataPlane = {
  status: () => call<DataPlaneStatus>('data_status'),
  setPaused: (paused: boolean) => call<DataPlaneStatus>('data_set_paused', { paused }),
  /**
   * Every origin the current config wants, with the user's decision.
   *
   * Derived from what the plane is doing rather than from the panel file, so a
   * source that failed to parse never appears — it is not asking.
   */
  consentList: () => call<ConsentEntry[]>('data_consent_list'),
  /**
   * Grant, refuse, or forget an origin. `null` puts it back to asking.
   *
   * The backend refuses anything that is not loopback, so this cannot widen a
   * panel's reach; it only answers a question the panel already asked.
   */
  consentSet: (origin: string, decision: 'allow' | 'deny' | null) =>
    call<DataPlaneStatus>('data_consent_set', { origin, decision }),
};

export const customize = {
  paths: () => call<CustomizePaths>('customize_paths'),
  getTheme: () => call<ThemeConfig>('theme_get'),
  setTheme: (theme: ThemeConfig) => call<ThemeConfig>('theme_set', { theme }),
  resetTheme: () => call<ThemeConfig>('theme_reset'),
  getLayout: () => call<LayoutConfig>('layout_get'),
  setLayout: (layout: LayoutConfig) => call<LayoutConfig>('layout_set', { layout }),
  resetLayout: () => call<LayoutConfig>('layout_reset'),
  getActions: () => call<ActionsConfig>('actions_get'),
  setActions: (actions: ActionsConfig) => call<ActionsConfig>('actions_set', { actions }),
  resetActions: () => call<ActionsConfig>('actions_reset'),
  getPanels: () => call<PanelsConfig>('panels_get'),
  setPanels: (panels: PanelsConfig) => call<PanelsConfig>('panels_set', { panels }),
  resetPanels: () => call<PanelsConfig>('panels_reset'),
  /**
   * Why a plugin's panels did not load.
   *
   * A plugin panel is read on every load rather than listed for approval, so a
   * broken one has no other place to say so: without this the panel simply is
   * not there and the user has no way to tell a typo from a plugin that ships
   * no panels.
   */
  pluginIssues: () => call<string[]>('panels_plugin_issues'),
};

/* ---------------------------------------------------------------- *
 * Sources
 *
 * The frontend names a plugin and a kind, never a path: the backend
 * re-discovers, so the webview cannot hand in an arbitrary file to be
 * read and promoted into the live config.
 * ---------------------------------------------------------------- */

export const sources = {
  pending: () => call<PendingState>('sources_pending'),
  apply: (kind: ConfigKind, digest: string) => call<void>('sources_apply', { kind, digest }),
  reject: (kind: ConfigKind) => call<void>('sources_reject', { kind }),
  plugins: () => call<PluginSource[]>('sources_plugins'),
  applyPlugin: (plugin: string, kind: ConfigKind, digest: string) =>
    call<void>('sources_apply_plugin', { plugin, kind, digest }),
};

/* ---------------------------------------------------------------- *
 * VibeDesk's own preferences
 * ---------------------------------------------------------------- */

export interface Prefs {
  creatorMode: boolean;
  /**
   * Whether a swarm run is handed the installed skills.
   *
   * On by default, and the default is on the Rust side too: a body that predates
   * the field must not read as "off", or the first unrelated preference write
   * would switch the feature off for every existing user.
   */
  swarmSkills: boolean;
}

export const prefs = {
  get: () => call<Prefs>('prefs_get'),
  set: (value: Prefs) => call<Prefs>('prefs_set', { prefs: value }),
};

/* ---------------------------------------------------------------- *
 * Updates
 * ---------------------------------------------------------------- */

export const updates = {
  check: (force = false) => call<UpdateStatus>('update_check', { force }),
  apply: () => call<UpdateStatus>('update_apply'),
};

/* ---------------------------------------------------------------- *
 * Capabilities
 * ---------------------------------------------------------------- */

/**
 * The six upstream projects VibeDesk owns, as one ledger.
 *
 * `snapshot` is the only read the panels poll: six separate reads could
 * disagree with each other mid-render, and the ledger's whole job is that the
 * totals add up. Everything else is an action on that ledger.
 */
export const capabilities = {
  snapshot: () => call<CapabilityRecord[]>('capabilities_snapshot'),
  setEnabled: (id: string, enabled: boolean) =>
    call<CapabilityRecord[]>('capabilities_set_enabled', { id, enabled }),
  settings: () => call<CapabilitySettings>('capabilities_settings_get'),
  saveSettings: (next: CapabilitySettings) =>
    call<CapabilitySettings>('capabilities_settings_set', { next }),
  reset: (id: string) => call<CapabilityRecord[]>('capabilities_reset', { id }),

  compaction: () => call<CompactionStatus>('capabilities_compaction_status'),

  memory: () => call<MemoryStatus>('capabilities_memory_status'),
  dream: (sessions: MemorySession[]) =>
    call<DreamResult>('capabilities_memory_dream', { sessions }),
  forget: (id: string) => call<MemoryStatus>('capabilities_memory_forget', { id }),

  external: () => call<ServerStatus[]>('capabilities_external_status'),
  register: (name: string) => call<ServerStatus[]>('capabilities_external_register', { name }),
  unregister: (name: string) =>
    call<ServerStatus[]>('capabilities_external_unregister', { name }),

  codegraph: (refresh = false) =>
    call<CodeGraphState>('capabilities_codegraph', { refresh }),

  codegraphGraph: (project: string, refresh = false) =>
    call<CodeGraphGraph>('capabilities_codegraph_graph', { project, refresh }),

  channelStatus: () => call<ChannelStatus>('capabilities_channel_status'),

  terminalRules: () => call<TerminalRules>('capabilities_terminal_rules'),
  readingProbe: (path: string) => call<ReadingProbe>('capabilities_reading_probe', { path }),
};

/* ---------------------------------------------------------------- *
 * Swarm
 * ---------------------------------------------------------------- */

/**
 * What the user chose about the crew, sent with every command that assigns one.
 *
 * Empty means "you decide": an empty model falls back to the policy's pick, an
 * empty level to the role's default. That is why every field is a string rather
 * than an option — the engine's `CrewChoice` is the same shape, and a `null`
 * would have to be translated at the boundary for no gain.
 *
 * The catalogue travels beside this, not inside it, because the window is the
 * only side that holds the agent's real model list — `session/new` returns it as
 * `configOptions`, and the Rust side has no session to ask. Passing it makes the
 * assignment the agent's list rather than `config.toml`'s, which is the
 * difference between six models and two.
 */
export interface CrewChoice {
  /** A model for the lead, or empty to share the worker's. */
  lead: string;
  /** A model for the workers, or empty to use the active model. */
  worker: string;
  /** A model for the verifiers, or empty to take the first different one. */
  verifier: string;
  /** The reasoning level per role, or empty for the role's default. */
  thinking: ThinkingByRole;
}

/** Nothing chosen: the engine's own policy decides every role. */
export const NO_CREW: CrewChoice = {
  lead: '',
  worker: '',
  verifier: '',
  thinking: { lead: '', worker: '', verifier: '' },
};

/**
 * The orchestrator, as the panel drives it.
 *
 * `snapshot` is the only read the panel polls, for the same reason the
 * capability ledger has one: a run assembled from several round trips can
 * disagree with itself, and this panel's whole job is to be the single true
 * account of a run.
 *
 * `rehearse` is deliberately separate from `start`. It schedules and costs a
 * plan with no processes and no spend, which is what lets the user decide
 * whether 15x the tokens of a chat is worth it for *this* plan before committing.
 */
export const swarm = {
  /**
   * The whole panel, in one read.
   *
   * `selected` is the member the panel is showing in detail. It travels with the
   * read rather than being remembered by the engine because it is a fact about
   * the window: the feed is two hundred items, and sending one for every member
   * of a twenty-one member run every two seconds would be four thousand items
   * per poll for a pane that can only show one.
   */
  snapshot: (selected?: string | null) =>
    call<SwarmSnapshot>('swarm_snapshot', { selected: selected ?? null }),
  rehearse: (plan: Plan, concurrency?: number) =>
    call<RehearsalView>('swarm_rehearse', { plan, concurrency: concurrency ?? null }),
  rehearseExample: (concurrency?: number) =>
    call<RehearsalView>('swarm_rehearse_example', { concurrency: concurrency ?? null }),
  examplePlan: () => call<Plan>('swarm_example_plan'),
  start: (
    goal: string,
    concurrency?: number,
    budgetTokens?: number,
    catalog?: Catalog,
    crew?: CrewChoice,
  ) =>
    call<Run>('swarm_start', {
      goal,
      concurrency: concurrency ?? null,
      budgetTokens: budgetTokens ?? null,
      catalog: catalog ?? null,
      crew: crew ?? null,
    }),
  approve: (plan: Plan, concurrency?: number, catalog?: Catalog, crew?: CrewChoice) =>
    call<Run>('swarm_approve', {
      plan,
      concurrency: concurrency ?? null,
      catalog: catalog ?? null,
      crew: crew ?? null,
    }),
  stop: () => call<Run>('swarm_stop'),
  /**
   * Ask the process driving an adopted run to stop it.
   *
   * A request, not a stop: the answer is the run as it stands, and the run's own
   * state is the only thing that can say whether the driver acted.
   */
  requestStop: () => call<Run>('swarm_request_stop'),
  remove: (id: string) => call<void>('swarm_delete', { id }),
  open: (id: string) => call<Run>('swarm_open', { id }),
  preconditions: (catalog?: Catalog) =>
    call<Precondition[]>('swarm_preconditions', { catalog: catalog ?? null }),
  models: (catalog?: Catalog, crew?: CrewChoice) =>
    call<CrewView>('swarm_models', {
      catalog: catalog ?? null,
      crew: crew ?? null,
    }),
  /** The agent's own model list, probed once per `config.toml` revision. */
  catalog: () => call<Catalog>('swarm_catalog'),
  /**
   * The installed skills, and what a run with this goal would be handed.
   *
   * The ranking is the same function the engine calls, so the preview is the
   * run's own decision rather than a second opinion about it.
   */
  skills: (goal?: string) =>
    call<SkillPreview>('swarm_skills', { goal: goal ?? null }),
  /**
   * Install a skill the reflection pass proposed.
   *
   * The one write in this feature that reaches the user's own skill store, and
   * it is a click rather than a side effect. The body is read from the run's
   * directory on the Rust side, never from here.
   */
  acceptSkill: (runId: string, name: string) =>
    call<{ name: string; path: string }>('swarm_skill_accept', { runId, name }),
};

/** What `swarm_skills` answers: the catalogue, the ranking, and the switch. */
export interface SkillPreview {
  catalogue: { skills: Skill[] };
  ranked: SkillMatch[];
  enabled: boolean;
}

/* ---------------------------------------------------------------- *
 * Event subscriptions
 * ---------------------------------------------------------------- */

export interface AcpNotification {
  method: string;
  params: JsonValue;
}

export interface AcpRequest {
  requestId: string;
  method: string;
  params: JsonValue;
}

export function onNotification(handler: (n: AcpNotification) => void): Promise<UnlistenFn> {
  return listen<AcpNotification>(EVENTS.notification, (e) => handler(e.payload));
}

export function onRequest(handler: (r: AcpRequest) => void): Promise<UnlistenFn> {
  return listen<AcpRequest>(EVENTS.request, (e) => handler(e.payload));
}

export function onStatus(handler: (s: AgentStatus) => void): Promise<UnlistenFn> {
  return listen<AgentStatus>(EVENTS.status, (e) => handler(e.payload));
}

export function onLog(handler: (l: AgentLogLine) => void): Promise<UnlistenFn> {
  return listen<AgentLogLine>(EVENTS.log, (e) => handler(e.payload));
}

/**
 * The backend closed a turn the webview had already stopped waiting for — a
 * stalled turn that finally answered. A `ready` status cannot carry this,
 * because a `ready` from the *previous* turn can arrive after the next one has
 * already started.
 */
export function onTurnEnded(handler: (payload: { reason: string }) => void): Promise<UnlistenFn> {
  return listen<{ reason: string }>(EVENTS.turnEnded, (e) => handler(e.payload));
}

export function onConfigChanged(handler: (p: ConfigPayload) => void): Promise<UnlistenFn> {
  return listen<ConfigPayload>(EVENTS.config, (e) => handler(e.payload));
}

export function onPendingChanged(handler: (p: PendingState) => void): Promise<UnlistenFn> {
  return listen<PendingState>(EVENTS.pending, (e) => handler(e.payload));
}

/** One source's latest answer, or the error that replaced it. */
export function onPanelData(handler: (e: DataEvent) => void): Promise<UnlistenFn> {
  return listen<DataEvent>(EVENTS.panelData, (e) => handler(e.payload));
}

/**
 * A run moved.
 *
 * The payload is a summary, not the run: the panel re-reads the snapshot rather
 * than patching from an event, so a dropped event cannot leave it showing a
 * state the engine is no longer in.
 */
export function onSwarm(handler: (p: unknown) => void): Promise<UnlistenFn> {
  return listen<unknown>(EVENTS.swarm, (e) => handler(e.payload));
}

export type { PermissionRequest, ElicitationRequest };
