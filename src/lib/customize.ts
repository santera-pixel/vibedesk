/**
 * Runtime theming and layout.
 *
 * A theme is data: a map of CSS custom properties plus declaration blocks for
 * the app's own classes. Rust validates it before it reaches the webview, so
 * this module only has to turn a validated object into a stylesheet.
 *
 * The stylesheet lives in a single `<style id="vd-user-theme">` element that is
 * replaced in place, which is what makes a change apply without a reload.
 *
 * A layout is narrower still: integers and closed enums, applied as custom
 * properties on the document element. Nothing here can become a declaration.
 */

export interface ThemeRule {
  selector: string;
  declarations: Record<string, string>;
}

export interface ThemeConfig {
  tokens: Record<string, string>;
  rules: ThemeRule[];
}

export type Side = 'left' | 'right';
export type Density = 'compact' | 'comfortable' | 'roomy';

/**
 * How much motion the app may use.
 *
 * `system` follows `prefers-reduced-motion`. The other two override it, because
 * the OS setting is machine-wide and blunt: a user who turned animation effects
 * off for battery reasons still wants a theme reveal, and a user who wants no
 * motion should not have to change a system preference to get it. Neither value
 * can conjure motion the platform cannot render — `revealTheme` still falls back
 * when `startViewTransition` is missing.
 */
export type Motion = 'system' | 'full' | 'reduced';

export interface LayoutConfig {
  railWidth: number;
  sidebarWidth: number;
  sidebarSide: Side;
  sidebarCollapsed: boolean;
  transcriptMax: number;
  composerMax: number;
  density: Density;
  motion: Motion;
}

/* ---------------------------------------------------------------- *
 * Actions — mirrors src-tauri/src/actions.rs
 * ---------------------------------------------------------------- */

/** The closed icon vocabulary. Rust rejects anything outside it. */
export type ActionIcon =
  | 'bolt'
  | 'book'
  | 'bug'
  | 'check'
  | 'code'
  | 'file'
  | 'folder'
  | 'git-branch'
  | 'globe'
  | 'hammer'
  | 'info'
  | 'list'
  | 'play'
  | 'refresh'
  | 'search'
  | 'settings'
  | 'sparkles'
  | 'terminal'
  | 'test-tube'
  | 'wrench';

export type ActionSlot = 'composer' | 'rail';

/** VibeDesk's own commands. Not a shell — the set is closed in Rust. */
export type BuiltinCommand =
  | 'reload-session'
  | 'reload-plugins'
  | 'new-chat'
  | 'toggle-theme'
  | 'toggle-sidebar'
  | 'focus-composer'
  | 'check-updates'
  | 'open-plugins'
  | 'open-config'
  | 'open-sessions'
  | 'open-context'
  | 'open-skills'
  | 'open-customize'
  | 'open-help'
  | 'open-swarm';

export type ActionRun =
  | { kind: 'prompt'; text: string }
  | { kind: 'command'; command: BuiltinCommand }
  | { kind: 'open-url'; url: string }
  | { kind: 'reveal'; path: string }
  /** Switch the agent's session mode. `mode` is an id it advertised. */
  | { kind: 'mode'; mode: string }
  /** Set one of the agent's config options. `option` is an id it advertised. */
  | { kind: 'option'; option: string; value: string }
  /** Cancel the turn the agent is running. */
  | { kind: 'cancel' }
  /**
   * Answer the permission request the agent is paused on with an allow.
   *
   * This is the control that unblocks a stopped agent. The option is chosen by
   * kind, not by id, because the ids belong to the agent and differ between
   * agents — `allow_once` is the one thing every ACP agent agrees on.
   */
  | { kind: 'approve'; scope: PermissionScope }
  /** Answer the permission request with a reject. */
  | { kind: 'reject'; scope: PermissionScope }
  /**
   * Answer the elicitation form the agent is paused on. `content` is a literal
   * object of form values, read only by `accept`.
   */
  | { kind: 'answer'; action: ElicitationAnswer; content?: Record<string, unknown> | null };

/** Which of the two options the agent offered to take. */
export type PermissionScope = 'once' | 'always';

/** How a button answers an elicitation form. */
export type ElicitationAnswer = 'accept' | 'decline' | 'cancel';

export interface CustomAction {
  id: string;
  label: string;
  icon?: ActionIcon | null;
  slot: ActionSlot;
  hint?: string | null;
  run: ActionRun;
}

export interface ActionsConfig {
  actions: CustomAction[];
}

/* ---------------------------------------------------------------- *
 * Panels — mirrors src-tauri/src/panels.rs
 * ---------------------------------------------------------------- */

/**
 * A `file` block carries a path and nothing else. It is rendered as a stub and
 * read only after an explicit right-click, which is the whole point: a panel
 * stays confined until the user asks it to look at the workspace.
 *
 * `data`, `chart`, `table` and `progress` carry expressions rather than
 * literals. The expression is a JSON value the Rust side has already parsed and
 * type-checked, so the renderer never has to validate one — it evaluates.
 */
export type PanelBlock =
  | { kind: 'text'; text: string }
  | { kind: 'file'; path: string; label?: string | null }
  | { kind: 'list'; label?: string | null; items: string[] }
  | { kind: 'link'; label: string; url: string }
  | { kind: 'actions'; ids: string[] }
  | { kind: 'sandbox'; html: string; title?: string | null; height: number }
  | {
      kind: 'widget';
      /**
       * Always populated by the time the renderer sees it. A widget authored as
       * `src: "widgets/x.html"` is read into `html` on load, so the frame URL —
       * which is the content hash — is computed from one field either way.
       */
      html: string;
      /** The file the html came from, when it came from one. */
      src?: string | null;
      title?: string | null;
      height: number;
      capabilities: WidgetCapability[];
      /**
       * Data inputs this widget declared for itself, on top of the panel's.
       * A widget that draws a graph over its own feed declares it here rather
       * than forcing the whole panel to carry a source only it reads.
       */
      sources?: PanelSource[];
    }
  | { kind: 'data'; label: string; value: Expr; hint?: string | null }
  | {
      kind: 'chart';
      label: string;
      style: ChartStyle;
      /** Single-series shorthand. A chart has this or `series`, never neither. */
      values?: Expr | null;
      series?: ChartSeries[];
      /** X labels, one per point. Numbers with `time` set are drawn as times. */
      x?: Expr | null;
      yMin?: Expr | null;
      yMax?: Expr | null;
      height?: number | null;
      /** Appended to axis labels and tooltip values: `%`, `ms`, `tok`. */
      unit?: string | null;
      time?: boolean;
      /** How the series combine. Only `bar` and `area` can stack. */
      stack?: ChartStack;
      /** How a value maps to a y position. `log` drops non-positive points. */
      scale?: ChartScale;
      /** Horizontal threshold lines: a budget, an SLA, a capacity. */
      reference?: ChartReference[];
    }
  | {
      kind: 'diagram';
      label: string;
      layout: DiagramLayout;
      nodes: DiagramNode[];
      edges: DiagramEdge[];
      routing: DiagramRouting;
      height?: number | null;
    }
  | { kind: 'table'; label?: string | null; columns: string[]; rows: Expr }
  | { kind: 'progress'; label: string; value: Expr; max?: Expr | null };

/** One named series in a `chart` block. Mirrors `ChartSeries` in `panels.rs`. */
export interface ChartSeries {
  name?: string | null;
  values: Expr;
}

/**
 * One thing a widget may ask the app to do. Mirrors `Capability` in
 * `panels.rs`, which is a closed set: a widget cannot reach an IPC command that
 * is not on this list, because the frame has no route to the app at all.
 */
export type WidgetCapability =
  | { kind: 'prompt' }
  | { kind: 'action'; id: string }
  | { kind: 'file' };

/** The name a widget sees, and the name the bridge checks against. */
export function capabilityName(capability: WidgetCapability): string {
  switch (capability.kind) {
    case 'prompt':
      return 'prompt';
    case 'file':
      return 'file';
    default:
      return `action:${capability.id}`;
  }
}

/**
 * Where a widget document is served from.
 *
 * Tauri serves a custom scheme on Windows and Android as
 * `http://<scheme>.localhost`, which is a different origin from the app — that
 * is what lets the document carry its own CSP instead of inheriting
 * `script-src 'self'`. A macOS or Linux port serves `widget://localhost`
 * instead; both are already in `frame-src` in `tauri.conf.json`.
 */
export const WIDGET_ORIGIN = widgetOrigin();

/**
 * Where the widget scheme lives on this platform.
 *
 * Tauri serves a custom scheme as `http://<scheme>.localhost` on Windows and
 * Android, and as `<scheme>://localhost` on macOS and Linux. The app's own
 * origin is decided by the same rule, so it is the one signal that cannot
 * disagree with the platform: if the app is at `http://tauri.localhost` the
 * widgets are at `http://widget.localhost`, and if the app is at
 * `tauri://localhost` they are at `widget://localhost`.
 *
 * A user-agent sniff would be the obvious alternative and is the wrong one: it
 * answers a question about the browser rather than about the scheme, and it
 * breaks the moment a platform changes its UA string. Both origins are already
 * in `frame-src` in `tauri.conf.json`, so whichever this returns is allowed.
 */
function widgetOrigin(): string {
  const origin = typeof window === 'undefined' ? '' : window.location.origin;
  return origin.startsWith('http://tauri.localhost') || origin.startsWith('https://tauri.localhost')
    ? 'http://widget.localhost'
    : 'widget://localhost';
}

/** Height bounds, mirroring `WIDGET_HEIGHT` in `panels.rs`. */
export const WIDGET_HEIGHT: [number, number] = [80, 720];

/** Longest prompt a widget may send, so a runaway loop cannot flood the agent. */
export const MAX_WIDGET_PROMPT = 8000;

/**
 * FNV-1a over the UTF-8 bytes, as 16 lowercase hex digits.
 *
 * The same function as `widget::hash` in Rust, and it has to be: the URL is the
 * content address, so a mismatch is a 404 in the frame. Both sides pin the same
 * test vectors.
 */
export function widgetHash(html: string): string {
  const bytes = new TextEncoder().encode(html);
  let value = 0xcbf29ce484222325n;
  const prime = 0x100000001b3n;
  const mask = 0xffffffffffffffffn;
  for (const byte of bytes) {
    value = ((value ^ BigInt(byte)) * prime) & mask;
  }
  return value.toString(16).padStart(16, '0');
}

/** The URL a widget block's document is served from. */
export function widgetUrl(html: string): string {
  return `${WIDGET_ORIGIN}/w/${widgetHash(html)}`;
}

export type ChartStyle = 'bar' | 'line' | 'area' | 'sparkline';

/**
 * How a `chart` combines its series. Mirrors `ChartStack`.
 *
 * `none` is the default because it is the only mode that is honest about series
 * that are not parts of a whole: two latencies side by side say something, two
 * latencies stacked say their sum, which is a number nobody asked for.
 */
export type ChartStack = 'none' | 'stacked' | 'percent';

/** How a `chart` maps a value to a y position. Mirrors `ChartScale`. */
export type ChartScale = 'linear' | 'log';

/** A horizontal threshold line across a `chart`. Mirrors `ChartReference`. */
export interface ChartReference {
  /** Where the line sits, in the chart's own units. */
  value: Expr;
  /** Drawn at the right end of the line. */
  label?: string | null;
  /** One of `DiagramTone`. Anything else resolves to `neutral`. */
  tone?: Expr | null;
}

/** How a `diagram` block places its nodes. Mirrors `DiagramLayout`. */
export type DiagramLayout = 'layered' | 'radial' | 'tree' | 'sequence';

/** How a `diagram` block draws its edges. Mirrors `DiagramRouting`. */
export type DiagramRouting = 'orthogonal' | 'curved';

/**
 * The semantic colour of a node. A closed set on purpose: a diagram that
 * accepted `#ff0000` would be a diagram whose colours ignore the theme. These
 * map to the app's own tokens, so a `danger` node is the same red as a failed
 * tool call in both themes.
 */
export type DiagramTone = 'neutral' | 'accent' | 'ok' | 'warn' | 'danger' | 'muted';

/**
 * One box in a `diagram` block. Mirrors `DiagramNode` in `panels.rs`.
 *
 * Every field that reaches the screen is an expression, so a node is a live
 * thing rather than a label: `tone` can resolve to `danger` when a source says
 * the service is down, `detail` can show the current region, and `when` can
 * remove the node entirely when it is not part of the picture.
 */
export interface DiagramNode {
  /** Referenced by an edge's `from` and `to`. Unique within the diagram. */
  id: string;
  label: Expr;
  /** A second line inside the box: a region, a version, a live count. */
  detail?: Expr | null;
  tone?: Expr | null;
  /** Hide the node, and every edge touching it, unless this is truthy. */
  when?: Expr | null;
  /** An id from `actions.json`, fired when the node is clicked. */
  action?: string | null;
}

/** One arrow in a `diagram` block. Mirrors `DiagramEdge` in `panels.rs`. */
export interface DiagramEdge {
  /** Optional. When present it must be unique; it is only a key. */
  id?: string | null;
  from: string;
  to: string;
  /** Drawn on the arrow. An expression, so it can be a live latency. */
  label?: Expr | null;
}

/**
 * A binding. Literals are expressions too, so this is the whole grammar:
 * `{ "@path": "/ci/queue/depth" }`, `{ "@gt": [a, b] }`, and so on. See
 * `src-tauri/src/expr.rs` for the operator list and the limits.
 */
export type Expr = unknown;

/**
 * A block plus the condition under which it is shown.
 *
 * The wire format is flat: `when` sits beside `kind`, not above it. Rust
 * serialises `Block` with `#[serde(flatten)]` on the body, so a block is
 * `{ "kind": "text", "text": "…", "when": { … } }` — not `{ body: { … } }`.
 */
export type Block = PanelBlock & { when?: Expr | null };

export interface CustomPanel {
  id: string;
  title: string;
  icon?: ActionIcon | null;
  width: number;
  /**
   * The layout tree. Source of truth when present.
   *
   * Typed as `PanelNode` because Rust's `layout` is a `Node`, which may be a
   * single block: a panel whose whole body is one paragraph is a legal file.
   */
  layout?: PanelNode | null;
  /** The document-order flattening of `layout`, computed by Rust. */
  blocks: Block[];
  /** Declared data inputs. A binding reads a path into the model they produce. */
  sources?: PanelSource[];
  /**
   * The plugin that contributed this panel, if any.
   *
   * Read-only: Rust sets it, and a user's `panels.json` that carries it is
   * refused. The editor filters these panels out before it saves, because the
   * whole list it was shown is what it writes back.
   */
  plugin?: string | null;
}

/**
 * A declared input. `builtin` reads VibeDesk's own state and needs no network;
 * `http` is fetched in Rust and may only address loopback. See
 * `src-tauri/src/data.rs` for the rule and why it is the load-bearing one.
 */
export type PanelSource =
  | { id: string; type: 'builtin'; name?: BuiltinName | null }
  | {
      id: string;
      type: 'http';
      url: string;
      mode?: 'poll' | 'sse';
      intervalMs?: number;
      timeoutMs?: number;
    };

export type BuiltinName = 'status' | 'usage' | 'session' | 'chats' | 'plugins' | 'clock';

/**
 * Every source a panel declares, including the ones its widgets declared.
 *
 * A widget may carry its own `sources` so a feed only it reads does not have to
 * sit on the panel. Rust runs them under the panel's id, so the model the widget
 * receives already contains them — which means the frontend has to read them
 * from the same place, or a widget-declared feed would run and never arrive.
 */
export function panelSources(panel: CustomPanel): PanelSource[] {
  const declared = panel.sources ?? [];
  const fromWidgets = (panel.blocks ?? []).flatMap((block) =>
    block.kind === 'widget' ? (block.sources ?? []) : [],
  );
  return fromWidgets.length ? [...declared, ...fromWidgets] : declared;
}

/**
 * A container in a panel's layout tree.
 *
 * `children` holds `PanelNode`s, not `LayoutNode`s: Rust's `Vec<Node>` mixes
 * containers and blocks at every level, so a stack may hold a block directly.
 *
 * `when` is a field on every node and every block. A node whose condition is
 * false hides its whole subtree, which is what lets a panel guard a group
 * instead of guarding each leaf inside it.
 */
export type LayoutNode =
  | { node: 'tabs'; tabs: Tab[]; when?: Expr | null }
  | { node: 'split'; children: PanelNode[]; weights?: number[] | null; when?: Expr | null }
  | { node: 'stack'; children: PanelNode[]; when?: Expr | null };

/** A leaf or a container. The two are told apart by the `node` tag. */
export type PanelNode = LayoutNode | Block;

export interface Tab {
  label: string;
  children: PanelNode[];
}

export interface PanelsConfig {
  panels: CustomPanel[];
}

/** What `config://changed` carries. `null` means "keep what you have". */
export interface ConfigPayload {
  theme: ThemeConfig | null;
  layout: LayoutConfig | null;
  actions: ActionsConfig | null;
  panels: PanelsConfig | null;
  error: string | null;
  source: string;
}

export interface CustomizePaths {
  configDir: string;
  pendingDir: string;
  themePath: string;
  layoutPath: string;
  actionsPath: string;
  panelsPath: string;
  manifestPath: string;
  safeMode: boolean;
}

/* ---------------------------------------------------------------- *
 * Sources
 *
 * Two ways a customization arrives from somewhere other than the user's
 * own editor: a plugin ships one, or the agent writes a proposal into
 * `<configDir>/pending/`. Neither is applied by being present. Both
 * arrive here as a listing, and the user picks.
 * ---------------------------------------------------------------- */

export type ConfigKind = 'theme' | 'layout' | 'actions' | 'panels';

export const CONFIG_KINDS: ConfigKind[] = ['theme', 'layout', 'actions', 'panels'];

/** A validated proposal waiting for a decision. */
/** One field a proposal moves. `from` is empty for a new field, `to` for a removed one. */
export interface Change {
  field: string;
  from: string;
  to: string;
}

export interface Proposal {
  kind: ConfigKind;
  source: string;
  path: string;
  summary: string;
  /** What actually moves, field by field. Capped at 24 by the backend. */
  changes: Change[];
  /** sha256 of the normalised body. `apply` refuses a mismatch. */
  digest: string;
}

/** What happened to a proposal, written where the agent can read it. */
export interface Receipt {
  kind: ConfigKind;
  applied: boolean;
  at: string;
  digest: string;
  summary: string;
  error: string | null;
}

/** What `config://pending` carries. */
export interface PendingState {
  proposals: Proposal[];
  issues: string[];
  /** The last decision per kind, so the bar can show what the agent will read. */
  receipts: Receipt[];
}

/** A customization a plugin ships. `valid` is false when it would be rejected. */
export interface PluginSource {
  plugin: string;
  scope: string;
  kind: ConfigKind;
  path: string;
  summary: string;
  digest: string;
  valid: boolean;
  error: string | null;
}

const STYLE_ID = 'vd-user-theme';

export function emptyTheme(): ThemeConfig {
  return { tokens: {}, rules: [] };
}

export function emptyActions(): ActionsConfig {
  return { actions: [] };
}

export function emptyPanels(): PanelsConfig {
  return { panels: [] };
}

export function defaultLayout(): LayoutConfig {
  return {
    railWidth: 48,
    sidebarWidth: 252,
    sidebarSide: 'right',
    sidebarCollapsed: false,
    transcriptMax: 720,
    composerMax: 720,
    density: 'comfortable',
    motion: 'system',
  };
}

export function themeToCss(theme: ThemeConfig): string {
  const lines: string[] = [];

  const tokens = Object.entries(theme.tokens ?? {});
  if (tokens.length > 0) {
    // `:root[data-theme]` outranks the built-in `[data-theme='ink']` block, so
    // a user token wins without needing `!important`.
    lines.push(':root, :root[data-theme] {');
    for (const [name, value] of tokens) lines.push(`  ${name}: ${value};`);
    lines.push('}');
  }

  for (const rule of theme.rules ?? []) {
    if (!rule.selector?.trim()) continue;
    lines.push(`${rule.selector} {`);
    for (const [property, value] of Object.entries(rule.declarations ?? {})) {
      lines.push(`  ${property}: ${value};`);
    }
    lines.push('}');
  }

  return lines.join('\n');
}

/** Apply a theme, or clear it when `theme` is null or empty. */
export function applyTheme(theme: ThemeConfig | null): void {
  const css = theme ? themeToCss(theme) : '';
  const existing = document.getElementById(STYLE_ID) as HTMLStyleElement | null;

  if (!css.trim()) {
    existing?.remove();
    return;
  }

  const node = existing ?? document.createElement('style');
  if (!existing) {
    node.id = STYLE_ID;
    document.head.appendChild(node);
  }
  node.textContent = css;
}

/**
 * Push a layout onto the document element.
 *
 * Every value is written as a custom property rather than as an inline style on
 * a component, so the stylesheet stays the only place that decides what a width
 * means. `--vd-density` is a multiplier the stylesheet applies to vertical
 * rhythm; the three named densities are the only values Rust will accept.
 */
export function applyLayout(layout: LayoutConfig | null): void {
  const root = document.documentElement;
  const value = layout ?? defaultLayout();

  root.style.setProperty('--vd-rail-w', `${value.railWidth}px`);
  root.style.setProperty('--vd-sidebar-w', `${value.sidebarWidth}px`);
  root.style.setProperty('--vd-transcript-max', `${value.transcriptMax}px`);
  root.style.setProperty('--vd-composer-max', `${value.composerMax}px`);
  root.style.setProperty('--vd-density', String(DENSITY_SCALE[value.density] ?? 1));
  root.dataset.sidebarSide = value.sidebarSide;
  root.dataset.sidebarCollapsed = String(value.sidebarCollapsed);
  // Read by `reveal.ts` and by the `[data-motion]` rules in `motion.css`. The
  // attribute is always present so a stylesheet never has to distinguish
  // "unset" from "system".
  root.dataset.motion = value.motion ?? 'system';
}

const DENSITY_SCALE: Record<Density, number> = {
  compact: 0.82,
  comfortable: 1,
  roomy: 1.22,
};
