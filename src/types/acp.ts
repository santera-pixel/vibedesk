/**
 * Wire types for the Agent Client Protocol (ACP) as implemented by
 * `vibe-acp`, plus the VibeDesk-side view models derived from them.
 *
 * Field names mirror the wire format (camelCase) so payloads can be passed
 * through the Rust bridge without a translation layer.
 */

export type JsonValue =
  | null
  | boolean
  | number
  | string
  | JsonValue[]
  | { [key: string]: JsonValue };

/* ------------------------------------------------------------------ *
 * JSON-RPC envelope
 * ------------------------------------------------------------------ */

export interface JsonRpcError {
  code: number;
  message: string;
  data?: JsonValue;
}

export interface JsonRpcMessage {
  jsonrpc: '2.0';
  id?: number | string | null;
  method?: string;
  params?: JsonValue;
  result?: JsonValue;
  error?: JsonRpcError;
}

/* ------------------------------------------------------------------ *
 * Content blocks
 * ------------------------------------------------------------------ */

export interface TextContent {
  type: 'text';
  text: string;
}

export interface ImageContent {
  type: 'image';
  data: string;
  mimeType: string;
}

export interface ResourceLinkContent {
  type: 'resource_link';
  uri: string;
  name?: string;
  mimeType?: string;
}

export type ContentBlock = TextContent | ImageContent | ResourceLinkContent | { type: string };

/**
 * An image on its way out to the agent.
 *
 * The same shape as [`ImageContent`] minus the discriminant, because the
 * backend builds the block: the webview only knows the bytes and what they are.
 */
export interface PromptImage {
  data: string;
  mimeType: string;
}

/* ------------------------------------------------------------------ *
 * Tool calls
 * ------------------------------------------------------------------ */

export type ToolKind =
  | 'read'
  | 'edit'
  | 'delete'
  | 'move'
  | 'search'
  | 'execute'
  | 'think'
  | 'fetch'
  | 'switch_mode'
  | 'other';

export type ToolStatus = 'pending' | 'in_progress' | 'completed' | 'failed';

export interface ToolCallLocation {
  path: string;
  line?: number | null;
}

export interface DiffContent {
  type: 'diff';
  path: string;
  oldText?: string | null;
  newText: string;
}

export interface TerminalContent {
  type: 'terminal';
  terminalId: string;
}

export interface ContentToolContent {
  type: 'content';
  content: ContentBlock;
}

export type ToolCallContent = DiffContent | TerminalContent | ContentToolContent;

export interface ToolCall {
  toolCallId: string;
  title?: string;
  kind?: ToolKind;
  status?: ToolStatus;
  content?: ToolCallContent[];
  locations?: ToolCallLocation[];
  rawInput?: JsonValue;
  rawOutput?: JsonValue;
}

/* ------------------------------------------------------------------ *
 * Session updates
 * ------------------------------------------------------------------ */

export interface PlanEntry {
  content: string;
  priority: 'high' | 'medium' | 'low';
  status: 'pending' | 'in_progress' | 'completed';
}

export interface UsageCost {
  amount: number;
  currency: string;
}

/**
 * The `_meta` block `usage_update` carries alongside `used`/`size`.
 *
 * It is where the token ledger lives: `cachedTokens` is what the provider
 * served from its prompt cache, so `cachedTokens / promptTokens` is the hit
 * rate. Every field is optional because the agent omits the block entirely
 * before the first model call.
 */
export interface UsageMeta {
  steps?: number;
  promptTokens?: number;
  completionTokens?: number;
  cachedTokens?: number;
  totalTokens?: number;
  tokensPerSecond?: number;
  lastTurnDuration?: number;
  lastTurnTotalTokens?: number;
}

/** `_account/read` + `_identity/read` + `_auth/status`, shaped by `billing.rs`. */
export interface PlanSnapshot {
  status: string | null;
  title: string | null;
  name: string | null;
  kind: string | null;
  usagePercentage: number | null;
  resetAt: string | null;
  paygEnabled: boolean;
  keyPreview: string | null;
  keyScope: string | null;
  keyAccess: string | null;
}

export interface IdentitySnapshot {
  email: string | null;
  name: string | null;
  workspace: string | null;
  organization: string | null;
  authState: string | null;
  authenticated: boolean;
  signOutAvailable: boolean;
}

export interface AccountSnapshot {
  plan: PlanSnapshot;
  identity: IdentitySnapshot;
}

export interface SessionMode {
  id: string;
  name: string;
  description?: string;
}

export interface SessionModeState {
  currentModeId: string;
  availableModes: SessionMode[];
}

export interface ConfigOptionValue {
  value: string;
  name: string;
  description?: string;
}

export interface ConfigOption {
  id: string;
  name: string;
  description?: string;
  category?: string;
  type: 'select' | 'boolean' | string;
  currentValue?: string | boolean;
  options?: ConfigOptionValue[];
}

export interface AvailableCommand {
  name: string;
  description: string;
  input?: { hint?: string } | null;
}

export type SessionUpdate =
  | { sessionUpdate: 'user_message_chunk'; content: ContentBlock; messageId?: string }
  | { sessionUpdate: 'agent_message_chunk'; content: ContentBlock; messageId?: string }
  | { sessionUpdate: 'agent_thought_chunk'; content: ContentBlock; messageId?: string }
  | ({ sessionUpdate: 'tool_call' } & ToolCall)
  | ({ sessionUpdate: 'tool_call_update' } & Partial<ToolCall> & { toolCallId: string })
  | { sessionUpdate: 'plan'; entries: PlanEntry[] }
  | {
      sessionUpdate: 'usage_update';
      used: number;
      size: number;
      cost?: UsageCost | null;
      _meta?: UsageMeta | null;
    }
  | { sessionUpdate: 'session_info_update'; title?: string; updatedAt?: string }
  | { sessionUpdate: 'config_option_update'; configOptions: ConfigOption[] }
  | { sessionUpdate: 'available_commands_update'; availableCommands: AvailableCommand[] }
  | { sessionUpdate: string; [key: string]: unknown };

/* ------------------------------------------------------------------ *
 * Permissions & elicitation
 * ------------------------------------------------------------------ */

export type PermissionOptionKind = 'allow_once' | 'allow_always' | 'reject_once' | 'reject_always';

export interface PermissionOption {
  optionId: string;
  name: string;
  kind: PermissionOptionKind;
}

export interface PermissionRequest {
  requestId: string;
  sessionId: string;
  toolCall: ToolCall;
  options: PermissionOption[];
}

export interface ElicitationEnumOption {
  const?: string;
  title?: string;
}

/**
 * One field of an `elicitation/create` form. ACP restricts these to primitive
 * types plus a multi-select array, so the renderer only needs five cases.
 */
export interface ElicitationProperty {
  type: 'string' | 'number' | 'integer' | 'boolean' | 'array' | string;
  title?: string | null;
  description?: string | null;
  default?: JsonValue;
  enum?: string[] | null;
  oneOf?: ElicitationEnumOption[] | null;
  minLength?: number | null;
  maxLength?: number | null;
  pattern?: string | null;
  format?: string | null;
  minimum?: number | null;
  maximum?: number | null;
  items?: {
    type?: string;
    enum?: string[] | null;
    anyOf?: ElicitationEnumOption[] | null;
  } | null;
  minItems?: number | null;
  maxItems?: number | null;
}

export interface ElicitationSchema {
  type?: string;
  title?: string | null;
  description?: string | null;
  properties?: Record<string, ElicitationProperty>;
  required?: string[] | null;
}

export interface ElicitationRequest {
  requestId: string;
  sessionId: string;
  toolCallId?: string | null;
  message: string;
  /** `form` renders the schema; `url` sends the user to a page instead. */
  mode: 'form' | 'url' | string;
  schema: ElicitationSchema;
  url?: string | null;
}

/* ------------------------------------------------------------------ *
 * Bridge payloads (Rust -> webview)
 * ------------------------------------------------------------------ */

export interface AgentStatus {
  state: 'stopped' | 'starting' | 'ready' | 'busy' | 'failed';
  pid: number | null;
  version: string | null;
  cwd: string | null;
  sessionId: string | null;
  model: string | null;
  agent: string | null;
  binary: string | null;
  error: string | null;
  startedAt: string | null;
  restarts: number;
}

export interface SessionSnapshot {
  sessionId: string;
  modes: SessionModeState | null;
  configOptions: ConfigOption[];
  trust: TrustState | null;
}

export interface TrustState {
  trusted: boolean;
  root: string | null;
  reason?: string | null;
}

export interface SessionSummary {
  sessionId: string;
  cwd: string;
  title: string;
  updatedAt: string;
}

/**
 * A pinned project folder. The path is canonical and doubles as the key, so
 * two entries can never point at the same folder.
 */
export interface Workstation {
  path: string;
  name: string;
  addedAt: string;
}

/**
 * `PromptResponse.usage` is the ACP `Usage` shape (token counts for the turn),
 * not the `usage_update` notification shape (`used`/`size`/`cost`). The context
 * meter is driven by `usage_update`; these counts are informational.
 */
export interface PromptUsage {
  totalTokens?: number;
  inputTokens?: number;
  outputTokens?: number;
  cachedReadTokens?: number;
  used?: number;
  size?: number;
  cost?: UsageCost | null;
}

export interface PromptOutcome {
  stopReason: string;
  usage?: PromptUsage | null;
}

export interface AgentLogLine {
  level: 'debug' | 'info' | 'warn' | 'error';
  message: string;
  at: string;
}

/* ------------------------------------------------------------------ *
 * Vibe environment
 * ------------------------------------------------------------------ */

export interface ProviderRecord {
  name: string;
  apiBase: string;
  apiKeyEnvVar: string;
  backend: string;
  hasKey: boolean;
}

export interface ModelRecord {
  name: string;
  provider: string;
  alias: string;
  displayName?: string | null;
  thinking?: string | null;
  supportsImages: boolean;
  contextWindow?: number | null;
}

export interface McpServerRecord {
  name: string;
  transport: string;
  target: string;
  auth: string;
}

export interface HookRecord {
  name: string;
  event: string;
  match?: string | null;
  command: string;
  scope: 'user' | 'project' | 'plugin';
  source: string;
}

export interface VibeEnvironment {
  vibeHome: string;
  configPath: string;
  configExists: boolean;
  hooksPath: string;
  pluginsDirs: string[];
  activeModel: string | null;
  defaultAgent: string | null;
  theme: string | null;
  providers: ProviderRecord[];
  models: ModelRecord[];
  mcpServers: McpServerRecord[];
  hooks: HookRecord[];
  configIssues: string[];
  cliVersion: string | null;
  acpBinary: string | null;
  acpBinarySource: string;
  apiKeyPresent: boolean;
  /** Where VibeDesk stores its own settings (portable installs keep it local). */
  dataDir: string;
}

/* ------------------------------------------------------------------ *
 * Plugins
 * ------------------------------------------------------------------ */

export interface PluginComponent {
  kind: 'skill' | 'mcp' | 'hook' | 'agent' | 'knowledge' | 'connector' | 'library';
  name: string;
  detail?: string | null;
}

export interface PluginRecord {
  name: string;
  displayName: string | null;
  version: string | null;
  description: string | null;
  author: string | null;
  license: string | null;
  scope: 'user' | 'project';
  path: string;
  format: 'native' | 'claude' | 'codex' | 'kimi' | 'opencode' | 'unknown';
  enabled: boolean;
  defaultEnabled: boolean;
  toolNamespace: string | null;
  components: PluginComponent[];
  issues: string[];
  digest: string | null;
}

export interface PluginReloadReport {
  added: string[];
  removed: string[];
  updated: string[];
  unchanged: number;
  repinned: boolean;
  plugins: PluginRecord[];
}

/* ------------------------------------------------------------------ *
 * Updates
 * ------------------------------------------------------------------ */

export interface UpdateStatus {
  currentVersion: string | null;
  latestVersion: string | null;
  updateAvailable: boolean;
  channel: string;
  checkedAt: string | null;
  installMethod: string;
  upgradeCommand: string | null;
  releaseUrl: string | null;
  notes: string | null;
  error: string | null;
  applying: boolean;
}

/* ------------------------------------------------------------------ *
 * Workspace
 * ------------------------------------------------------------------ */

export interface FileEntry {
  path: string;
  name: string;
  isDir: boolean;
}

/* ------------------------------------------------------------------ *
 * First-run setup
 * ------------------------------------------------------------------ */

export interface InstallPlan {
  method: 'uv' | 'pipx' | 'pip' | string;
  program: string;
  args: string[];
  command: string;
}

export interface InstallStatus {
  binaryFound: boolean;
  binaryPath: string | null;
  binarySource: string;
  uvPath: string | null;
  pipxPath: string | null;
  pythonPath: string | null;
  plan: InstallPlan | null;
  docsUrl: string;
}

export interface InstallOutcome {
  ok: boolean;
  code: number | null;
  output: string;
  binaryPath: string | null;
  status: InstallStatus;
}

/**
 * One provisioning step, as the setup screen renders it.
 *
 * `state` is the ledger's word, not a boolean: `skipped` means the step never
 * ran because something it needs is absent (no npm, no git), which is a
 * different thing from `failed` and is the only one worth a retry.
 */
export type ProvisionState = 'absent' | 'installing' | 'ready' | 'failed' | 'skipped';

export interface ProvisionRow {
  id: string;
  title: string;
  state: ProvisionState;
  version: string | null;
  detail: string;
  blocked: string | null;
  retryable: boolean;
}

export interface ProvisionStatus {
  rows: ProvisionRow[];
  /** Set when the ledger on disk could not be read, so the panel can say so. */
  note: string | null;
}

/* ------------------------------------------------------------------ *
 * Skills & release notes
 * ------------------------------------------------------------------ */

export interface SkillRecord {
  name: string;
  description: string;
  /**
   * `mastery` is a bundle cloned under `~/.vibe/mastery/sources`, which is
   * neither a project skill nor a plugin skill: it is a whole pack of skills
   * the user pulled in, and `plugin` carries the bundle's label.
   */
  scope: 'project' | 'user' | 'plugin' | 'builtin' | 'mastery';
  source: string;
  plugin: string | null;
  userInvocable: boolean;
  enabled: boolean;
}

export interface ReleaseNote {
  version: string;
  body: string;
}
