/**
 * The capability layer, as the panels see it.
 *
 * These types mirror `src-tauri/src/capabilities` field for field. They are
 * hand-written rather than generated because the Rust side serialises with
 * `rename_all = "camelCase"` and a few `serde_json::Value` blobs, and a
 * generator would either lose the blob shapes or invent fields the commands do
 * not send. When a command changes, this file changes with it — that is the
 * point of keeping it small.
 *
 * Nothing here calls the bridge. The namespace lives in `bridge.ts` with every
 * other command family, so there is still exactly one module that owns
 * `invoke`.
 */
import { formatTokens } from '@/lib/utils';

/* ---------------------------------------------------------------- *
 * The ledger
 * ---------------------------------------------------------------- */

/**
 * The drawers the capability layer owns.
 *
 * Declared here rather than in the store because this module is what mirrors
 * `CapabilityId::panel()` on the Rust side: the registry decides which
 * capabilities have a panel, and this is the same list on the other end of the
 * wire. Every capability has one — a capability the user cannot open is a
 * capability they cannot judge.
 */
export type CapabilityPanelId =
  | 'capabilities'
  | 'compaction'
  | 'terminal'
  | 'reading'
  | 'memory'
  | 'code-graph'
  | 'web'
  | 'channeler';

/** One thing a capability did, kept for its panel. */
export interface CapabilityEvent {
  at: string;
  /** A rule id, a block id, a gate decision — the capability's own vocabulary. */
  label: string;
  detail: string;
  bytesIn: number;
  bytesOut: number;
  /**
   * The same event in tokens, or zero when the capability has no token figure.
   *
   * Compaction works in tokens and reports the exact count; the terminal and
   * the reading gate measure characters and report the same four-per-token
   * estimate the kernel uses for its own thresholds. Memory and the external
   * servers report zero, and the panel shows bytes for those rather than a
   * number nobody measured.
   */
  tokensIn: number;
  tokensOut: number;
}

export interface CapabilityStats {
  calls: number;
  bytesIn: number;
  bytesOut: number;
  /** Zero when no event in this capability reported a token figure. */
  tokensIn: number;
  tokensOut: number;
  /** Named counters: rules fired, blocks created, memories kept. */
  counters: Record<string, number>;
  lastAt: string | null;
  /** Newest first, capped at twenty by the registry. */
  recent: CapabilityEvent[];
}

export interface CapabilityRecord {
  id: string;
  name: string;
  repo: string;
  summary: string;
  /** The boundary it attaches to, e.g. `terminal/output`. */
  boundary: string;
  /** The panel that renders it. Never absent: every capability has one. */
  panel: CapabilityPanelId;
  enabled: boolean;
  /** False when a precondition failed. Null means there is none to check. */
  ready: boolean | null;
  blocked: string | null;
  stats: CapabilityStats;
}

/* ---------------------------------------------------------------- *
 * Settings
 * ---------------------------------------------------------------- */

export interface CompactionSettings {
  contextLimit: number;
  nudgeThreshold: number;
  truncateThreshold: number;
  preserveRecent: number;
  protectedTools: string[];
  ccr: boolean;
  ccrMinTokens: number;
  /** Pending tokens at or above which the nudge asks for a compression. */
  minCompressRange: number;
  /** Pending tokens at or above which it asks for a distillation instead. */
  minDistillRange: number;
  upstreamProvider: string;
  proxyPort: number;
}

export interface TerminalSettings {
  maxInlineChars: number;
  disabledRules: string[];
  passthroughCommands: string[];
}

export interface ReadingSettings {
  documentFloor: number;
  collectionFloor: number;
  /** `auto`, `force` or `native`. */
  mode: string;
}

export interface MemorySettings {
  maxMemories: number;
  minSessions: number;
  injectRecall: boolean;
}

export interface CapabilitySettings {
  enabled: Record<string, boolean>;
  compaction: CompactionSettings;
  terminal: TerminalSettings;
  reading: ReadingSettings;
  memory: MemorySettings;
}

/* ---------------------------------------------------------------- *
 * Compaction
 * ---------------------------------------------------------------- */

/** The kernel's view of one conversation the proxy is holding. */
export interface KernelStatus {
  tokens: number;
  limit: number;
  usage: number;
  activeBlocks: number;
  totalBlocks: number;
  /** Active blocks per tier. Keys are tier numbers as strings, because JSON. */
  tierCounts: Record<string, number>;
  tokensCompressed: number;
  compressionCount: number;
  storedCount: number;
  retrievalCount: number;
  nudgesShown: number;
}

export interface KernelSession {
  /** The fingerprint the proxy keys the conversation by. */
  session: string;
  status: KernelStatus;
}

export interface ProviderRegistration {
  registered: boolean;
  provider: string;
  models: string[];
  /**
   * The mirrored models with the display name the agent will show for them.
   *
   * The agent captures its model list at session start and does not announce a
   * new one when it re-reads `config.toml`, so the picker has to be told about
   * these directly or the routed model is selectable but invisible.
   */
  mirrored: { alias: string; displayName: string | null }[];
  activeModel: string | null;
}

export interface CompactionStatus {
  port: number | null;
  listening: boolean;
  upstreamProvider: string;
  registration: ProviderRegistration;
  sessions: KernelSession[];
  config: {
    contextLimit: number;
    nudgeThreshold: number;
    truncateThreshold: number;
    preserveRecent: number;
    ccr: boolean;
    ccrMinTokens: number;
    /** Pending tokens at or above which the nudge asks for a compression. */
    minCompressRange: number;
    /** Pending tokens at or above which it asks for a distillation instead. */
    minDistillRange: number;
  };
}

/* ---------------------------------------------------------------- *
 * Memory
 * ---------------------------------------------------------------- */

export type MemoryType = 'user' | 'feedback' | 'project' | 'reference';

export interface MemoryHeader {
  id: string;
  name: string;
  description: string;
  type: MemoryType;
  updatedAt: number;
}

export interface MemoryGate {
  ready: boolean;
  reason: string;
  sessionsSince: number;
  hoursSince: number;
}

export interface MemoryStatus {
  root: string;
  count: number;
  byType: Record<string, number>;
  gate: MemoryGate;
  memories: MemoryHeader[];
}

export interface MemoryMessage {
  role: string;
  content: string;
}

export interface MemorySession {
  id: string;
  messages: MemoryMessage[];
}

export interface DreamResult {
  success: boolean;
  phase: string;
  memoriesScanned: number;
  memoriesCreated: number;
  memoriesUpdated: number;
  memoriesPruned: number;
  durationMs: number;
  summary: string;
  error: string | null;
}

/* ---------------------------------------------------------------- *
 * External MCP servers
 * ---------------------------------------------------------------- */

export interface ServerStatus {
  name: string;
  binary: string;
  /** The capability key this server provides. */
  capability: string;
  gives: string;
  path: string | null;
  version: string | null;
  registered: boolean;
  /** Whether VibeDesk hands it to the agent on `session/new`. */
  declared: boolean;
  blocked: string | null;
}

/** A repository the code graph server has indexed. */
export interface CodeGraphProject {
  name: string;
  root: string;
  branch: string;
  /** Present when the server answered for this project. */
  nodes?: number;
  edges?: number;
  indexedAt?: string;
}

/** One project's index, as the server reports it. */
export interface CodeGraphIndex {
  project: string;
  nodes: number;
  edges: number;
  status: string;
  root: string;
  indexedAt: string | null;
  /** Files the parser could only partly read. */
  parsePartial: number;
  /** Files the parser could not read at all. */
  parseUnusable: number;
  skipped: number;
  /** Directories deliberately left out of the graph. */
  notIndexedDirs: number;
  /** Files deliberately left out of the graph. */
  notIndexedFiles: number;
  watching: boolean;
}

/** Everything the code graph panel needs, in one round trip. */
export interface CodeGraphState {
  available: boolean;
  reason: string | null;
  workspace: string | null;
  /** Whether the workspace is itself one of the indexed projects. */
  workspaceIndexed: boolean;
  /** Indexed projects whose root sits inside the workspace. */
  contained: CodeGraphProject[];
  projects: CodeGraphProject[];
  current: CodeGraphIndex | null;
}

/** One node in the code graph. */
export interface CodeGraphNode {
  /** The qualified name, which is also the key edges refer to. */
  id: string;
  name: string;
  /** The first node label: `Function`, `File`, `Module`, and so on. */
  label: string;
  file: string;
  line: number | null;
  complexity: number | null;
  importance: number | null;
  entry: boolean;
  exported: boolean;
  test: boolean;
  lines: number | null;
  /** How many edges touch this node, in either direction. */
  degree: number;
}

/** One edge, by qualified name. */
export interface CodeGraphEdge {
  source: string;
  target: string;
  /** `CALLS`, `DEFINES`, `IMPORTS`, and so on. */
  kind: string;
}

/** A label or edge type with its exact count. */
export interface CodeGraphLabelCount {
  label: string;
  count: number;
}

/** A whole project's graph, ready to draw. */
export interface CodeGraphGraph {
  project: string;
  nodes: CodeGraphNode[];
  edges: CodeGraphEdge[];
  /** Every node label the index holds, with its exact count. */
  labels: CodeGraphLabelCount[];
  /** Every edge type the index holds, with its exact count. */
  kinds: CodeGraphLabelCount[];
  /** The index's own totals, which are exact even when the fetch is not. */
  totalNodes: number;
  totalEdges: number;
  /** Whether the fetch stopped before it had read the whole graph. */
  truncated: boolean;
  /** What was read, when it was not everything. */
  truncation: string | null;
}

/* ---------------------------------------------------------------- *
 * Terminal rules
 * ---------------------------------------------------------------- */

export interface RuleFamily {
  family: string;
  id: string;
  description: string;
}

export interface TerminalRules {
  count: number;
  families: RuleFamily[];
  fallback: string;
}

/* ---------------------------------------------------------------- *
 * Reading
 * ---------------------------------------------------------------- */

export type ReadingMode = 'force' | 'advisory' | 'native';

export interface ReadingDecision {
  mode: ReadingMode;
  reason: string;
  confidence: number;
  recommendedMode: string;
  code: string;
  previewRecommended: boolean;
  scopeKind: string;
}

export interface ReadingSample {
  anchor: string;
  text: string;
}

export interface ReadingSignal {
  kind: string;
  detail: string;
}

/**
 * The card's structure, tagged by `shape`.
 *
 * `unsupported` is a real member, not a fallback: the port reports a shape it
 * cannot preview rather than pretending to have one.
 */
export type ReadingStructure =
  | { shape: 'text'; unitCount: number; headings: string[] }
  | { shape: 'log'; lineCount: number; levels: Record<string, number>; repeated: string[] }
  | {
      shape: 'tabular';
      columns: string[];
      columnTypes: Record<string, string>;
      rowCount: number;
    }
  | { shape: 'mapping'; container: string; keys: string[]; length: number | null }
  | {
      shape: 'markup';
      root: string;
      children: Record<string, number>;
      attributes: string[];
    }
  | {
      shape: 'collection';
      fileCount: number;
      kindCounts: Record<string, number>;
      totalBytes: number;
    }
  | { shape: 'unsupported'; reason: string };

export interface ReadingCard {
  path: string;
  kind: string;
  sizeBytes: number;
  lineCount: number;
  structure: ReadingStructure;
  samples: ReadingSample[];
  signals: ReadingSignal[];
  code: string;
  reason: string;
  action: string;
  confidence: number;
}

export interface ReadingProbe {
  path: string;
  decision: ReadingDecision;
  intercepted: boolean;
  rawChars: number;
  outChars: number;
  denoiseRules: string[];
  card: ReadingCard | null;
  /** The exact `line`/`limit` calls that get the agent the rest of the file. */
  nextActions: string[] | null;
}

/* ---------------------------------------------------------------- *
 * Helpers
 * ---------------------------------------------------------------- */

/** Bytes as a short human string. Panels show savings, so this is everywhere. */
export function formatBytes(n: number): string {
  if (!Number.isFinite(n) || n <= 0) return '0 B';
  if (n < 1024) return `${Math.round(n)} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(n < 10 * 1024 ? 1 : 0)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

/** How much a capability saved, as a percentage of what it was handed. */
export function savedPercent(stats: CapabilityStats): number {
  if (stats.bytesIn <= 0) return 0;
  return Math.max(0, Math.min(1, (stats.bytesIn - stats.bytesOut) / stats.bytesIn));
}

/** Whether this capability reports tokens rather than only bytes. */
export function hasTokens(stats: CapabilityStats): boolean {
  return stats.tokensIn > 0;
}

/**
 * The saving, in the unit the capability actually measures.
 *
 * Tokens when there are tokens, bytes otherwise. A capability that reports
 * both is shown in tokens, because the window is measured in tokens and that
 * is the number the user is trying to move.
 */
export function savedLabel(stats: CapabilityStats): { value: string; unit: string } {
  if (hasTokens(stats)) {
    return { value: formatTokens(stats.tokensIn - stats.tokensOut), unit: 'tokens' };
  }
  return { value: formatBytes(stats.bytesIn - stats.bytesOut), unit: 'bytes' };
}

/** The same choice for one event. */
export function eventLabel(event: CapabilityEvent): {
  from: string;
  to: string;
  unit: 'tokens' | 'bytes';
  unchanged: boolean;
} {
  if (event.tokensIn > 0) {
    return {
      from: formatTokens(event.tokensIn),
      to: formatTokens(event.tokensOut),
      unit: 'tokens',
      unchanged: event.tokensIn === event.tokensOut,
    };
  }
  return {
    from: formatBytes(event.bytesIn),
    to: formatBytes(event.bytesOut),
    unit: 'bytes',
    unchanged: event.bytesIn > 0 && event.bytesIn === event.bytesOut,
  };
}

/** The counters worth showing, biggest first, with the noise dropped. */
export function topCounters(stats: CapabilityStats, limit = 6): [string, number][] {
  return Object.entries(stats.counters)
    .filter(([, value]) => value > 0)
    .sort((a, b) => b[1] - a[1])
    .slice(0, limit);
}

/** A counter key as a label: `rules_fired` becomes `rules fired`. */
export function counterLabel(key: string): string {
  return key.replace(/[_-]+/g, ' ');
}

/** The one-line summary of a card's structure, for the panel's header row. */
export function structureSummary(structure: ReadingStructure): string {
  switch (structure.shape) {
    case 'text':
      return `${structure.unitCount} units, ${structure.headings.length} headings`;
    case 'log':
      return `${structure.lineCount} lines, ${structure.repeated.length} repeated`;
    case 'tabular':
      return `${structure.rowCount} rows, ${structure.columns.length} columns`;
    case 'mapping':
      return `${structure.keys.length} keys${structure.length === null ? '' : `, ${structure.length} entries`}`;
    case 'markup':
      return `${structure.root}, ${Object.keys(structure.children).length} child kinds`;
    case 'collection':
      return `${structure.fileCount} files, ${formatBytes(structure.totalBytes)}`;
    case 'unsupported':
      return structure.reason;
  }
}

/* ---------------------------------------------------------------- *
 * The channeler
 * ---------------------------------------------------------------- */

/** One open channel, as the panel lists it. */
export interface ChannelEntry {
  id: string;
  backend: 'webview' | 'window' | 'process' | 'engine';
  host: 'desktop' | 'hidden-desktop' | 'session';
  actions: string[];
  capture: boolean;
}

/**
 * Whether the channeler is listening, and what it has open.
 *
 * The channeler is the app's own loopback server rather than an external
 * binary, so there is no path, no version and nothing to register. What the
 * panel can honestly say is whether it is up and what it is driving.
 */
export interface ChannelStatus {
  listening: boolean;
  port: number | null;
  url: string | null;
  channels: ChannelEntry[];
}
