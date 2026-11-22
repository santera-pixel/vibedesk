//! Declarative panels: a panel is a list of blocks, and a block is data.
//!
//! The requirement this module exists to satisfy is that a panel may read the
//! workspace, but only when the user asks it to. So a `file` block carries a
//! path and nothing else — no content, no cache, no eager read. The frontend
//! renders it as a stub, and the file is read only after an explicit
//! right-click. There is deliberately **no** `expanded` field: a config cannot
//! opt itself into reading the workspace on open, because that would defeat the
//! whole point.
//!
//! Everything else follows the same rule as `actions.rs`. `kind` is a tagged
//! enum, so a file cannot invent a block type. Paths must be relative and stay
//! inside the workspace. URLs must be http(s). Widths are clamped, so a panel
//! cannot push the transcript off the screen.
//!
//! The one exception to "a block is data" is `sandbox`, which carries markup.
//! It is the only block with a boundary to defend, and it is defended in three
//! independent places: the iframe's `sandbox` attribute, a CSP injected into the
//! document, and the checks below. See `SandboxBlock.tsx` for why it has no
//! scripts.
//!
//! `widget` is the second exception, and it is the one that runs scripts. It is
//! served from its own origin by `widget.rs`, so it carries its own policy
//! instead of inheriting the app's, and what it may *do* is a declared list of
//! capabilities rather than an implied one. A `sandbox` block is markup; a
//! `widget` block is a program with a named, bounded set of levers.
//!
//! # Layout
//!
//! A panel used to be a flat list of blocks. It is now a tree: `tabs`, `split`
//! and `stack` nodes whose leaves are blocks. The tree is the source of truth
//! (`CustomPanel::layout`); `CustomPanel::blocks` is the document-order
//! flattening of it, kept because the renderer reads it and because a flat list
//! is what a v1 file contains. The two can never disagree: `blocks` is
//! recomputed from `layout` on load and on save, and is not written to disk when
//! `layout` is present.
//!
//! # Bindings
//!
//! A block may carry a `when` condition, and the `data`, `chart`, `table` and
//! `progress` blocks carry values that are expressions rather than literals.
//! The language is in `expr.rs`: eight operator shapes, no loops, no calls, no
//! side effects, bounded depth and node count. Every expression is parsed and
//! type-checked here, at approval time, so a bad binding is a receipt the agent
//! can read rather than a blank box the user has to report.

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::actions::ActionIcon;
use crate::data::{self, Source};
use crate::expr::{Expr, Type};

/// A panels file larger than this is a mistake, not a layout.
pub const MAX_PANELS_BYTES: u64 = 32 * 1024;
pub const MAX_PANELS: usize = 12;
/// Prefix reserved for ids a plugin contributes.
///
/// `plugin_panels` applies it to every id it reads, and `validate` refuses a
/// user file that uses it, so a user panel can neither impersonate nor shadow a
/// plugin panel.
pub const PLUGIN_ID_PREFIX: &str = "plugin:";
/// Most blocks a panel may show once its layout is flattened.
pub const MAX_BLOCKS: usize = 40;
/// Most nodes and blocks together, counted across the whole tree.
pub const MAX_LAYOUT_NODES: usize = 60;
/// Deepest a layout may nest. A panel is a sidebar, not a window manager.
pub const MAX_LAYOUT_DEPTH: usize = 4;
/// Most tabs one `tabs` node may hold.
pub const MAX_TABS: usize = 8;
pub const MAX_TAB_LABEL: usize = 40;
pub const MAX_COLUMNS: usize = 8;
const MAX_WEIGHT: u16 = 12;
const MAX_LIST_ITEMS: usize = 50;
const MAX_ID: usize = 32;
const MAX_TITLE: usize = 40;
const MAX_TEXT: usize = 4000;
const MAX_ITEM: usize = 200;
const MAX_URL: usize = 2048;
const MAX_PATH: usize = 260;

/// Inclusive bounds for a panel's width, in CSS pixels.
pub const PANEL_WIDTH: (u16, u16) = (240, 520);

/// Inclusive bounds for a chart block's height, in CSS pixels.
///
/// A chart is the one block whose height is part of its meaning: a 38px strip
/// cannot carry an axis, and an axis is the difference between a chart and a
/// sparkline. The floor is the smallest height that fits four y-ticks and their
/// labels.
pub const CHART_HEIGHT: (u16, u16) = (60, 320);

/// How many series one chart may draw.
///
/// Four is where a categorical palette stops being distinguishable at panel
/// width. A fifth series is a second chart.
pub const MAX_CHART_SERIES: usize = 4;
/// More than four threshold lines is a chart nobody can read.
pub const MAX_CHART_REFERENCES: usize = 4;

/// A unit suffix is `%`, `ms`, `tok` — not a sentence.
const MAX_UNIT: usize = 12;

/// Inclusive bounds for a diagram block's height, in CSS pixels.
///
/// A diagram is a graph, and a graph needs room for two ranks of boxes plus the
/// gap between them. The floor is one rank and its labels; the ceiling is where
/// a panel stops being a sidebar.
pub const DIAGRAM_HEIGHT: (u16, u16) = (120, 520);

/// Most nodes one diagram may draw.
///
/// Past forty boxes at panel width the labels are unreadable and the layout is
/// a hairball. A graph that big is a `widget`'s job, where the author controls
/// the geometry.
pub const MAX_DIAGRAM_NODES: usize = 40;

/// Most edges one diagram may draw. Twice the nodes: a graph denser than that
/// is a matrix, not a diagram.
pub const MAX_DIAGRAM_EDGES: usize = 80;

/// A node id is a slug, not a sentence.
const MAX_DIAGRAM_ID: usize = 32;

/// Inclusive bounds for a sandbox block's height, in CSS pixels.
pub const SANDBOX_HEIGHT: (u16, u16) = (80, 480);

/// A sandbox block larger than this is a page, not a widget.
pub const MAX_SANDBOX_HTML: usize = 16 * 1024;

/// Substrings a sandbox block may not contain.
///
/// This is **not** the boundary. The boundary is the opaque-origin iframe the
/// block is rendered in plus the CSP injected into that document: no network, no
/// parent access, no storage. This list exists so the common mistake — a CDN
/// `<link>`, a Google Fonts `@import`, an `<iframe>` embed — fails at validation
/// with a message the agent reads in its receipt, instead of rendering as a
/// blank box the agent never finds out about. Matched case-insensitively,
/// because HTML is.
pub const SANDBOX_FORBIDDEN: [&str; 10] = [
    "http://",
    "https://",
    "<script src",
    "<link",
    "<iframe",
    "<object",
    "<embed",
    "<form",
    "javascript:",
    "@import",
];

/// Inclusive bounds for a widget block's height, in CSS pixels.
///
/// Taller than a sandbox block because a widget draws: a chart with axes, a
/// diagram, a control surface. The renderer also lets the widget ask for its own
/// height at runtime, clamped to the same range.
pub const WIDGET_HEIGHT: (u16, u16) = (80, 720);

/// A widget block larger than this is a page, not a widget.
pub const MAX_WIDGET_HTML: usize = 64 * 1024;

/// Most capabilities one widget may declare.
pub const MAX_WIDGET_CAPABILITIES: usize = 8;

/// Substrings a widget block may not contain.
///
/// Unlike `SANDBOX_FORBIDDEN` this is not about scripts — a widget runs its own.
/// It is about the things that would fail **silently**: the widget's policy is
/// `connect-src 'none'` and `img-src data:`, so a CDN `<script>`, a stylesheet
/// `<link>`, a Google Fonts `@import` or a remote `<img>` loads nothing and
/// reports nothing. Refusing them at write time turns a blank box into a receipt
/// the agent can read. A runtime failure inside the widget's own script is
/// visible in its console and is not caught here.
pub const WIDGET_FORBIDDEN: [&str; 10] = [
    "<script src",
    "<link",
    "@import",
    "<iframe",
    "<object",
    "<embed",
    "<form",
    "src=\"http",
    "src='http",
    "url(http",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
// `rename_all` renames the *variants*; without `rename_all_fields` a field like
// `y_min` stays snake_case and a file that writes `yMin` has it silently
// dropped by serde. Every other field in this enum is a single word, so the
// field rename is a no-op for them and a fix for the multi-word ones.
#[serde(tag = "kind", rename_all = "kebab-case", rename_all_fields = "camelCase")]
pub enum PanelBlock {
    /// A paragraph. Rendered as prose, never as markup.
    Text { text: String },
    /// A workspace file, shown as a stub until the user right-clicks it.
    File {
        path: String,
        #[serde(default)]
        label: Option<String>,
    },
    /// A short list of strings.
    List {
        #[serde(default)]
        label: Option<String>,
        items: Vec<String>,
    },
    /// A link that opens in the browser.
    Link { label: String, url: String },
    /// Buttons borrowed from `actions.json`, by id.
    Actions { ids: Vec<String> },
    /// Agent- or plugin-authored HTML, rendered in an opaque-origin iframe.
    ///
    /// The only block whose content is markup rather than data, so it is the
    /// only one that needs a boundary. It is markup and CSS only: no scripts.
    /// That is not a limitation of this code but of the platform — a `srcdoc`,
    /// `blob:` or `data:` document all inherit the *parent's* CSP in Chromium,
    /// and the app's policy is `script-src 'self'`. A frame cannot be given a
    /// looser policy than the app without loosening the app, and `'unsafe-inline'`
    /// on the whole app to enable a widget is not a trade worth making. What is
    /// left — styled cards, tables, SVG, CSS animation — is what a panel needs.
    /// A widget that has to *do* something uses an `actions` block, which is
    /// declarative and already validated.
    Sandbox {
        html: String,
        #[serde(default)]
        title: Option<String>,
        #[serde(default = "default_sandbox_height")]
        height: u16,
    },
    /// Agent- or plugin-authored HTML **and scripts**, in a sandboxed frame on
    /// its own origin.
    ///
    /// This is the block a `sandbox` cannot be. The document is served from the
    /// `widget` scheme, which is a different origin from the app, so it carries
    /// its own `Content-Security-Policy` instead of inheriting `script-src
    /// 'self'`. See `widget.rs` for the boundary and for why the app's own policy
    /// does not have to be loosened to get here.
    ///
    /// What it may *do* is declared, not implied. `capabilities` is the whole
    /// list: `prompt` sends text to the agent, `action` fires one entry from
    /// `actions.json`. A widget that asks for anything else is refused, and the
    /// refusal is reported back to it.
    Widget {
        /// Inline html. A block writes `html` or `src`, not both; after load
        /// `html` is always populated, because `src` is resolved into it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        html: Option<String>,
        /// A file under `<config>/widgets/`, e.g. `widgets/context-ring.html`.
        ///
        /// This is the shape a real widget wants: a 64 KB widget is a 64 KB
        /// escaped string inside `panels.json`, which is neither pleasant to
        /// write nor reviewable in a diff. A sibling file is both.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        src: Option<String>,
        #[serde(default)]
        title: Option<String>,
        #[serde(default = "default_widget_height")]
        height: u16,
        #[serde(default)]
        capabilities: Vec<Capability>,
        /// Sources this widget needs.
        ///
        /// They join the panel's data plane under the panel's id, so a widget
        /// can declare its own feed instead of needing the panel edited too.
        #[serde(default)]
        sources: Vec<Source>,
    },
    /// One bound value, rendered large. The first block that shows live data.
    Data {
        label: String,
        value: Expr,
        #[serde(default)]
        hint: Option<String>,
    },
    /// A chart over one or more bound series.
    ///
    /// `values` is the single-series shorthand; `series` is the real shape. A
    /// chart with neither is refused rather than drawn as an empty box, because
    /// an empty box is indistinguishable from a broken binding.
    Chart {
        label: String,
        #[serde(default)]
        style: ChartStyle,
        #[serde(default)]
        values: Option<Expr>,
        #[serde(default)]
        series: Vec<ChartSeries>,
        /// X labels, one per point. Strings are drawn as categories; numbers
        /// with `time` set are drawn as a time axis.
        #[serde(default)]
        x: Option<Expr>,
        /// Explicit scale bounds. Without them the axis is fitted to the data
        /// and rounded out to whole ticks.
        #[serde(default)]
        y_min: Option<Expr>,
        #[serde(default)]
        y_max: Option<Expr>,
        #[serde(default)]
        height: Option<u16>,
        /// Appended to every axis label and tooltip value: `%`, `ms`, `tok`.
        #[serde(default)]
        unit: Option<String>,
        /// Treat the x values as epoch milliseconds and label them as times.
        #[serde(default)]
        time: bool,
        /// How the series combine. `none` draws them side by side, which is the
        /// only honest option for series that are not parts of a whole.
        #[serde(default)]
        stack: ChartStack,
        /// How a value maps to a y position. `log` is the only way to read a
        /// series that spans orders of magnitude.
        #[serde(default)]
        scale: ChartScale,
        /// Horizontal threshold lines: a budget, an SLA, a capacity.
        #[serde(default)]
        reference: Vec<ChartReference>,
    },
    /// A graph: nodes and edges, laid out by one of four algorithms.
    ///
    /// This is the block a `chart` cannot be and a `widget` should not have to
    /// be. A chart plots a series; a widget is a blank canvas that costs two
    /// hundred lines of JavaScript. A diagram says "draw this graph" in twenty
    /// lines of JSON, and every visible field on it is an expression, so a node
    /// turns red when a source says the service is down and an edge's label
    /// shows the live latency.
    ///
    /// The layout is an algorithm, not a grid: `layered` ranks by longest path
    /// and orders each rank by barycentre, `radial` finds the cycle and closes
    /// it into a ring, `tree` is a tidy tree with children centred under their
    /// parent, `sequence` is lifelines and messages. Each is deterministic, so
    /// the same JSON always draws the same picture.
    Diagram {
        label: String,
        #[serde(default)]
        layout: DiagramLayout,
        nodes: Vec<DiagramNode>,
        #[serde(default)]
        edges: Vec<DiagramEdge>,
        /// How edges are drawn. `orthogonal` is elbows through the gaps between
        /// ranks; `curved` is a bowed bezier. Both are layout-aware, so neither
        /// crosses a node.
        #[serde(default)]
        routing: DiagramRouting,
        #[serde(default)]
        height: Option<u16>,
    },
    /// A table over a bound array of rows. Each row is an array of cells or an
    /// object keyed by `columns`.
    Table {
        #[serde(default)]
        label: Option<String>,
        columns: Vec<String>,
        rows: Expr,
    },
    /// A progress bar over a bound number.
    Progress {
        label: String,
        value: Expr,
        #[serde(default)]
        max: Option<Expr>,
    },
}

/// One named series in a `chart` block.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChartSeries {
    /// Shown in the legend and the tooltip. A single unnamed series draws no
    /// legend, because a legend of one is noise.
    #[serde(default)]
    pub name: Option<String>,
    pub values: Expr,
}

/// How a `chart` block draws its values.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChartStyle {
    #[default]
    Bar,
    Line,
    /// A line with the area under it filled. The right style for a single
    /// series over time, where the magnitude matters as much as the shape.
    Area,
    /// No axes, no labels, no tooltip. The strip the old `chart` always was,
    /// kept as an explicit choice rather than as the only option.
    Sparkline,
}

impl ChartStyle {
    pub const ALL: [&'static str; 4] = ["bar", "line", "area", "sparkline"];
}

/// How a `chart` combines its series.
///
/// Grouped is the default because it is the only mode that is honest about
/// series that are not parts of a whole: two latencies side by side say
/// something, two latencies stacked say their sum, which is a number nobody
/// asked for. Stacking is opt-in for the case where the sum *is* the question.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChartStack {
    /// Side by side.
    #[default]
    None,
    /// Accumulated: each series sits on the one below it, so the top of the
    /// stack is the total.
    Stacked,
    /// Accumulated and normalised to 100%. The right mode when the question is
    /// "what share" rather than "how much".
    Percent,
}

impl ChartStack {
    pub const ALL: [&'static str; 3] = ["none", "stacked", "percent"];

    /// Stacking a line is meaningless — a line has no thickness to accumulate.
    /// Rejecting it is better than drawing something that ignores the field.
    pub fn fits(self, style: ChartStyle) -> bool {
        match self {
            ChartStack::None => true,
            ChartStack::Stacked | ChartStack::Percent => {
                matches!(style, ChartStyle::Bar | ChartStyle::Area)
            }
        }
    }
}

/// How a `chart` maps a value to a y position.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChartScale {
    #[default]
    Linear,
    /// Each decade is the same height. A value at or below zero has no place on
    /// a log axis, so it is dropped rather than drawn at minus infinity, and the
    /// domain never includes zero.
    Log,
}

impl ChartScale {
    pub const ALL: [&'static str; 2] = ["linear", "log"];
}

/// A horizontal threshold line across a `chart`.
///
/// The value is an expression, so a budget can come from a source and move when
/// the budget moves. The tone is the same closed vocabulary a diagram node uses,
/// because a red line means the same thing in both.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChartReference {
    /// Where the line sits, in the chart's own units.
    pub value: Expr,
    /// Drawn at the right end of the line. Without one the line is unlabelled,
    /// which is only useful when the value is obvious from the axis.
    #[serde(default)]
    pub label: Option<String>,
    /// One of `DiagramTone::ALL`. Anything else resolves to `neutral`.
    #[serde(default)]
    pub tone: Option<Expr>,
}

/// One box in a `diagram` block.
///
/// Every field that reaches the screen is an expression, so a node is a live
/// thing rather than a label: `tone` can resolve to `danger` when a source says
/// the service is down, `detail` can show the current region, and `when` can
/// remove the node entirely when it is not part of the picture.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagramNode {
    /// Referenced by an edge's `from` and `to`. Unique within the diagram.
    pub id: String,
    pub label: Expr,
    /// A second line inside the box: a region, a version, a live count.
    #[serde(default)]
    pub detail: Option<Expr>,
    /// One of `DiagramTone::ALL`. Anything else resolves to `neutral`.
    #[serde(default)]
    pub tone: Option<Expr>,
    /// Hide the node, and every edge touching it, unless this is truthy.
    #[serde(default)]
    pub when: Option<Expr>,
    /// An id from `actions.json`, fired when the node is clicked. This is what
    /// makes a diagram a control surface rather than a picture.
    #[serde(default)]
    pub action: Option<String>,
}

/// One arrow in a `diagram` block.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagramEdge {
    /// Optional. When present it must be unique; it is only a key.
    #[serde(default)]
    pub id: Option<String>,
    pub from: String,
    pub to: String,
    /// Drawn on the arrow. An expression, so it can be a live latency.
    #[serde(default)]
    pub label: Option<Expr>,
}

/// How a `diagram` block places its nodes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DiagramLayout {
    /// Rank by longest path, order each rank by barycentre, place in columns.
    /// The flowchart layout, and the default.
    #[default]
    Layered,
    /// Find the cycle and close it into a ring. The "process that repeats"
    /// layout.
    Radial,
    /// A tidy tree: one root, children centred under their parent.
    Tree,
    /// Lifelines across, messages down, in declaration order.
    Sequence,
}

impl DiagramLayout {
    pub const ALL: [&'static str; 4] = ["layered", "radial", "tree", "sequence"];
}

/// How a `diagram` block draws its edges.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DiagramRouting {
    /// Elbows through the gap between ranks. Never crosses a node, because the
    /// vertical run happens in empty space.
    #[default]
    Orthogonal,
    /// A bowed bezier. Reads better for a radial layout, where the chord is
    /// already clear of every node.
    Curved,
}

impl DiagramRouting {
    pub const ALL: [&'static str; 2] = ["orthogonal", "curved"];
}

/// The semantic colour of a node.
///
/// A closed set, because a diagram that accepts `#ff0000` is a diagram whose
/// colours do not follow the theme. These map to the app's own tokens, so a
/// `danger` node is the same red as a failed tool call in both themes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DiagramTone {
    #[default]
    Neutral,
    Accent,
    Ok,
    Warn,
    Danger,
    Muted,
}

impl DiagramTone {
    pub const ALL: [&'static str; 6] = ["neutral", "accent", "ok", "warn", "danger", "muted"];
}

fn default_sandbox_height() -> u16 {
    160
}

fn default_widget_height() -> u16 {
    240
}

/// One thing a widget is allowed to ask the app to do.
///
/// A closed set, on purpose. The alternative — letting a widget call any IPC
/// command — would make the frame boundary decorative: the point of the sandbox
/// is that the document cannot reach the app, so the only way through is a
/// capability the panel declared in a file the user can read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Capability {
    /// Send text to the agent, exactly as if it had been typed into the
    /// composer. The one capability that changes what the agent does.
    Prompt,
    /// Fire one action from `actions.json`, by id. The action's own `run` is
    /// what decides the effect, so a widget cannot invent one.
    Action { id: String },
    /// Read a workspace file by relative path.
    ///
    /// The path is resolved against the workspace root and refused if it
    /// escapes it, so this is the same reach a `file` block has — the
    /// difference is that a widget can ask on its own schedule instead of
    /// waiting for a right-click. Declaring it is the consent: the block is in
    /// the file the user approved, and the manifest says what it means.
    File,
}

impl Capability {
    /// The name the widget sees, and the name the renderer checks against.
    ///
    /// Flat strings rather than objects because the widget's side of the bridge
    /// is a string list: `vibedesk.has('action:refresh')` is the whole check.
    pub fn name(&self) -> String {
        match self {
            Capability::Prompt => "prompt".into(),
            Capability::Action { id } => format!("action:{id}"),
            Capability::File => "file".into(),
        }
    }
}

impl PanelBlock {
    /// Every block kind, as the tag serde expects. Published in the manifest so
    /// the agent does not invent an eleventh; the test below fails if a variant
    /// is added without being listed here.
    pub const KINDS: [&'static str; 12] = [
        "text", "file", "list", "link", "actions", "sandbox", "widget", "data", "chart", "diagram",
        "table", "progress",
    ];
}

/// A block plus the condition under which it is shown.
///
/// `when` is a field on the wrapper rather than on each variant so that every
/// block kind gets it for free, and so the JSON shape of a block without a
/// condition is byte-identical to what v1 wrote.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Block {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<Expr>,
    #[serde(flatten)]
    pub body: PanelBlock,
}

impl Block {
    pub fn new(body: PanelBlock) -> Self {
        Block { when: None, body }
    }

}

impl From<PanelBlock> for Block {
    fn from(body: PanelBlock) -> Self {
        Block::new(body)
    }
}

/// One tab in a `tabs` node.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tab {
    pub label: String,
    #[serde(default)]
    pub children: Vec<Node>,
}

/// A container in a panel's layout tree.
///
/// `when` is a field on every node and every block, so a whole container can be
/// hidden, not only a leaf. It is skipped when absent, which keeps the JSON of a
/// node without a condition byte-identical to what Phase 1 wrote.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "node", rename_all = "kebab-case")]
pub enum LayoutNode {
    /// One tab visible at a time.
    Tabs {
        tabs: Vec<Tab>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        when: Option<Expr>,
    },
    /// Side by side. `weights` are relative widths; omitted means equal.
    Split {
        children: Vec<Node>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        weights: Option<Vec<u16>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        when: Option<Expr>,
    },
    /// Stacked vertically.
    Stack {
        children: Vec<Node>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        when: Option<Expr>,
    },
}

impl LayoutNode {
    /// Every container kind, as the `node` tag serde expects.
    pub const KINDS: [&'static str; 3] = ["tabs", "split", "stack"];

    /// Fields a node may carry beside its `node` tag. Published in the manifest
    /// so the agent knows a container can be guarded, not only a block.
    pub const FIELDS: [&'static str; 1] = ["when"];

    /// The condition under which this node is shown, if any.
    pub fn when(&self) -> Option<&Expr> {
        match self {
            LayoutNode::Tabs { when, .. }
            | LayoutNode::Split { when, .. }
            | LayoutNode::Stack { when, .. } => when.as_ref(),
        }
    }

    /// Every block under this node, mutably.
    ///
    /// `blocks` is a derived view; `layout` is what renders and what `validate`
    /// walks. A clamp applied to the derived view alone leaves the tree holding
    /// the original value, so the clamp has to reach the tree.
    pub fn for_each_block_mut(&mut self, f: &mut impl FnMut(&mut Block)) {
        match self {
            LayoutNode::Tabs { tabs, .. } => {
                for tab in tabs {
                    for child in &mut tab.children {
                        child.for_each_block_mut(f);
                    }
                }
            }
            LayoutNode::Split { children, .. } | LayoutNode::Stack { children, .. } => {
                for child in children {
                    child.for_each_block_mut(f);
                }
            }
        }
    }
}

/// A node in a panel's layout tree: either a container or a block.
///
/// Deserialised by hand rather than with `#[serde(untagged)]` so that a node
/// with neither tag produces one sentence the agent can act on, instead of
/// serde's "data did not match any variant of untagged enum Node".
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum Node {
    Layout(LayoutNode),
    Block(Block),
}

impl<'de> Deserialize<'de> for Node {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        Node::from_value(&value).map_err(serde::de::Error::custom)
    }
}

impl Node {
    fn from_value(value: &Value) -> Result<Node, String> {
        let Value::Object(map) = value else {
            return Err("a layout node must be an object".into());
        };
        if map.contains_key("node") {
            let layout: LayoutNode = serde_json::from_value(value.clone())
                .map_err(|e| format!("invalid layout node: {e}"))?;
            Ok(Node::Layout(layout))
        } else if map.contains_key("kind") {
            let block: Block = serde_json::from_value(value.clone())
                .map_err(|e| format!("invalid block: {e}"))?;
            Ok(Node::Block(block))
        } else {
            Err(
                "a layout node needs either `node` (one of tabs, split, stack) or `kind` (a block)"
                    .into(),
            )
        }
    }

    /// The condition under which this node is shown, if any.
    ///
    /// `when` is a field on every node and every block, so this is the one place
    /// to read it. A hidden node still contributes its blocks to `flatten`,
    /// because that view is structural: it says what the panel contains, not
    /// what it currently shows.
    pub fn when(&self) -> Option<&Expr> {
        match self {
            Node::Block(block) => block.when.as_ref(),
            Node::Layout(layout) => layout.when(),
        }
    }

    /// Every block in this subtree, mutably. See `LayoutNode::for_each_block_mut`.
    pub fn for_each_block_mut(&mut self, f: &mut impl FnMut(&mut Block)) {
        match self {
            Node::Block(block) => f(block),
            Node::Layout(layout) => layout.for_each_block_mut(f),
        }
    }

    /// Every block in the tree, in document order.
    pub fn flatten(&self) -> Vec<Block> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }

    fn collect(&self, out: &mut Vec<Block>) {
        match self {
            Node::Block(block) => out.push(block.clone()),
            Node::Layout(LayoutNode::Tabs { tabs, .. }) => {
                for tab in tabs {
                    for child in &tab.children {
                        child.collect(out);
                    }
                }
            }
            Node::Layout(LayoutNode::Split { children, .. })
            | Node::Layout(LayoutNode::Stack { children, .. }) => {
                for child in children {
                    child.collect(out);
                }
            }
        }
    }

    /// Every path any block or node condition in the tree reads, in document
    /// order. Phase 2 uses this to decide which sources a panel needs, so a
    /// panel that binds nothing polls nothing.
    pub fn paths(&self) -> Vec<String> {
        let mut found = Vec::new();
        self.collect_paths(&mut found);
        found
    }

    fn collect_paths(&self, found: &mut Vec<String>) {
        // A node's own condition is a promise to read a path just as a block's
        // is, so it is collected before the children.
        if let Some(when) = self.when() {
            push_paths(found, when.paths());
        }
        match self {
            Node::Block(block) => push_paths(found, block.body.paths()),
            Node::Layout(LayoutNode::Tabs { tabs, .. }) => {
                for tab in tabs {
                    for child in &tab.children {
                        child.collect_paths(found);
                    }
                }
            }
            Node::Layout(LayoutNode::Split { children, .. })
            | Node::Layout(LayoutNode::Stack { children, .. }) => {
                for child in children {
                    child.collect_paths(found);
                }
            }
        }
    }
}

fn push_paths(found: &mut Vec<String>, paths: Vec<String>) {
    for path in paths {
        if !found.contains(&path) {
            found.push(path);
        }
    }
}

impl PanelBlock {
    /// Every path this block's bindings read.
    pub fn paths(&self) -> Vec<String> {
        let mut found = Vec::new();
        let mut add = |expr: &Expr| {
            for path in expr.paths() {
                if !found.contains(&path) {
                    found.push(path);
                }
            }
        };
        match self {
            PanelBlock::Data { value, .. } => add(value),
            PanelBlock::Chart {
                values,
                series,
                x,
                y_min,
                y_max,
                reference,
                ..
            } => {
                if let Some(values) = values {
                    add(values);
                }
                for one in series {
                    add(&one.values);
                }
                if let Some(x) = x {
                    add(x);
                }
                if let Some(y) = y_min {
                    add(y);
                }
                if let Some(y) = y_max {
                    add(y);
                }
                for one in reference {
                    add(&one.value);
                    if let Some(tone) = &one.tone {
                        add(tone);
                    }
                }
            }
            PanelBlock::Table { rows, .. } => add(rows),
            PanelBlock::Diagram { nodes, edges, .. } => {
                for node in nodes {
                    add(&node.label);
                    if let Some(detail) = &node.detail {
                        add(detail);
                    }
                    if let Some(tone) = &node.tone {
                        add(tone);
                    }
                    if let Some(when) = &node.when {
                        add(when);
                    }
                }
                for edge in edges {
                    if let Some(label) = &edge.label {
                        add(label);
                    }
                }
            }
            PanelBlock::Progress { value, max, .. } => {
                add(value);
                if let Some(max) = max {
                    add(max);
                }
            }
            _ => {}
        }
        found
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomPanel {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub icon: Option<ActionIcon>,
    /// Clamped into `PANEL_WIDTH`.
    #[serde(default = "default_width")]
    pub width: u16,
    /// The layout tree. Source of truth when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<Node>,
    /// The document-order flattening of `layout`. Derived: recomputed on load
    /// and on save, never authored, and not written to disk when `layout` is.
    #[serde(default)]
    pub blocks: Vec<Block>,
    /// Declared data inputs. A binding reads a path into the model these
    /// produce; see `data.rs` for what may be declared and why.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<Source>,
    /// The plugin that contributed this panel, if any.
    ///
    /// Reserved: `plugin_panels` is the only writer, and `validate` refuses a
    /// user file that sets it. Skipped when absent, so a user's `panels.json`
    /// never gains the key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin: Option<String>,
}

impl CustomPanel {
    /// Bring `layout` and `blocks` back into agreement.
    ///
    /// A v1 file has `blocks` and no `layout`; it is upgraded to a single stack.
    /// A v2 file has `layout`; `blocks` is rebuilt from it and whatever the file
    /// said is discarded, so a hand edit to the derived view cannot take effect.
    fn reconcile(&mut self) {
        match self.layout.take() {
            Some(layout) => {
                self.blocks = layout.flatten();
                self.layout = Some(layout);
            }
            None => {
                let children: Vec<Node> =
                    self.blocks.iter().cloned().map(Node::Block).collect();
                self.layout = Some(Node::Layout(LayoutNode::Stack { children, when: None }));
            }
        }
    }

    /// The tree, upgrading a v1 panel on the fly.
    pub fn tree(&self) -> Node {
        match &self.layout {
            Some(layout) => layout.clone(),
            None => Node::Layout(LayoutNode::Stack {
                children: self.blocks.iter().cloned().map(Node::Block).collect(),
                when: None,
            }),
        }
    }
}

fn default_width() -> u16 {
    320
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PanelsConfig {
    pub panels: Vec<CustomPanel>,
}

impl PanelsConfig {
    /// Pull every width and block height into range, and rebuild the derived
    /// `blocks` view. Idempotent.
    ///
    /// The clamp runs over the layout tree, not over `blocks`. `blocks` is
    /// derived from the tree on every load, so clamping it alone would leave the
    /// tree — which is what renders and what `validate` walks — holding the
    /// original value, and a v1 panel would fail validation on a height the
    /// clamp had already fixed.
    pub fn clamped(mut self) -> Self {
        for panel in &mut self.panels {
            panel.width = panel.width.clamp(PANEL_WIDTH.0, PANEL_WIDTH.1);
            panel.reconcile();
            if let Some(layout) = &mut panel.layout {
                layout.for_each_block_mut(&mut |block| match &mut block.body {
                    PanelBlock::Sandbox { height, .. } => {
                        *height = (*height).clamp(SANDBOX_HEIGHT.0, SANDBOX_HEIGHT.1);
                    }
                    PanelBlock::Widget { height, .. } => {
                        *height = (*height).clamp(WIDGET_HEIGHT.0, WIDGET_HEIGHT.1);
                    }
                    PanelBlock::Chart { height, .. } => {
                        if let Some(height) = height {
                            *height = (*height).clamp(CHART_HEIGHT.0, CHART_HEIGHT.1);
                        }
                    }
                    PanelBlock::Diagram { height, .. } => {
                        if let Some(height) = height {
                            *height = (*height).clamp(DIAGRAM_HEIGHT.0, DIAGRAM_HEIGHT.1);
                        }
                    }
                    _ => {}
                });
            }
            panel.reconcile();
        }
        self
    }
}

pub fn panels_path() -> PathBuf {
    super::customize::config_dir().join("panels.json")
}

/// The directory a widget's `src` is resolved against.
pub fn widgets_dir() -> PathBuf {
    super::customize::config_dir().join("widgets")
}

/// A stamp over `widgets/`, so editing a widget file reloads the panel.
///
/// The four config files are stamped individually. A widget's html lives in a
/// sibling file, and a directory's own mtime does not move when a file inside it
/// is edited — so this is the newest mtime plus the total length of every
/// `.html` in the directory. Bounded: one `read_dir` and one `metadata` per
/// entry, and nothing is read.
pub fn widgets_stamp() -> Option<(std::time::SystemTime, u64)> {
    let entries = std::fs::read_dir(widgets_dir()).ok()?;
    let mut newest = std::time::SystemTime::UNIX_EPOCH;
    let mut total = 0u64;
    let mut seen = false;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("html") {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        seen = true;
        total = total.saturating_add(meta.len());
        if let Ok(modified) = meta.modified() {
            if modified > newest {
                newest = modified;
            }
        }
    }
    if seen {
        Some((newest, total))
    } else {
        None
    }
}

pub fn load_panels() -> Result<PanelsConfig, String> {
    load_panels_at(&panels_path(), super::customize::safe_mode())
}

/// The safe-mode decision is a parameter so the bypass can be tested without
/// writing to the real data directory.
pub fn load_panels_at(path: &std::path::Path, safe: bool) -> Result<PanelsConfig, String> {
    if safe {
        return Ok(PanelsConfig::default());
    }
    load_panels_from(path)
}

pub fn load_panels_from(path: &std::path::Path) -> Result<PanelsConfig, String> {
    let Ok(meta) = std::fs::metadata(path) else {
        return Ok(PanelsConfig::default());
    };
    if meta.len() > MAX_PANELS_BYTES {
        return Err(format!(
            "panels.json is {} bytes; the limit is {}",
            meta.len(),
            MAX_PANELS_BYTES
        ));
    }

    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("could not read {}: {e}", path.display()))?;
    if raw.trim().is_empty() {
        return Ok(PanelsConfig::default());
    }

    let config: PanelsConfig = serde_json::from_str(&raw)
        .map_err(|e| format!("panels.json is not valid JSON: {e}"))?;
    validate(&config)?;
    let config = resolve_widget_srcs(config, path)?;
    Ok(config.clamped())
}

/// Read one widget block's `src` into its `html`, if it has one.
fn resolve_widget_block(
    body: &mut PanelBlock,
    panel_id: &str,
    dir: &std::path::Path,
) -> Result<(), String> {
    let PanelBlock::Widget { html, src, .. } = body else {
        return Ok(());
    };
    let Some(src) = src else { return Ok(()) };
    let file = dir.join(src);
    let meta = std::fs::metadata(&file).map_err(|e| {
        format!(
            "panel `{panel_id}`: widget `src` {} could not be read: {e}",
            file.display()
        )
    })?;
    if meta.len() > MAX_WIDGET_HTML as u64 {
        return Err(format!(
            "panel `{panel_id}`: widget `src` {} is {} bytes, more than the {MAX_WIDGET_HTML} allowed",
            file.display(),
            meta.len()
        ));
    }
    let body = std::fs::read_to_string(&file).map_err(|e| {
        format!(
            "panel `{panel_id}`: widget `src` {} could not be read: {e}",
            file.display()
        )
    })?;
    *html = Some(body);
    Ok(())
}

/// Walk a layout tree, resolving every widget block in it.
fn resolve_widget_nodes(
    node: &mut Node,
    panel_id: &str,
    dir: &std::path::Path,
) -> Result<(), String> {
    match node {
        Node::Block(block) => resolve_widget_block(&mut block.body, panel_id, dir),
        Node::Layout(layout) => match layout {
            LayoutNode::Tabs { tabs, .. } => {
                for tab in tabs {
                    for child in &mut tab.children {
                        resolve_widget_nodes(child, panel_id, dir)?;
                    }
                }
                Ok(())
            }
            LayoutNode::Split { children, .. } | LayoutNode::Stack { children, .. } => {
                for child in children {
                    resolve_widget_nodes(child, panel_id, dir)?;
                }
                Ok(())
            }
        },
    }
}

/// Read every `widgets/<name>.html` a block points at into the block's `html`.
///
/// The file is the source of truth and `html` is the derived view, which is why
/// `to_disk` strips `html` back out for a block that has a `src`. Doing it here
/// rather than in the renderer means the frontend keeps one shape — a widget
/// always has html to hash — and a missing file is a load error the agent reads
/// in its receipt instead of a blank frame nobody can explain.
///
/// Both views are walked: a v1 file carries `blocks`, a v2 file carries
/// `layout`, and a file that was saved by this module carries `layout` only.
/// Resolving before `clamped()` means `reconcile` then rebuilds `blocks` from
/// the already-resolved tree, so the two cannot disagree.
fn resolve_widget_srcs(
    mut config: PanelsConfig,
    panels_path: &std::path::Path,
) -> Result<PanelsConfig, String> {
    let Some(dir) = panels_path.parent() else {
        return Ok(config);
    };
    for panel in &mut config.panels {
        let id = panel.id.clone();
        for block in &mut panel.blocks {
            resolve_widget_block(&mut block.body, &id, dir)?;
        }
        if let Some(layout) = &mut panel.layout {
            resolve_widget_nodes(layout, &id, dir)?;
        }
    }
    Ok(config)
}

/// Clamp, then write. An out-of-range width never reaches disk.
pub fn save_panels(config: &PanelsConfig) -> Result<PanelsConfig, String> {
    save_panels_to(&panels_path(), config)
}

pub fn save_panels_to(
    path: &std::path::Path,
    config: &PanelsConfig,
) -> Result<PanelsConfig, String> {
    validate(config)?;
    let config = config.clone().clamped();

    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    }

    let body = serde_json::to_string_pretty(&to_disk(&config))
        .map_err(|e| format!("could not serialise the panels: {e}"))?;
    if body.len() as u64 > MAX_PANELS_BYTES {
        return Err(format!("panels are larger than {MAX_PANELS_BYTES} bytes"));
    }

    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, &body)
        .map_err(|e| format!("could not write {}: {e}", temp.display()))?;
    std::fs::rename(&temp, path)
        .map_err(|e| format!("could not replace {}: {e}", path.display()))?;

    Ok(config)
}

pub fn reset_panels() -> Result<PanelsConfig, String> {
    reset_panels_at(&panels_path())
}

/// The on-disk shape.
///
/// `blocks` is dropped from any panel that has a `layout`, because it is derived
/// from it. Writing both would let a hand edit to the derived view sit in the
/// file looking authoritative while being ignored on load.
fn to_disk(config: &PanelsConfig) -> Value {
    let mut value = serde_json::to_value(config).unwrap_or(Value::Null);
    if let Some(panels) = value.get_mut("panels").and_then(Value::as_array_mut) {
        for panel in panels {
            let Some(map) = panel.as_object_mut() else {
                continue;
            };
            if map.contains_key("layout") {
                map.remove("blocks");
            }
            // A widget with a `src` carries its html as a derived view. Writing
            // it back would put a second copy of the file in panels.json, and
            // the two would drift the moment either was edited.
            strip_derived_widget_html(map);
        }
    }
    value
}

/// Remove `html` from every widget block that has a `src`, at any depth.
fn strip_derived_widget_html(panel: &mut serde_json::Map<String, Value>) {
    fn walk(value: &mut Value) {
        match value {
            Value::Object(map) => {
                if map.get("kind").and_then(Value::as_str) == Some("widget")
                    && map.contains_key("src")
                {
                    map.remove("html");
                }
                for child in map.values_mut() {
                    walk(child);
                }
            }
            Value::Array(items) => {
                for item in items {
                    walk(item);
                }
            }
            _ => {}
        }
    }
    for key in ["blocks", "layout"] {
        if let Some(value) = panel.get_mut(key) {
            walk(value);
        }
    }
}

pub fn reset_panels_at(path: &std::path::Path) -> Result<PanelsConfig, String> {
    if path.exists() {
        std::fs::remove_file(path)
            .map_err(|e| format!("could not remove {}: {e}", path.display()))?;
    }
    Ok(PanelsConfig::default())
}

pub fn validate(config: &PanelsConfig) -> Result<(), String> {
    let mut problems: Vec<String> = Vec::new();

    if config.panels.len() > MAX_PANELS {
        problems.push(format!(
            "{} panels is more than the {MAX_PANELS} allowed",
            config.panels.len()
        ));
    }

    let mut seen: BTreeSet<&str> = BTreeSet::new();

    for (index, panel) in config.panels.iter().enumerate() {
        let label = if panel.id.trim().is_empty() {
            format!("panel {}", index + 1)
        } else {
            format!("panel `{}`", panel.id)
        };

        if panel.plugin.is_some() {
            problems.push(format!(
                "{label}: `plugin` is reserved for plugin-contributed panels"
            ));
        }

        if panel.id.starts_with(PLUGIN_ID_PREFIX) {
            problems.push(format!(
                "{label}: an id starting with `{PLUGIN_ID_PREFIX}` is reserved for plugin-contributed panels"
            ));
        } else if !is_id(&panel.id) {
            problems.push(format!(
                "{label}: id must be 1-{MAX_ID} lowercase letters, digits or dashes"
            ));
        } else if !seen.insert(panel.id.as_str()) {
            problems.push(format!("{label}: id is used twice"));
        }

        if let Err(reason) = check_text(&panel.title, MAX_TITLE, "title") {
            problems.push(format!("{label}: {reason}"));
        }

        let tree = panel.tree();
        let flat = tree.flatten();
        if flat.len() > MAX_BLOCKS {
            problems.push(format!(
                "{label}: {} blocks is more than the {MAX_BLOCKS} allowed",
                flat.len()
            ));
        }

        let mut budget = MAX_LAYOUT_NODES;
        check_node(&tree, 0, &mut budget, &label, &mut problems);

        // A widget may declare its own sources. They join the panel's data
        // plane under the panel's id, so the count and the uniqueness rule have
        // to see both lists — otherwise a widget could smuggle in a fifth
        // source or shadow one the panel already declared.
        let mut all_sources: Vec<&data::Source> = panel.sources.iter().collect();
        for block in &flat {
            if let PanelBlock::Widget { sources, .. } = &block.body {
                all_sources.extend(sources.iter());
            }
        }
        if all_sources.len() > data::MAX_SOURCES_PER_PANEL {
            problems.push(format!(
                "{label}: {} sources is more than the {} allowed",
                all_sources.len(),
                data::MAX_SOURCES_PER_PANEL
            ));
        }
        let mut source_ids: BTreeSet<&str> = BTreeSet::new();
        for source in all_sources {
            if let Err(reason) = data::check_source(source) {
                problems.push(format!("{label}, source `{}`: {reason}", source.id));
            }
            if !source_ids.insert(source.id.as_str()) {
                problems.push(format!(
                    "{label}: source id `{}` is used twice",
                    source.id
                ));
            }
        }
    }

    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("\n"))
    }
}

/// Walk the tree once, collecting every problem rather than stopping at the
/// first. The agent gets the whole list in one receipt and fixes it in one edit.
fn check_node(
    node: &Node,
    depth: usize,
    budget: &mut usize,
    at: &str,
    problems: &mut Vec<String>,
) {
    if depth > MAX_LAYOUT_DEPTH {
        problems.push(format!(
            "{at}: layout nests deeper than {MAX_LAYOUT_DEPTH} levels"
        ));
        return;
    }
    if *budget == 0 {
        problems.push(format!(
            "{at}: layout has more than {MAX_LAYOUT_NODES} nodes and blocks"
        ));
        return;
    }
    *budget -= 1;

    // `when` is a field on every node and every block, so it is checked here
    // once rather than in each arm below.
    if let Some(when) = node.when() {
        if let Err(reason) = when.check_condition() {
            problems.push(format!("{at}: {reason}"));
        }
    }

    match node {
        Node::Block(block) => {
            if let Err(reason) = check_block(&block.body) {
                problems.push(format!("{at}: {reason}"));
            }
        }
        Node::Layout(LayoutNode::Tabs { tabs, .. }) => {
            if tabs.is_empty() {
                problems.push(format!("{at}: a tabs node has no tabs"));
                return;
            }
            if tabs.len() > MAX_TABS {
                problems.push(format!(
                    "{at}: {} tabs is more than the {MAX_TABS} allowed",
                    tabs.len()
                ));
            }
            let mut labels: BTreeSet<&str> = BTreeSet::new();
            for (index, tab) in tabs.iter().enumerate() {
                let tab_at = format!("{at}, tab {}", index + 1);
                if let Err(reason) = check_text(&tab.label, MAX_TAB_LABEL, "tab label") {
                    problems.push(format!("{tab_at}: {reason}"));
                } else if !labels.insert(tab.label.as_str()) {
                    problems.push(format!("{tab_at}: label `{}` is used twice", tab.label));
                }
                if tab.children.is_empty() {
                    problems.push(format!("{tab_at}: tab is empty"));
                }
                for child in &tab.children {
                    check_node(child, depth + 1, budget, &tab_at, problems);
                }
            }
        }
        Node::Layout(LayoutNode::Split { children, weights, .. }) => {
            if children.len() < 2 {
                problems.push(format!("{at}: a split needs at least two children"));
            }
            if let Some(weights) = weights {
                if weights.len() != children.len() {
                    problems.push(format!(
                        "{at}: {} weights for {} children",
                        weights.len(),
                        children.len()
                    ));
                }
                if let Some(bad) = weights.iter().find(|w| **w == 0 || **w > MAX_WEIGHT) {
                    problems.push(format!(
                        "{at}: weight {bad} is outside 1..{MAX_WEIGHT}"
                    ));
                }
            }
            for (index, child) in children.iter().enumerate() {
                check_node(child, depth + 1, budget, &format!("{at}, child {}", index + 1), problems);
            }
        }
        Node::Layout(LayoutNode::Stack { children, .. }) => {
            if children.is_empty() {
                problems.push(format!("{at}: a stack has no children"));
            }
            for (index, child) in children.iter().enumerate() {
                check_node(child, depth + 1, budget, &format!("{at}, child {}", index + 1), problems);
            }
        }
    }
}

fn check_block(block: &PanelBlock) -> Result<(), String> {
    match block {
        PanelBlock::Text { text } => check_text(text, MAX_TEXT, "text"),
        PanelBlock::File { path, label } => {
            check_relative_path(path)?;
            if let Some(label) = label {
                check_text(label, MAX_TITLE, "label")?;
            }
            Ok(())
        }
        PanelBlock::List { label, items } => {
            if let Some(label) = label {
                check_text(label, MAX_TITLE, "label")?;
            }
            if items.len() > MAX_LIST_ITEMS {
                return Err(format!(
                    "{} items is more than the {MAX_LIST_ITEMS} allowed",
                    items.len()
                ));
            }
            for item in items {
                check_text(item, MAX_ITEM, "list item")?;
            }
            Ok(())
        }
        PanelBlock::Link { label, url } => {
            check_text(label, MAX_TITLE, "label")?;
            check_url(url)
        }
        PanelBlock::Actions { ids } => {
            if ids.is_empty() {
                return Err("actions block names no ids".into());
            }
            if ids.len() > MAX_LIST_ITEMS {
                return Err(format!(
                    "{} ids is more than the {MAX_LIST_ITEMS} allowed",
                    ids.len()
                ));
            }
            for id in ids {
                if !is_id(id) {
                    return Err(format!("`{id}` is not a valid action id"));
                }
            }
            Ok(())
        }
        PanelBlock::Sandbox {
            html,
            title,
            height,
        } => {
            if html.trim().is_empty() {
                return Err("sandbox html is empty".into());
            }
            if html.len() > MAX_SANDBOX_HTML {
                return Err(format!(
                    "sandbox html is {} bytes, more than the {MAX_SANDBOX_HTML} allowed",
                    html.len()
                ));
            }
            if let Some(title) = title {
                check_text(title, MAX_TITLE, "title")?;
            }
            if *height < SANDBOX_HEIGHT.0 || *height > SANDBOX_HEIGHT.1 {
                return Err(format!(
                    "sandbox height {height} is outside {}..{}",
                    SANDBOX_HEIGHT.0, SANDBOX_HEIGHT.1
                ));
            }
            let lowered = html.to_ascii_lowercase();
            if let Some(token) = SANDBOX_FORBIDDEN
                .iter()
                .find(|token| lowered.contains(*token))
            {
                return Err(format!(
                    "sandbox html contains `{token}`; a sandbox block has no network and cannot embed frames or forms"
                ));
            }
            Ok(())
        }
        PanelBlock::Widget {
            html,
            src,
            title,
            height,
            capabilities,
            sources,
        } => {
            if html.is_none() && src.is_none() {
                return Err("widget has neither `html` nor `src`".into());
            }
            if let Some(src) = src {
                check_widget_src(src)?;
            }
            // `html` is derived from `src` at load time, so it is checked
            // whenever it is present — which, after a load, is always.
            if let Some(html) = html {
                if html.trim().is_empty() {
                    return Err("widget html is empty".into());
                }
                if html.len() > MAX_WIDGET_HTML {
                    return Err(format!(
                        "widget html is {} bytes, more than the {MAX_WIDGET_HTML} allowed",
                        html.len()
                    ));
                }
            }
            // The sources themselves are checked at panel level, where the
            // count and the id-uniqueness rule can see the panel's own sources
            // too. Checking here as well would report the same problem twice.
            let _ = sources;
            if let Some(title) = title {
                check_text(title, MAX_TITLE, "title")?;
            }
            if *height < WIDGET_HEIGHT.0 || *height > WIDGET_HEIGHT.1 {
                return Err(format!(
                    "widget height {height} is outside {}..{}",
                    WIDGET_HEIGHT.0, WIDGET_HEIGHT.1
                ));
            }
            if capabilities.len() > MAX_WIDGET_CAPABILITIES {
                return Err(format!(
                    "{} capabilities is more than the {MAX_WIDGET_CAPABILITIES} allowed",
                    capabilities.len()
                ));
            }
            let mut seen: BTreeSet<String> = BTreeSet::new();
            for capability in capabilities {
                if let Capability::Action { id } = capability {
                    if !is_id(id) {
                        return Err(format!("`{id}` is not a valid action id"));
                    }
                }
                let name = capability.name();
                if !seen.insert(name.clone()) {
                    return Err(format!("capability `{name}` is declared twice"));
                }
            }
            if let Some(html) = html {
                let lowered = html.to_ascii_lowercase();
                if let Some(token) = WIDGET_FORBIDDEN
                    .iter()
                    .find(|token| lowered.contains(*token))
                {
                    return Err(format!(
                        "widget html contains `{token}`; a widget has no network, so an external script, stylesheet, font, image or frame would load nothing"
                    ));
                }
            }
            Ok(())
        }
        PanelBlock::Data { label, value, hint } => {
            check_text(label, MAX_TITLE, "label")?;
            if let Some(hint) = hint {
                check_text(hint, MAX_ITEM, "hint")?;
            }
            value.check().map(|_| ())
        }
        PanelBlock::Chart {
            label,
            style,
            values,
            series,
            x,
            y_min,
            y_max,
            unit,
            stack,
            scale,
            reference,
            ..
        } => {
            check_text(label, MAX_TITLE, "label")?;
            if values.is_none() && series.is_empty() {
                return Err("chart has neither `values` nor `series`".into());
            }
            if !stack.fits(*style) {
                return Err(
                    "chart `stack` is not `none` but `style` is not `bar` or `area`; a line has no thickness to accumulate"
                        .into(),
                );
            }
            if reference.len() > MAX_CHART_REFERENCES {
                return Err(format!(
                    "chart has {} reference lines; the limit is {MAX_CHART_REFERENCES}",
                    reference.len()
                ));
            }
            for (index, one) in reference.iter().enumerate() {
                one.value.check()?;
                if let Some(label) = &one.label {
                    check_text(label, MAX_ITEM, &format!("reference {index} label"))?;
                }
                if let Some(tone) = &one.tone {
                    tone.check()?;
                }
            }
            if *scale == ChartScale::Log && y_min.is_some() {
                // A log axis cannot start at zero, and an explicit `yMin` of
                // zero is the most likely thing to write. Say so rather than
                // silently dropping the bound.
                return Err("chart `scale` is `log`; `yMin` must be positive or omitted".into());
            }
            if series.len() > MAX_CHART_SERIES {
                return Err(format!(
                    "chart has {} series; the limit is {MAX_CHART_SERIES}",
                    series.len()
                ));
            }
            if let Some(values) = values {
                values.check()?;
            }
            for (index, one) in series.iter().enumerate() {
                if let Some(name) = &one.name {
                    check_text(name, MAX_TITLE, &format!("series {index} name"))?;
                }
                one.values.check()?;
            }
            if let Some(x) = x {
                x.check()?;
            }
            if let Some(y) = y_min {
                y.check()?;
            }
            if let Some(y) = y_max {
                y.check()?;
            }
            if let Some(unit) = unit {
                check_text(unit, MAX_UNIT, "unit")?;
            }
            Ok(())
        }
        PanelBlock::Diagram {
            label,
            nodes,
            edges,
            height,
            ..
        } => {
            check_text(label, MAX_TITLE, "label")?;
            if nodes.is_empty() {
                return Err("diagram has no nodes".into());
            }
            if nodes.len() > MAX_DIAGRAM_NODES {
                return Err(format!(
                    "diagram has {} nodes; the limit is {MAX_DIAGRAM_NODES}",
                    nodes.len()
                ));
            }
            if edges.len() > MAX_DIAGRAM_EDGES {
                return Err(format!(
                    "diagram has {} edges; the limit is {MAX_DIAGRAM_EDGES}",
                    edges.len()
                ));
            }
            if let Some(height) = height {
                if *height < DIAGRAM_HEIGHT.0 || *height > DIAGRAM_HEIGHT.1 {
                    return Err(format!(
                        "diagram height {height} is outside {}..{}",
                        DIAGRAM_HEIGHT.0, DIAGRAM_HEIGHT.1
                    ));
                }
            }

            let mut seen: Vec<&str> = Vec::with_capacity(nodes.len());
            for (index, node) in nodes.iter().enumerate() {
                if node.id.is_empty() {
                    return Err(format!("node {index} has no id"));
                }
                if node.id.len() > MAX_DIAGRAM_ID {
                    return Err(format!(
                        "node id `{}` is longer than {MAX_DIAGRAM_ID} characters",
                        node.id
                    ));
                }
                if !node
                    .id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                {
                    return Err(format!(
                        "node id `{}` may only use letters, digits, `-` and `_`",
                        node.id
                    ));
                }
                if seen.contains(&node.id.as_str()) {
                    return Err(format!("node id `{}` is used twice", node.id));
                }
                seen.push(&node.id);

                node.label.check()?;
                if let Some(detail) = &node.detail {
                    detail.check()?;
                }
                if let Some(tone) = &node.tone {
                    tone.check()?;
                }
                if let Some(when) = &node.when {
                    when.check()?;
                }
                if let Some(action) = &node.action {
                    if !is_id(action) {
                        return Err(format!("`{action}` is not a valid action id"));
                    }
                }
            }

            let mut edge_ids: Vec<&str> = Vec::with_capacity(edges.len());
            for (index, edge) in edges.iter().enumerate() {
                if let Some(id) = &edge.id {
                    if id.is_empty() || id.len() > MAX_DIAGRAM_ID {
                        return Err(format!(
                            "edge {index} id must be 1..{MAX_DIAGRAM_ID} characters"
                        ));
                    }
                    if edge_ids.contains(&id.as_str()) {
                        return Err(format!("edge id `{id}` is used twice"));
                    }
                    edge_ids.push(id);
                }
                if !seen.contains(&edge.from.as_str()) {
                    return Err(format!(
                        "edge {index} starts at `{}`, which is not a node in this diagram",
                        edge.from
                    ));
                }
                if !seen.contains(&edge.to.as_str()) {
                    return Err(format!(
                        "edge {index} ends at `{}`, which is not a node in this diagram",
                        edge.to
                    ));
                }
                if let Some(label) = &edge.label {
                    label.check()?;
                }
            }
            Ok(())
        }
        PanelBlock::Table {
            label,
            columns,
            rows,
        } => {
            if let Some(label) = label {
                check_text(label, MAX_TITLE, "label")?;
            }
            if columns.is_empty() {
                return Err("table has no columns".into());
            }
            if columns.len() > MAX_COLUMNS {
                return Err(format!(
                    "{} columns is more than the {MAX_COLUMNS} allowed",
                    columns.len()
                ));
            }
            let mut seen: BTreeSet<&str> = BTreeSet::new();
            for column in columns {
                check_text(column, MAX_TITLE, "column")?;
                if !seen.insert(column.as_str()) {
                    return Err(format!("column `{column}` is used twice"));
                }
            }
            rows.check().map(|_| ())
        }
        PanelBlock::Progress { label, value, max } => {
            check_text(label, MAX_TITLE, "label")?;
            for (side, expr) in [("value", Some(value)), ("max", max.as_ref())] {
                let Some(expr) = expr else { continue };
                let ty = expr.check()?;
                if !matches!(ty, Type::Any | Type::Number) {
                    return Err(format!(
                        "progress {side} must be a number, but it is {}",
                        match ty {
                            Type::String => "a string",
                            Type::Bool => "a boolean",
                            Type::Null => "null",
                            _ => "not a number",
                        }
                    ));
                }
            }
            Ok(())
        }
    }
}

fn check_url(url: &str) -> Result<(), String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err("url is empty".into());
    }
    if trimmed.len() > MAX_URL {
        return Err(format!("url is longer than {MAX_URL} characters"));
    }
    if trimmed.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("url contains whitespace or control characters".into());
    }
    let lower = trimmed.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return Err("url must start with http:// or https://".into());
    }
    let after_scheme = &trimmed[trimmed.find("//").map(|i| i + 2).unwrap_or(0)..];
    let authority = after_scheme.split(['/', '?', '#']).next().unwrap_or("");
    if authority.contains('@') {
        return Err("url must not embed credentials".into());
    }
    Ok(())
}

fn check_relative_path(path: &str) -> Result<(), String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("path is empty".into());
    }
    if trimmed.len() > MAX_PATH {
        return Err(format!("path is longer than {MAX_PATH} characters"));
    }
    if trimmed.chars().any(|c| c.is_control()) {
        return Err("path contains control characters".into());
    }
    if trimmed.starts_with('/') || trimmed.starts_with('\\') || trimmed.starts_with('~') {
        return Err("path must be relative to the workspace".into());
    }
    if trimmed.contains(':') {
        return Err("path must not contain `:`".into());
    }
    if trimmed.split(['/', '\\']).any(|segment| segment == "..") {
        return Err("path must not climb out of the workspace with `..`".into());
    }
    Ok(())
}

/// A widget `src` is a path under `<config>/widgets/`.
///
/// Narrower than `check_relative_path` on purpose: this file is read by the app
/// rather than by the user, so it must live in the one directory the app owns
/// and it must be html.
fn check_widget_src(src: &str) -> Result<(), String> {
    check_relative_path(src)?;
    if !src.starts_with("widgets/") {
        return Err("widget `src` must start with `widgets/`".into());
    }
    if !src.to_ascii_lowercase().ends_with(".html") {
        return Err("widget `src` must end with `.html`".into());
    }
    Ok(())
}

fn check_text(value: &str, max: usize, what: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("{what} is empty"));
    }
    if value.chars().count() > max {
        return Err(format!("{what} is longer than {max} characters"));
    }
    if value
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err(format!("{what} contains control characters"));
    }
    Ok(())
}

fn is_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn panel(id: &str, blocks: Vec<PanelBlock>) -> CustomPanel {
        CustomPanel {
            id: id.into(),
            title: "Notes".into(),
            icon: None,
            width: 320,
            layout: None,
            blocks: blocks.into_iter().map(Block::new).collect(),
            sources: Vec::new(),
            plugin: None,
        }
    }

    fn tree_panel(id: &str, layout: Node) -> CustomPanel {
        CustomPanel {
            id: id.into(),
            title: "Notes".into(),
            icon: None,
            width: 320,
            layout: Some(layout),
            blocks: Vec::new(),
            sources: Vec::new(),
            plugin: None,
        }
    }

    fn text(body: &str) -> PanelBlock {
        PanelBlock::Text { text: body.into() }
    }

    fn config(panels: Vec<CustomPanel>) -> PanelsConfig {
        PanelsConfig { panels }
    }

    #[test]
    fn accepts_one_of_each_block() {
        let config = config(vec![panel(
            "notes",
            vec![
                PanelBlock::Text {
                    text: "A paragraph.".into(),
                },
                PanelBlock::File {
                    path: "README.md".into(),
                    label: None,
                },
                PanelBlock::List {
                    label: Some("Steps".into()),
                    items: vec!["one".into(), "two".into()],
                },
                PanelBlock::Link {
                    label: "Docs".into(),
                    url: "https://docs.mistral.ai".into(),
                },
                PanelBlock::Actions {
                    ids: vec!["review".into()],
                },
                widget("<canvas></canvas>", 240, vec![Capability::Prompt]),
            ],
        )]);
        assert!(validate(&config).is_ok());
    }

    #[test]
    fn rejects_an_unknown_block_kind() {
        let raw = r#"{"panels":[{"id":"x","title":"X","blocks":[{"kind":"html","html":"<script>"}]}]}"#;
        assert!(serde_json::from_str::<PanelsConfig>(raw).is_err());
    }

    fn sandbox(html: &str, height: u16) -> PanelBlock {
        PanelBlock::Sandbox {
            html: html.into(),
            title: None,
            height,
        }
    }

    #[test]
    fn a_sandbox_block_round_trips_and_defaults_its_height() {
        let raw = r#"{"panels":[{"id":"p","title":"P","blocks":[{"kind":"sandbox","html":"<p>hi</p>"}]}]}"#;
        let parsed: PanelsConfig = serde_json::from_str(raw).unwrap();
        match &parsed.panels[0].blocks[0].body {
            PanelBlock::Sandbox { height, .. } => assert_eq!(*height, 160),
            other => panic!("expected a sandbox block, got {other:?}"),
        }
        assert!(validate(&parsed).is_ok());
    }

    /// The platform constraint this block is built around: a frame cannot run
    /// scripts the app itself may not run, so a `scripts` field would be a lie.
    /// A file that carries one is ignored rather than honoured.
    #[test]
    fn a_sandbox_block_has_no_way_to_ask_for_scripts() {
        let raw = r#"{"panels":[{"id":"p","title":"P","blocks":[{"kind":"sandbox","html":"<p>hi</p>","scripts":true}]}]}"#;
        let parsed: PanelsConfig = serde_json::from_str(raw).unwrap();
        let json = serde_json::to_string(&parsed).unwrap();
        assert!(!json.contains("scripts"));
    }

    #[test]
    fn a_sandbox_block_without_network_or_frames_is_accepted() {
        let config = config(vec![panel(
            "p",
            vec![sandbox(
                "<style>p{color:#ff7a1a}</style><p>42</p><img src=\"data:image/gif;base64,R0lGOD\">",
                200,
            )],
        )]);
        assert!(validate(&config).is_ok());
    }

    #[test]
    fn a_sandbox_block_cannot_reach_the_network_or_embed_a_frame() {
        for html in [
            "<link rel=\"stylesheet\" href=\"https://cdn.example/x.css\">",
            "<script src=\"https://cdn.example/x.js\"></script>",
            "<SCRIPT SRC=\"https://cdn.example/x.js\"></SCRIPT>",
            "<style>@import url(//fonts.example/x.css);</style>",
            "<iframe src=\"about:blank\"></iframe>",
            "<object data=\"x\"></object>",
            "<embed src=\"x\">",
            "<form action=\"x\"><input></form>",
            "<a href=\"javascript:alert(1)\">x</a>",
            "<img src=\"http://example.com/x.png\">",
        ] {
            let config = config(vec![panel("p", vec![sandbox(html, 160)])]);
            assert!(validate(&config).is_err(), "{html} should be rejected");
        }
    }

    #[test]
    fn a_sandbox_block_is_size_limited() {
        let big = "x".repeat(MAX_SANDBOX_HTML + 1);
        let oversized = config(vec![panel("p", vec![sandbox(&big, 160)])]);
        assert!(validate(&oversized).is_err());

        let empty = config(vec![panel("p", vec![sandbox("   ", 160)])]);
        assert!(validate(&empty).is_err());
    }

    #[test]
    fn a_sandbox_height_is_clamped_and_idempotent() {
        let tall = config(vec![panel("p", vec![sandbox("<p>x</p>", 9_000)])]).clamped();
        let short = config(vec![panel("p", vec![sandbox("<p>x</p>", 1)])]).clamped();

        let height_of = |value: &PanelsConfig| match &value.panels[0].blocks[0].body {
            PanelBlock::Sandbox { height, .. } => *height,
            other => panic!("expected a sandbox block, got {other:?}"),
        };

        assert_eq!(height_of(&tall), SANDBOX_HEIGHT.1);
        assert_eq!(height_of(&short), SANDBOX_HEIGHT.0);
        assert_eq!(height_of(&tall.clone().clamped()), height_of(&tall));
    }

    #[test]
    fn an_out_of_range_sandbox_height_is_refused_before_it_is_clamped() {
        // `clamped()` runs on load, so a file that reaches `validate` unclamped
        // is a file written by something that skipped the loader.
        let config = config(vec![panel("p", vec![sandbox("<p>x</p>", 9_000)])]);
        assert!(validate(&config).is_err());
    }

    fn widget(html: &str, height: u16, capabilities: Vec<Capability>) -> PanelBlock {
        PanelBlock::Widget {
            html: Some(html.into()),
            src: None,
            title: None,
            height,
            capabilities,
            sources: Vec::new(),
        }
    }

    #[test]
    fn a_widget_block_round_trips_and_defaults_its_height() {
        let raw = r#"{"panels":[{"id":"p","title":"P","blocks":[{"kind":"widget","html":"<canvas></canvas>"}]}]}"#;
        let parsed: PanelsConfig = serde_json::from_str(raw).unwrap();
        match &parsed.panels[0].blocks[0].body {
            PanelBlock::Widget {
                height,
                capabilities,
                ..
            } => {
                assert_eq!(*height, 240);
                assert!(capabilities.is_empty());
            }
            other => panic!("expected a widget block, got {other:?}"),
        }
        assert!(validate(&parsed).is_ok());
    }

    #[test]
    fn a_widget_capability_round_trips_as_a_tagged_object() {
        let raw = r#"{"panels":[{"id":"p","title":"P","blocks":[{"kind":"widget","html":"<p>x</p>","capabilities":[{"kind":"prompt"},{"kind":"action","id":"refresh"}]}]}]}"#;
        let parsed: PanelsConfig = serde_json::from_str(raw).unwrap();
        let PanelBlock::Widget { capabilities, .. } = &parsed.panels[0].blocks[0].body else {
            panic!("expected a widget block");
        };
        assert_eq!(
            capabilities,
            &vec![
                Capability::Prompt,
                Capability::Action {
                    id: "refresh".into()
                }
            ]
        );
        assert_eq!(capabilities[0].name(), "prompt");
        assert_eq!(capabilities[1].name(), "action:refresh");
        assert!(validate(&parsed).is_ok());
    }

    #[test]
    fn a_widget_may_run_its_own_scripts() {
        // The whole difference between `sandbox` and `widget`: inline script is
        // the point, and it is not refused.
        let config = config(vec![panel(
            "p",
            vec![widget(
                "<canvas id=\"c\"></canvas><script>document.title='x'</script>",
                240,
                Vec::new(),
            )],
        )]);
        assert!(validate(&config).is_ok());
    }

    #[test]
    fn a_widget_cannot_load_anything_from_the_network() {
        for html in [
            "<script src=\"https://cdn.example/x.js\"></script>",
            "<SCRIPT SRC=\"https://cdn.example/x.js\"></SCRIPT>",
            "<link rel=\"stylesheet\" href=\"x.css\">",
            "<style>@import url(//fonts.example/x.css);</style>",
            "<style>body{background:url(http://x/y.png)}</style>",
            "<img src=\"http://example.com/x.png\">",
            "<img src='http://example.com/x.png'>",
            "<iframe src=\"about:blank\"></iframe>",
            "<object data=\"x\"></object>",
            "<embed src=\"x\">",
            "<form action=\"x\"><input></form>",
        ] {
            let config = config(vec![panel("p", vec![widget(html, 240, Vec::new())])]);
            assert!(validate(&config).is_err(), "{html} should be rejected");
        }
    }

    #[test]
    fn a_widget_is_size_limited() {
        let big = "x".repeat(MAX_WIDGET_HTML + 1);
        assert!(validate(&config(vec![panel(
            "p",
            vec![widget(&big, 240, Vec::new())]
        )]))
        .is_err());
        assert!(validate(&config(vec![panel(
            "p",
            vec![widget("   ", 240, Vec::new())]
        )]))
        .is_err());
    }

    #[test]
    fn a_widget_height_is_clamped_and_idempotent() {
        let tall = config(vec![panel("p", vec![widget("<p>x</p>", 9_000, Vec::new())])]).clamped();
        let short = config(vec![panel("p", vec![widget("<p>x</p>", 1, Vec::new())])]).clamped();

        let height_of = |value: &PanelsConfig| match &value.panels[0].blocks[0].body {
            PanelBlock::Widget { height, .. } => *height,
            other => panic!("expected a widget block, got {other:?}"),
        };

        assert_eq!(height_of(&tall), WIDGET_HEIGHT.1);
        assert_eq!(height_of(&short), WIDGET_HEIGHT.0);
        assert_eq!(height_of(&tall.clone().clamped()), height_of(&tall));
    }

    #[test]
    fn an_out_of_range_widget_height_is_refused_before_it_is_clamped() {
        let config = config(vec![panel("p", vec![widget("<p>x</p>", 9_000, Vec::new())])]);
        assert!(validate(&config).is_err());
    }

    #[test]
    fn a_widget_capability_list_is_checked() {
        // A duplicate is a mistake the agent should see, not a silent no-op.
        let duplicate = config(vec![panel(
            "p",
            vec![widget(
                "<p>x</p>",
                240,
                vec![Capability::Prompt, Capability::Prompt],
            )],
        )]);
        assert!(validate(&duplicate).is_err());

        let duplicate_action = config(vec![panel(
            "p",
            vec![widget(
                "<p>x</p>",
                240,
                vec![
                    Capability::Action { id: "go".into() },
                    Capability::Action { id: "go".into() },
                ],
            )],
        )]);
        assert!(validate(&duplicate_action).is_err());

        // An action id has to be one `actions.json` could actually hold.
        let bad_id = config(vec![panel(
            "p",
            vec![widget(
                "<p>x</p>",
                240,
                vec![Capability::Action {
                    id: "Not An Id".into(),
                }],
            )],
        )]);
        assert!(validate(&bad_id).is_err());

        let too_many = config(vec![panel(
            "p",
            vec![widget(
                "<p>x</p>",
                240,
                (0..=MAX_WIDGET_CAPABILITIES)
                    .map(|index| Capability::Action {
                        id: format!("a{index}"),
                    })
                    .collect(),
            )],
        )]);
        assert!(validate(&too_many).is_err());
    }

    #[test]
    fn a_widget_may_point_at_a_file_instead_of_carrying_html() {
        // The shape a real widget wants: a sibling file, reviewable in a diff,
        // instead of a 64 KB escaped string inside panels.json.
        let ok = config(vec![panel(
            "p",
            vec![PanelBlock::Widget {
                html: None,
                src: Some("widgets/context-ring.html".into()),
                title: None,
                height: 240,
                capabilities: Vec::new(),
                sources: Vec::new(),
            }],
        )]);
        assert!(validate(&ok).is_ok());

        for src in [
            "../widgets/x.html",
            "/widgets/x.html",
            "widgets/../../x.html",
            "x.html",
            "widgets/x.js",
            "widgets/x.html.exe",
        ] {
            let bad = config(vec![panel(
                "p",
                vec![PanelBlock::Widget {
                    html: None,
                    src: Some(src.into()),
                    title: None,
                    height: 240,
                    capabilities: Vec::new(),
                    sources: Vec::new(),
                }],
            )]);
            assert!(validate(&bad).is_err(), "{src} should be rejected");
        }
    }

    #[test]
    fn a_widget_with_neither_html_nor_src_is_refused() {
        let raw = r#"{"panels":[{"id":"p","title":"P","blocks":[{"kind":"widget"}]}]}"#;
        let config: PanelsConfig = serde_json::from_str(raw).unwrap();
        assert!(validate(&config).is_err());
    }

    #[test]
    fn a_widget_source_joins_the_panel_source_budget() {
        // A widget could otherwise smuggle in a fifth source past the limit.
        let panel_sources: Vec<data::Source> = (0..data::MAX_SOURCES_PER_PANEL)
            .map(|index| data::Source {
                id: format!("s{index}"),
                kind: data::SourceKind::Builtin { name: None },
            })
            .collect();
        let mut one = panel("p", vec![]);
        one.sources = panel_sources;
        one.blocks = vec![Block::new(PanelBlock::Widget {
            html: Some("<p>x</p>".into()),
            src: None,
            title: None,
            height: 240,
            capabilities: Vec::new(),
            sources: vec![data::Source {
                id: "extra".into(),
                kind: data::SourceKind::Builtin { name: None },
            }],
        })];
        assert!(validate(&config(vec![one])).is_err());
    }

    #[test]
    fn a_widget_source_may_not_shadow_a_panel_source() {
        let mut one = panel("p", vec![]);
        one.sources = vec![data::Source {
            id: "clock".into(),
            kind: data::SourceKind::Builtin { name: None },
        }];
        one.blocks = vec![Block::new(PanelBlock::Widget {
            html: Some("<p>x</p>".into()),
            src: None,
            title: None,
            height: 240,
            capabilities: Vec::new(),
            sources: vec![data::Source {
                id: "clock".into(),
                kind: data::SourceKind::Builtin { name: None },
            }],
        })];
        assert!(validate(&config(vec![one])).is_err());
    }

    #[test]
    fn a_widget_may_declare_the_file_capability() {
        let config = config(vec![panel(
            "p",
            vec![widget("<p>x</p>", 240, vec![Capability::File])],
        )]);
        assert!(validate(&config).is_ok());
        assert_eq!(Capability::File.name(), "file");
    }

    #[test]
    fn a_widget_src_is_read_into_html_on_load_and_not_written_back() {
        let dir = std::env::temp_dir().join(format!("vibedesk-widget-src-{}", std::process::id()));
        let widgets = dir.join("widgets");
        std::fs::create_dir_all(&widgets).unwrap();
        std::fs::write(widgets.join("ring.html"), "<canvas id=\"ring\"></canvas>").unwrap();
        let path = dir.join("panels.json");
        std::fs::write(
            &path,
            r#"{"panels":[{"id":"p","title":"P","blocks":[{"kind":"widget","src":"widgets/ring.html"}]}]}"#,
        )
        .unwrap();

        let loaded = load_panels_from(&path).unwrap();
        match &loaded.panels[0].blocks[0].body {
            PanelBlock::Widget { html, src, .. } => {
                assert_eq!(html.as_deref(), Some("<canvas id=\"ring\"></canvas>"));
                assert_eq!(src.as_deref(), Some("widgets/ring.html"));
            }
            other => panic!("expected a widget, got {other:?}"),
        }

        // Saving must not copy the file into panels.json: the two would drift.
        save_panels_to(&path, &loaded).unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("widgets/ring.html"), "{written}");
        assert!(!written.contains("canvas"), "the derived html was written back: {written}");

        // A missing file is a load error naming the file, not a blank frame.
        std::fs::remove_file(widgets.join("ring.html")).unwrap();
        let error = load_panels_from(&path).unwrap_err();
        assert!(error.contains("ring.html"), "{error}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_widget_cannot_declare_a_capability_that_does_not_exist() {
        let raw = r#"{"panels":[{"id":"p","title":"P","blocks":[{"kind":"widget","html":"<p>x</p>","capabilities":[{"kind":"shell"}]}]}]}"#;
        assert!(serde_json::from_str::<PanelsConfig>(raw).is_err());
    }

    #[test]
    fn rejects_a_file_block_that_escapes_the_workspace() {
        for path in ["../x", "/etc/passwd", "C:/Windows", "~/x", "a/../../b"] {
            let config = config(vec![panel(
                "p",
                vec![PanelBlock::File {
                    path: path.into(),
                    label: None,
                }],
            )]);
            assert!(validate(&config).is_err(), "{path} should be rejected");
        }
    }

    #[test]
    fn rejects_a_non_http_link() {
        let config = config(vec![panel(
            "p",
            vec![PanelBlock::Link {
                label: "x".into(),
                url: "javascript:alert(1)".into(),
            }],
        )]);
        assert!(validate(&config).is_err());
    }

    #[test]
    fn clamps_width_and_is_idempotent() {
        let wide = config(vec![CustomPanel {
            width: 9_000,
            ..panel("p", vec![])
        }])
        .clamped();
        assert_eq!(wide.panels[0].width, PANEL_WIDTH.1);

        let narrow = config(vec![CustomPanel {
            width: 1,
            ..panel("p", vec![])
        }])
        .clamped();
        assert_eq!(narrow.panels[0].width, PANEL_WIDTH.0);

        assert_eq!(wide.clone().clamped().panels[0].width, wide.panels[0].width);
    }

    #[test]
    fn rejects_duplicate_ids_and_bad_action_ids() {
        let dup = config(vec![panel("same", vec![]), panel("same", vec![])]);
        assert!(validate(&dup).is_err());

        let bad = config(vec![panel(
            "p",
            vec![PanelBlock::Actions {
                ids: vec!["Not An Id".into()],
            }],
        )]);
        assert!(validate(&bad).is_err());
    }

    #[test]
    fn a_file_block_has_no_way_to_ask_for_an_eager_read() {
        // The invariant the user asked for: a config cannot opt itself into
        // reading the workspace on open. An `expanded` field is not part of the
        // schema, so it is ignored rather than honoured.
        let raw = r#"{"panels":[{"id":"p","title":"P","blocks":[{"kind":"file","path":"README.md","expanded":true,"content":"leaked"}]}]}"#;
        let parsed: PanelsConfig = serde_json::from_str(raw).unwrap();
        assert!(validate(&parsed).is_ok());
        // Round-tripping drops the unknown fields, so nothing carries content.
        let json = serde_json::to_string(&parsed).unwrap();
        assert!(!json.contains("expanded"));
        assert!(!json.contains("leaked"));
    }

    #[test]
    fn round_trips_a_file_and_treats_a_missing_one_as_empty() {
        let dir = std::env::temp_dir().join(format!("vibedesk-panels-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("panels.json");

        std::fs::write(
            &path,
            r#"{"panels":[{"id":"p","title":"P","width":9999,"blocks":[{"kind":"text","text":"hi"}]}]}"#,
        )
        .unwrap();
        let loaded = load_panels_from(&path).unwrap();
        assert_eq!(loaded.panels[0].width, PANEL_WIDTH.1);

        std::fs::remove_file(&path).unwrap();
        assert!(load_panels_from(&path).unwrap().panels.is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }

    // ---- layout tree -------------------------------------------------------

    #[test]
    fn a_v1_panel_is_upgraded_to_a_single_stack() {
        let raw = r#"{"panels":[{"id":"p","title":"P","blocks":[{"kind":"text","text":"one"},{"kind":"text","text":"two"}]}]}"#;
        let parsed: PanelsConfig = serde_json::from_str(raw).unwrap();
        let upgraded = parsed.clamped();

        let layout = upgraded.panels[0].layout.as_ref().expect("a layout");
        match layout {
            Node::Layout(LayoutNode::Stack { children, .. }) => assert_eq!(children.len(), 2),
            other => panic!("expected a stack, got {other:?}"),
        }
        assert_eq!(upgraded.panels[0].blocks.len(), 2);
        validate(&upgraded).unwrap();
    }

    #[test]
    fn a_v2_panel_derives_its_blocks_from_the_layout() {
        let raw = r#"{"panels":[{"id":"p","title":"P","layout":{"node":"tabs","tabs":[
            {"label":"One","children":[{"kind":"text","text":"a"}]},
            {"label":"Two","children":[{"kind":"text","text":"b"},{"kind":"text","text":"c"}]}
        ]}}]}"#;
        let parsed: PanelsConfig = serde_json::from_str(raw).unwrap();
        let upgraded = parsed.clamped();

        let texts: Vec<String> = upgraded.panels[0]
            .blocks
            .iter()
            .map(|block| match &block.body {
                PanelBlock::Text { text } => text.clone(),
                other => panic!("expected text, got {other:?}"),
            })
            .collect();
        assert_eq!(texts, vec!["a", "b", "c"]);
        validate(&upgraded).unwrap();
    }

    #[test]
    fn a_hand_edited_blocks_view_is_ignored_when_a_layout_exists() {
        let raw = r#"{"panels":[{"id":"p","title":"P",
            "layout":{"node":"stack","children":[{"kind":"text","text":"real"}]},
            "blocks":[{"kind":"text","text":"stale"}]}]}"#;
        let parsed: PanelsConfig = serde_json::from_str(raw).unwrap();
        let upgraded = parsed.clamped();

        assert_eq!(upgraded.panels[0].blocks.len(), 1);
        match &upgraded.panels[0].blocks[0].body {
            PanelBlock::Text { text } => assert_eq!(text, "real"),
            other => panic!("expected text, got {other:?}"),
        }
    }

    #[test]
    fn blocks_are_not_written_to_disk_when_a_layout_exists() {
        let config = config(vec![tree_panel(
            "p",
            Node::Layout(LayoutNode::Stack {
                children: vec![Node::Block(Block::new(text("hi")))],
                when: None,
            }),
        )]);
        let disk = to_disk(&config);
        let panel = &disk["panels"][0];
        assert!(panel.get("layout").is_some());
        assert!(panel.get("blocks").is_none(), "{panel}");
    }

    #[test]
    fn a_v1_panel_keeps_its_blocks_on_disk() {
        let config = config(vec![panel("p", vec![text("hi")])]);
        let disk = to_disk(&config);
        let panel = &disk["panels"][0];
        assert!(panel.get("blocks").is_some(), "{panel}");
        assert!(panel.get("layout").is_none(), "{panel}");
    }

    #[test]
    fn a_node_with_neither_tag_says_what_it_needs() {
        let raw = r#"{"panels":[{"id":"p","title":"P","layout":{"children":[]}}]}"#;
        let error = serde_json::from_str::<PanelsConfig>(raw)
            .unwrap_err()
            .to_string();
        assert!(error.contains("`node`"), "{error}");
        assert!(error.contains("`kind`"), "{error}");
    }

    #[test]
    fn a_split_needs_two_children() {
        let config = config(vec![tree_panel(
            "p",
            Node::Layout(LayoutNode::Split {
                children: vec![Node::Block(Block::new(text("only")))],
                weights: None,
                when: None,
            }),
        )]);
        let error = validate(&config).unwrap_err();
        assert!(error.contains("at least two children"), "{error}");
    }

    #[test]
    fn split_weights_must_match_the_children_and_stay_in_range() {
        let children = || {
            vec![
                Node::Block(Block::new(text("a"))),
                Node::Block(Block::new(text("b"))),
            ]
        };

        let mismatched = config(vec![tree_panel(
            "p",
            Node::Layout(LayoutNode::Split {
                children: children(),
                weights: Some(vec![1]),
                when: None,
            }),
        )]);
        assert!(validate(&mismatched).unwrap_err().contains("weights for"));

        let out_of_range = config(vec![tree_panel(
            "p",
            Node::Layout(LayoutNode::Split {
                children: children(),
                weights: Some(vec![1, 99]),
                when: None,
            }),
        )]);
        assert!(validate(&out_of_range).unwrap_err().contains("outside 1.."));

        let fine = config(vec![tree_panel(
            "p",
            Node::Layout(LayoutNode::Split {
                children: children(),
                weights: Some(vec![2, 1]),
                when: None,
            }),
        )]);
        validate(&fine).unwrap();
    }

    #[test]
    fn a_tab_label_must_be_unique_and_a_tab_must_not_be_empty() {
        let duplicate = config(vec![tree_panel(
            "p",
            Node::Layout(LayoutNode::Tabs {
                tabs: vec![
                    Tab {
                        label: "Same".into(),
                        children: vec![Node::Block(Block::new(text("a")))],
                    },
                    Tab {
                        label: "Same".into(),
                        children: vec![Node::Block(Block::new(text("b")))],
                    },
                ],
                when: None,
            }),
        )]);
        assert!(validate(&duplicate).unwrap_err().contains("used twice"));

        let empty = config(vec![tree_panel(
            "p",
            Node::Layout(LayoutNode::Tabs {
                tabs: vec![Tab {
                    label: "One".into(),
                    children: vec![],
                }],
                when: None,
            }),
        )]);
        assert!(validate(&empty).unwrap_err().contains("tab is empty"));
    }

    #[test]
    fn a_tabs_node_with_no_tabs_is_refused() {
        let config = config(vec![tree_panel(
            "p",
            Node::Layout(LayoutNode::Tabs { tabs: vec![], when: None }),
        )]);
        assert!(validate(&config).unwrap_err().contains("no tabs"));
    }

    #[test]
    fn layout_depth_is_bounded() {
        let mut node = Node::Block(Block::new(text("leaf")));
        for _ in 0..=MAX_LAYOUT_DEPTH {
            node = Node::Layout(LayoutNode::Stack {
                children: vec![node],
                when: None,
            });
        }
        let config = config(vec![tree_panel("p", node)]);
        let error = validate(&config).unwrap_err();
        assert!(error.contains("nests deeper"), "{error}");
    }

    #[test]
    fn layout_node_count_is_bounded() {
        let children: Vec<Node> = (0..MAX_LAYOUT_NODES)
            .map(|_| Node::Block(Block::new(text("x"))))
            .collect();
        let config = config(vec![tree_panel(
            "p",
            Node::Layout(LayoutNode::Stack { children, when: None }),
        )]);
        let error = validate(&config).unwrap_err();
        assert!(error.contains("nodes and blocks"), "{error}");
    }

    #[test]
    fn a_panel_may_not_hold_more_blocks_than_the_limit() {
        let children: Vec<Node> = (0..MAX_BLOCKS + 1)
            .map(|_| Node::Block(Block::new(text("x"))))
            .collect();
        let config = config(vec![tree_panel(
            "p",
            Node::Layout(LayoutNode::Stack { children, when: None }),
        )]);
        let error = validate(&config).unwrap_err();
        assert!(error.contains("more than the"), "{error}");
    }

    // ---- bindings ----------------------------------------------------------

    #[test]
    fn a_when_condition_is_checked_at_approval_time() {
        let config = config(vec![tree_panel(
            "p",
            Node::Layout(LayoutNode::Stack {
                children: vec![Node::Block(Block { when: Some(Expr::checked(&serde_json::json!({
                        "@gt": [{ "@path": "/ci/queue/depth" }, 0]
                    }))
                    .unwrap()), body: text("only when busy") })],
                    when: None,
            }),
        )]);
        validate(&config).unwrap();
    }

    #[test]
    fn a_when_that_is_not_a_condition_is_refused() {
        let config = config(vec![tree_panel(
            "p",
            Node::Layout(LayoutNode::Stack {
                children: vec![Node::Block(Block { when: Some(Expr::checked(&serde_json::json!(7)).unwrap()), body: text("x") })],
                when: None,
            }),
        )]);
        let error = validate(&config).unwrap_err();
        assert!(error.contains("must be a condition"), "{error}");
    }

    // ---- node-level `when` -------------------------------------------------

    #[test]
    fn a_when_on_a_stack_node_round_trips() {
        let raw = r#"{"panels":[{"id":"p","title":"P","layout":{"node":"stack",
            "when":{"@exists":"/ci/queue"},
            "children":[{"kind":"text","text":"hi"}]}}]}"#;
        let parsed: PanelsConfig = serde_json::from_str(raw).unwrap();
        let upgraded = parsed.clamped();

        let expected = serde_json::json!({ "@exists": "/ci/queue" });
        let layout = upgraded.panels[0].layout.as_ref().expect("a layout");
        assert_eq!(layout.when().map(Expr::to_value), Some(expected.clone()));

        let again = serde_json::to_string(&upgraded).unwrap();
        let reparsed: PanelsConfig = serde_json::from_str(&again).unwrap();
        let layout = reparsed.panels[0].layout.as_ref().expect("a layout");
        assert_eq!(layout.when().map(Expr::to_value), Some(expected));
        validate(&reparsed).unwrap();
    }

    #[test]
    fn a_when_on_a_tabs_node_is_accepted() {
        let config = config(vec![tree_panel(
            "p",
            Node::Layout(LayoutNode::Tabs {
                tabs: vec![Tab {
                    label: "One".into(),
                    children: vec![Node::Block(Block::new(text("a")))],
                }],
                when: Some(
                    Expr::checked(&serde_json::json!({
                        "@gt": [{ "@path": "/ci/queue/depth" }, 0]
                    }))
                    .unwrap(),
                ),
            }),
        )]);
        validate(&config).unwrap();
    }

    /// A node's condition is checked by the same code as a block's, and the
    /// message has to say which node, or the agent cannot find it in a tree.
    #[test]
    fn a_node_when_that_is_not_a_condition_is_refused() {
        let config = config(vec![tree_panel(
            "p",
            Node::Layout(LayoutNode::Stack {
                children: vec![Node::Layout(LayoutNode::Stack {
                    children: vec![Node::Block(Block::new(text("x")))],
                    when: Some(Expr::checked(&serde_json::json!(7)).unwrap()),
                })],
                when: None,
            }),
        )]);
        let error = validate(&config).unwrap_err();
        assert!(error.contains("panel `p`, child 1"), "{error}");
        assert!(error.contains("must be a condition"), "{error}");
    }

    /// A node without a condition must serialise exactly as Phase 1 wrote it, so
    /// an untouched file does not churn on the next save.
    #[test]
    fn a_node_without_a_when_serialises_without_the_key() {
        let config = config(vec![tree_panel(
            "p",
            Node::Layout(LayoutNode::Stack {
                children: vec![
                    Node::Block(Block::new(text("a"))),
                    Node::Layout(LayoutNode::Split {
                        children: vec![
                            Node::Block(Block::new(text("b"))),
                            Node::Block(Block::new(text("c"))),
                        ],
                        weights: None,
                        when: None,
                    }),
                ],
                when: None,
            }),
        )]);
        let value = serde_json::to_value(&config).unwrap();
        let layout = &value["panels"][0]["layout"];
        assert_eq!(
            layout,
            &serde_json::json!({
                "node": "stack",
                "children": [
                    { "kind": "text", "text": "a" },
                    {
                        "node": "split",
                        "children": [
                            { "kind": "text", "text": "b" },
                            { "kind": "text", "text": "c" }
                        ]
                    }
                ]
            })
        );
        assert!(!layout.to_string().contains("when"), "{layout}");
    }

    /// `blocks` is the structural view, not the visible one: it says what the
    /// panel contains, so a hidden node's blocks are still in it.
    #[test]
    fn a_hidden_node_still_contributes_its_blocks() {
        let raw = r#"{"panels":[{"id":"p","title":"P","layout":{"node":"stack","children":[
            {"kind":"text","text":"always"},
            {"node":"stack","when":{"@exists":"/ci/queue"},"children":[
                {"kind":"text","text":"hidden"}
            ]}
        ]}}]}"#;
        let parsed: PanelsConfig = serde_json::from_str(raw).unwrap();
        let upgraded = parsed.clamped();

        let texts: Vec<String> = upgraded.panels[0]
            .blocks
            .iter()
            .map(|block| match &block.body {
                PanelBlock::Text { text } => text.clone(),
                other => panic!("expected text, got {other:?}"),
            })
            .collect();
        assert_eq!(texts, vec!["always", "hidden"]);
        validate(&upgraded).unwrap();
    }

    #[test]
    fn the_four_bound_blocks_round_trip() {
        let raw = r#"{"panels":[{"id":"p","title":"P","layout":{"node":"stack","children":[
            {"kind":"data","label":"Queue","value":{"@path":"/ci/queue/depth"},"hint":"jobs"},
            {"kind":"chart","label":"Throughput","style":"line","values":{"@path":"/ci/throughput"}},
            {"kind":"table","label":"Runs","columns":["id","state"],"rows":{"@path":"/ci/runs"}},
            {"kind":"progress","label":"Budget","value":{"@path":"/ci/spend"},"max":100}
        ]}}]}"#;
        let parsed: PanelsConfig = serde_json::from_str(raw).unwrap();
        let upgraded = parsed.clamped();
        validate(&upgraded).unwrap();

        let kinds: Vec<&str> = upgraded.panels[0]
            .blocks
            .iter()
            .map(|block| match &block.body {
                PanelBlock::Data { .. } => "data",
                PanelBlock::Chart { .. } => "chart",
                PanelBlock::Table { .. } => "table",
                PanelBlock::Progress { .. } => "progress",
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(kinds, vec!["data", "chart", "table", "progress"]);

        let again = serde_json::to_string(&upgraded).unwrap();
        let reparsed: PanelsConfig = serde_json::from_str(&again).unwrap();
        validate(&reparsed).unwrap();
    }

    #[test]
    fn a_progress_value_must_be_a_number() {
        let config = config(vec![tree_panel(
            "p",
            Node::Layout(LayoutNode::Stack {
                children: vec![Node::Block(Block::new(PanelBlock::Progress {
                    label: "Budget".into(),
                    value: Expr::checked(&serde_json::json!("lots")).unwrap(),
                    max: None,
                }))],
                when: None,
            }),
        )]);
        let error = validate(&config).unwrap_err();
        assert!(error.contains("must be a number"), "{error}");
    }

    #[test]
    fn a_table_needs_columns_and_they_must_be_unique() {
        let build = |columns: Vec<&str>| {
            config(vec![tree_panel(
                "p",
                Node::Layout(LayoutNode::Stack {
                    children: vec![Node::Block(Block::new(PanelBlock::Table {
                        label: None,
                        columns: columns.into_iter().map(String::from).collect(),
                        rows: Expr::checked(&serde_json::json!({ "@path": "/ci/runs" })).unwrap(),
                    }))],
                    when: None,
                }),
            )])
        };

        assert!(validate(&build(vec![])).unwrap_err().contains("no columns"));
        assert!(validate(&build(vec!["a", "a"]))
            .unwrap_err()
            .contains("used twice"));
        validate(&build(vec!["a", "b"])).unwrap();
    }

    #[test]
    fn paths_are_collected_from_the_whole_tree() {
        let layout = Node::Layout(LayoutNode::Tabs {
            tabs: vec![
                Tab {
                    label: "One".into(),
                    children: vec![Node::Block(Block::new(PanelBlock::Data {
                        label: "Queue".into(),
                        value: Expr::checked(&serde_json::json!({ "@path": "/ci/queue/depth" }))
                            .unwrap(),
                        hint: None,
                    }))],
                },
                Tab {
                    label: "Two".into(),
                    children: vec![Node::Block(Block { when: Some(Expr::checked(&serde_json::json!({ "@exists": "/ci/queue" })).unwrap()), body: text("x") })],
                },
            ],
            when: None,
        });
        assert_eq!(layout.paths(), vec!["/ci/queue/depth", "/ci/queue"]);
    }

    #[test]
    fn every_block_kind_is_listed_in_the_manifest() {
        assert_eq!(PanelBlock::KINDS.len(), 12);
        let mut kinds = PanelBlock::KINDS.to_vec();
        kinds.sort_unstable();
        kinds.dedup();
        assert_eq!(kinds.len(), PanelBlock::KINDS.len());
        for kind in ["data", "chart", "diagram", "table", "progress", "widget"] {
            assert!(PanelBlock::KINDS.contains(&kind), "{kind} is missing");
        }
    }

    #[test]
    fn every_chart_style_is_listed() {
        assert_eq!(ChartStyle::ALL.len(), 4);
        for style in ChartStyle::ALL {
            let parsed: ChartStyle = serde_json::from_value(serde_json::json!(style)).unwrap();
            assert_eq!(
                serde_json::to_value(parsed).unwrap(),
                serde_json::json!(style)
            );
        }
    }

    #[test]
    fn a_chart_needs_something_to_draw() {
        // An empty chart is indistinguishable from a broken binding, so it is
        // refused at validation rather than rendered as a blank box.
        let raw = r#"{"kind":"chart","label":"Throughput"}"#;
        let block: PanelBlock = serde_json::from_str(raw).unwrap();
        assert!(check_block(&block).is_err());
    }

    #[test]
    fn a_chart_accepts_the_single_series_shorthand() {
        let raw = r#"{"kind":"chart","label":"Throughput","values":{"@path":"/ci/throughput"}}"#;
        let block: PanelBlock = serde_json::from_str(raw).unwrap();
        assert!(check_block(&block).is_ok());
        assert_eq!(block.paths(), vec!["/ci/throughput".to_string()]);
    }

    #[test]
    fn a_chart_reads_every_series_and_both_axes() {
        let raw = r#"{
            "kind":"chart","label":"Load","style":"area","unit":"%","time":true,
            "series":[
                {"name":"cpu","values":{"@path":"/host/cpu"}},
                {"name":"mem","values":{"@path":"/host/mem"}}
            ],
            "x":{"@path":"/host/at"},
            "yMin":0,
            "yMax":{"@path":"/host/ceiling"}
        }"#;
        let block: PanelBlock = serde_json::from_str(raw).unwrap();
        assert!(check_block(&block).is_ok());
        assert_eq!(
            block.paths(),
            vec![
                "/host/cpu".to_string(),
                "/host/mem".to_string(),
                "/host/at".to_string(),
                "/host/ceiling".to_string(),
            ]
        );
    }

    #[test]
    fn a_chart_may_not_draw_more_series_than_the_palette_holds() {
        let series: Vec<String> = (0..=MAX_CHART_SERIES)
            .map(|i| format!(r#"{{"name":"s{i}","values":{{"@path":"/s/{i}"}}}}"#))
            .collect();
        let raw = format!(
            r#"{{"kind":"chart","label":"Too many","series":[{}]}}"#,
            series.join(",")
        );
        let block: PanelBlock = serde_json::from_str(&raw).unwrap();
        assert!(check_block(&block).is_err());
    }

    #[test]
    fn every_chart_stack_and_scale_is_listed() {
        assert_eq!(ChartStack::ALL.len(), 3);
        assert_eq!(ChartScale::ALL.len(), 2);
        for name in ChartStack::ALL {
            let parsed: ChartStack = serde_json::from_value(serde_json::json!(name)).unwrap();
            assert_eq!(serde_json::to_value(parsed).unwrap(), serde_json::json!(name));
        }
        for name in ChartScale::ALL {
            let parsed: ChartScale = serde_json::from_value(serde_json::json!(name)).unwrap();
            assert_eq!(serde_json::to_value(parsed).unwrap(), serde_json::json!(name));
        }
    }

    #[test]
    fn a_chart_stacks_only_what_has_thickness() {
        // A line has no thickness to accumulate, so `stack` on a line is a
        // field that would be silently ignored. Refused instead.
        let line = r#"{"kind":"chart","label":"L","style":"line","stack":"stacked","values":[1,2]}"#;
        let block: PanelBlock = serde_json::from_str(line).unwrap();
        assert!(check_block(&block).is_err());

        let spark = r#"{"kind":"chart","label":"S","style":"sparkline","stack":"percent","values":[1,2]}"#;
        let block: PanelBlock = serde_json::from_str(spark).unwrap();
        assert!(check_block(&block).is_err());

        for style in ["bar", "area"] {
            let raw = format!(
                r#"{{"kind":"chart","label":"Ok","style":"{style}","stack":"stacked","values":[1,2]}}"#
            );
            let block: PanelBlock = serde_json::from_str(&raw).unwrap();
            assert!(check_block(&block).is_ok(), "{style} should stack");
        }
    }

    #[test]
    fn a_log_chart_refuses_a_zero_floor() {
        // A log axis cannot start at zero, and `yMin: 0` is the most likely
        // thing to write. Saying so beats silently dropping the bound.
        let raw = r#"{"kind":"chart","label":"L","scale":"log","yMin":0,"values":[1,10,100]}"#;
        let block: PanelBlock = serde_json::from_str(raw).unwrap();
        assert!(check_block(&block).is_err());

        let ok = r#"{"kind":"chart","label":"L","scale":"log","values":[1,10,100]}"#;
        let block: PanelBlock = serde_json::from_str(ok).unwrap();
        assert!(check_block(&block).is_ok());
    }

    #[test]
    fn a_chart_reads_its_reference_lines() {
        let raw = r#"{
            "kind":"chart","label":"Latency","values":{"@path":"/api/p95"},
            "reference":[
                {"value":{"@path":"/api/sla"},"label":"SLA","tone":"warn"},
                {"value":250,"label":"budget"}
            ]
        }"#;
        let block: PanelBlock = serde_json::from_str(raw).unwrap();
        assert!(check_block(&block).is_ok());
        assert_eq!(
            block.paths(),
            vec![
                "/api/p95".to_string(),
                "/api/sla".to_string(),
            ]
        );
    }

    #[test]
    fn a_chart_may_not_draw_more_references_than_it_can_label() {
        let marks: Vec<String> = (0..=MAX_CHART_REFERENCES)
            .map(|i| format!(r#"{{"value":{i},"label":"r{i}"}}"#))
            .collect();
        let raw = format!(
            r#"{{"kind":"chart","label":"Many","values":[1,2],"reference":[{}]}}"#,
            marks.join(",")
        );
        let block: PanelBlock = serde_json::from_str(&raw).unwrap();
        assert!(check_block(&block).is_err());
    }

    #[test]
    fn a_chart_height_is_clamped_not_refused() {
        // Same rule as a sandbox and a widget: a height outside the range is a
        // number to correct, not a reason to reject the whole file.
        let raw = r#"{"kind":"chart","label":"Tall","values":[1,2,3],"height":9000}"#;
        let block: PanelBlock = serde_json::from_str(raw).unwrap();
        assert!(check_block(&block).is_ok());
        let config = config(vec![panel("p", vec![block])]).clamped();
        match &config.panels[0].blocks[0].body {
            PanelBlock::Chart { height, .. } => assert_eq!(*height, Some(CHART_HEIGHT.1)),
            other => panic!("expected a chart, got {other:?}"),
        }
    }

    /// The migration path end to end: a v1 file on disk, loaded, saved, and
    /// loaded again. The blocks must survive, the file must now be v2, and the
    /// derived view must not have been written alongside the tree.
    #[test]
    fn a_v1_file_is_migrated_to_v2_on_save_and_still_loads() {
        let dir = std::env::temp_dir().join(format!("vibedesk-migrate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("panels.json");

        std::fs::write(
            &path,
            r#"{"panels":[{"id":"p","title":"P","blocks":[
                {"kind":"text","text":"one"},
                {"kind":"list","items":["a","b"]}
            ]}]}"#,
        )
        .unwrap();

        let loaded = load_panels_from(&path).unwrap();
        assert_eq!(loaded.panels[0].blocks.len(), 2);
        assert!(loaded.panels[0].layout.is_some(), "v1 was not upgraded");

        save_panels_to(&path, &loaded).unwrap();

        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(on_disk.contains("\"layout\""), "{on_disk}");
        assert!(
            !on_disk.contains("\"blocks\""),
            "the derived view was written next to the tree: {on_disk}"
        );

        let reloaded = load_panels_from(&path).unwrap();
        assert_eq!(reloaded.panels[0].blocks.len(), 2);
        match &reloaded.panels[0].blocks[1].body {
            PanelBlock::List { items, .. } => assert_eq!(items, &vec!["a".to_string(), "b".to_string()]),
            other => panic!("expected a list, got {other:?}"),
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A panel that binds nothing must not claim to bind anything, and a panel
    /// that binds three paths must report all three exactly once.
    #[test]
    fn bindings_are_reported_once_each() {
        let layout = Node::Layout(LayoutNode::Stack {
            children: vec![
                Node::Block(Block::new(PanelBlock::Data {
                    label: "Queue".into(),
                    value: Expr::checked(&serde_json::json!({ "@path": "/ci/queue/depth" })).unwrap(),
                    hint: None,
                })),
                Node::Block(Block::new(PanelBlock::Progress {
                    label: "Budget".into(),
                    value: Expr::checked(&serde_json::json!({ "@path": "/ci/spend" })).unwrap(),
                    max: Some(Expr::checked(&serde_json::json!({ "@path": "/ci/limit" })).unwrap()),
                })),
                Node::Block(Block::new(PanelBlock::Data {
                    label: "Again".into(),
                    value: Expr::checked(&serde_json::json!({ "@path": "/ci/queue/depth" })).unwrap(),
                    hint: None,
                })),
                Node::Block(Block::new(text("no binding here"))),
            ],
            when: None,
        });
        assert_eq!(
            layout.paths(),
            vec!["/ci/queue/depth", "/ci/spend", "/ci/limit"]
        );
    }

    /// The frontend types `Block` as `PanelBlock & { when? }`, which is only
    /// correct if the wire format is flat. It is: `#[serde(flatten)]` puts the
    /// body's fields beside `when`, not under a `body` key. This test exists
    /// because getting it wrong blanked the whole window once.
    #[test]
    fn a_block_serialises_flat() {
        let plain = Block::new(text("hi"));
        assert_eq!(
            serde_json::to_value(&plain).unwrap(),
            serde_json::json!({ "kind": "text", "text": "hi" })
        );

        let guarded = Block {
            when: Some(Expr::checked(&serde_json::json!({ "@exists": "/a" })).unwrap()),
            body: text("hi"),
        };
        assert_eq!(
            serde_json::to_value(&guarded).unwrap(),
            serde_json::json!({ "kind": "text", "text": "hi", "when": { "@exists": "/a" } })
        );
    }

    /// A panel's `blocks` must be the same shape as a v1 block, or every
    /// existing renderer breaks on the upgrade.
    #[test]
    fn a_panel_serialises_blocks_flat() {
        let config = config(vec![panel("p", vec![text("hi")])]).clamped();
        let value = serde_json::to_value(&config).unwrap();
        let block = &value["panels"][0]["blocks"][0];
        assert_eq!(block["kind"], "text");
        assert_eq!(block["text"], "hi");
        assert!(block.get("body").is_none(), "{block}");
    }

    // ---- sources -----------------------------------------------------------

    fn with_sources(mut panel: CustomPanel, sources: Vec<crate::data::Source>) -> CustomPanel {
        panel.sources = sources;
        panel
    }

    fn http_source(id: &str, url: &str) -> crate::data::Source {
        crate::data::Source {
            id: id.into(),
            kind: crate::data::SourceKind::Http {
                url: url.into(),
                mode: crate::data::HttpMode::Poll,
                interval_ms: 2_000,
                timeout_ms: 5_000,
            },
        }
    }

    #[test]
    fn a_panel_may_declare_a_loopback_source() {
        let config = config(vec![with_sources(
            panel("p", vec![text("hi")]),
            vec![http_source("ci", "http://127.0.0.1:8787/status")],
        )]);
        validate(&config).unwrap();
    }

    #[test]
    fn a_panel_may_not_declare_a_non_loopback_source() {
        let config = config(vec![with_sources(
            panel("p", vec![text("hi")]),
            vec![http_source("ci", "http://example.com/status")],
        )]);
        let error = validate(&config).unwrap_err();
        assert!(error.contains("not loopback"), "{error}");
    }

    #[test]
    fn a_panel_may_not_declare_more_sources_than_the_limit() {
        let sources: Vec<crate::data::Source> = (0..=crate::data::MAX_SOURCES_PER_PANEL)
            .map(|i| http_source(&format!("s{i}"), "http://127.0.0.1:8787/status"))
            .collect();
        let config = config(vec![with_sources(panel("p", vec![text("hi")]), sources)]);
        let error = validate(&config).unwrap_err();
        assert!(error.contains("more than the"), "{error}");
    }

    #[test]
    fn a_panel_may_not_declare_the_same_source_twice() {
        let config = config(vec![with_sources(
            panel("p", vec![text("hi")]),
            vec![
                http_source("ci", "http://127.0.0.1:8787/status"),
                http_source("ci", "http://127.0.0.1:8787/other"),
            ],
        )]);
        let error = validate(&config).unwrap_err();
        assert!(error.contains("used twice"), "{error}");
    }

    #[test]
    fn a_panel_without_sources_serialises_without_the_key() {
        let config = config(vec![panel("p", vec![text("hi")])]).clamped();
        let value = serde_json::to_value(&config).unwrap();
        assert!(value["panels"][0].get("sources").is_none(), "{value}");
    }

    #[test]
    fn a_panel_with_sources_round_trips_them() {
        let config = config(vec![with_sources(
            panel("p", vec![text("hi")]),
            vec![http_source("ci", "http://127.0.0.1:8787/status")],
        )])
        .clamped();
        let raw = serde_json::to_string(&config).unwrap();
        let back: PanelsConfig = serde_json::from_str(&raw).unwrap();
        validate(&back).unwrap();
        assert_eq!(back.panels[0].sources.len(), 1);
        assert_eq!(back.panels[0].sources[0].id, "ci");
    }

    // ---- plugin panels -----------------------------------------------------

    /// `plugin` is how the frontend tells a plugin panel from the user's own.
    /// A user file that sets it would be claiming a panel it does not own, so
    /// it is refused rather than ignored.
    #[test]
    fn a_user_panel_may_not_set_plugin() {
        let raw = r#"{"panels":[{"id":"p","title":"P","plugin":"ci-tools","blocks":[{"kind":"text","text":"hi"}]}]}"#;
        let parsed: PanelsConfig = serde_json::from_str(raw).unwrap();
        let error = validate(&parsed).unwrap_err();
        assert!(error.contains("reserved for plugin-contributed"), "{error}");
    }

    /// The other half of the same rule: a user panel may not take an id in the
    /// plugin namespace, so it can neither impersonate nor shadow one.
    #[test]
    fn a_user_panel_id_may_not_start_with_plugin() {
        let raw = r#"{"panels":[{"id":"plugin:ci-tools/ci","title":"P","blocks":[{"kind":"text","text":"hi"}]}]}"#;
        let parsed: PanelsConfig = serde_json::from_str(raw).unwrap();
        let error = validate(&parsed).unwrap_err();
        assert!(error.contains("reserved for plugin-contributed"), "{error}");
    }

    /// A user panel never gains the key on disk, so an untouched file does not
    /// churn and a hand edit cannot smuggle one in.
    #[test]
    fn a_user_panel_serialises_without_the_plugin_key() {
        let config = config(vec![panel("p", vec![text("hi")])]).clamped();
        let value = serde_json::to_value(&config).unwrap();
        assert!(value["panels"][0].get("plugin").is_none(), "{value}");
    }
    /* ---------------------------------------------------------------- *
     * diagram
     * ---------------------------------------------------------------- */

    fn diagram(raw: &str) -> Result<PanelsConfig, String> {
        let parsed: PanelsConfig = serde_json::from_str(raw).map_err(|e| e.to_string())?;
        let clamped = parsed.clamped();
        validate(&clamped)?;
        Ok(clamped)
    }

    #[test]
    fn a_diagram_round_trips() {
        let raw = r#"{"panels":[{"id":"p","title":"P","blocks":[
            {"kind":"diagram","label":"Request path","layout":"layered",
             "nodes":[
                {"id":"client","label":"Client","detail":"browser","tone":"accent"},
                {"id":"edge","label":"Edge","detail":{"@path":"/cdn/region"}},
                {"id":"api","label":"API","tone":{"@if":[{"@gt":[{"@path":"/api/errors"},0]},"danger","ok"]},"action":"restart-api"}
             ],
             "edges":[
                {"id":"e1","from":"client","to":"edge","label":"GET /"},
                {"id":"e2","from":"edge","to":"api","label":{"@format":"duration","@of":{"@path":"/cdn/latency"}}}
             ],
             "height":260}
        ]}]}"#;
        let config = diagram(raw).unwrap();
        let block = &config.panels[0].blocks[0].body;
        match block {
            PanelBlock::Diagram {
                layout,
                nodes,
                edges,
                height,
                ..
            } => {
                assert_eq!(*layout, DiagramLayout::Layered);
                assert_eq!(nodes.len(), 3);
                assert_eq!(edges.len(), 2);
                assert_eq!(*height, Some(260));
                assert_eq!(nodes[2].action.as_deref(), Some("restart-api"));
            }
            other => panic!("expected a diagram, got {other:?}"),
        }

        // Every binding on the diagram is a promise the panel is making.
        let paths = config.panels[0].tree().paths();
        for expected in ["/cdn/region", "/api/errors", "/cdn/latency"] {
            assert!(paths.contains(&expected.to_string()), "{paths:?}");
        }

        // And it survives a write/read cycle unchanged.
        let again = serde_json::to_string(&config).unwrap();
        let reparsed: PanelsConfig = serde_json::from_str(&again).unwrap();
        validate(&reparsed).unwrap();
    }

    #[test]
    fn a_diagram_needs_nodes() {
        let error = diagram(
            r#"{"panels":[{"id":"p","title":"P","blocks":[
                {"kind":"diagram","label":"Empty","nodes":[]}
            ]}]}"#,
        )
        .unwrap_err();
        assert!(error.contains("no nodes"), "{error}");
    }

    #[test]
    fn a_diagram_refuses_a_duplicate_node_id() {
        let error = diagram(
            r#"{"panels":[{"id":"p","title":"P","blocks":[
                {"kind":"diagram","label":"Twice","nodes":[
                    {"id":"a","label":"A"},{"id":"a","label":"B"}
                ]}
            ]}]}"#,
        )
        .unwrap_err();
        assert!(error.contains("used twice"), "{error}");
    }

    #[test]
    fn a_diagram_refuses_an_edge_to_a_node_that_is_not_there() {
        let error = diagram(
            r#"{"panels":[{"id":"p","title":"P","blocks":[
                {"kind":"diagram","label":"Dangling","nodes":[{"id":"a","label":"A"}],
                 "edges":[{"from":"a","to":"ghost"}]}
            ]}]}"#,
        )
        .unwrap_err();
        assert!(error.contains("`ghost`"), "{error}");
        assert!(error.contains("not a node"), "{error}");
    }

    #[test]
    fn a_diagram_refuses_an_id_that_is_not_a_slug() {
        let error = diagram(
            r#"{"panels":[{"id":"p","title":"P","blocks":[
                {"kind":"diagram","label":"Slug","nodes":[{"id":"has space","label":"A"}]}
            ]}]}"#,
        )
        .unwrap_err();
        assert!(error.contains("letters, digits"), "{error}");
    }

    #[test]
    fn a_diagram_refuses_an_action_id_that_is_not_one() {
        let error = diagram(
            r#"{"panels":[{"id":"p","title":"P","blocks":[
                {"kind":"diagram","label":"Act","nodes":[
                    {"id":"a","label":"A","action":"Not An Id"}
                ]}
            ]}]}"#,
        )
        .unwrap_err();
        assert!(error.contains("not a valid action id"), "{error}");
    }

    #[test]
    fn a_diagram_clamps_its_height_and_caps_its_nodes() {
        let tall = diagram(
            r#"{"panels":[{"id":"p","title":"P","blocks":[
                {"kind":"diagram","label":"Tall","height":9000,"nodes":[{"id":"a","label":"A"}]}
            ]}]}"#,
        )
        .unwrap();
        match &tall.panels[0].blocks[0].body {
            PanelBlock::Diagram { height, .. } => {
                assert_eq!(*height, Some(DIAGRAM_HEIGHT.1));
            }
            other => panic!("expected a diagram, got {other:?}"),
        }

        let nodes: Vec<String> = (0..=MAX_DIAGRAM_NODES)
            .map(|index| format!(r#"{{"id":"n{index}","label":"N{index}"}}"#))
            .collect();
        let error = diagram(&format!(
            r#"{{"panels":[{{"id":"p","title":"P","blocks":[
                {{"kind":"diagram","label":"Wide","nodes":[{}]}}
            ]}}]}}"#,
            nodes.join(",")
        ))
        .unwrap_err();
        assert!(error.contains("the limit is"), "{error}");
    }

    #[test]
    fn every_diagram_layout_and_routing_parses() {
        for layout in DiagramLayout::ALL {
            let raw = format!(
                r#"{{"panels":[{{"id":"p","title":"P","blocks":[
                    {{"kind":"diagram","label":"L","layout":"{layout}","nodes":[{{"id":"a","label":"A"}}]}}
                ]}}]}}"#
            );
            diagram(&raw).unwrap_or_else(|error| panic!("{layout}: {error}"));
        }
        for routing in DiagramRouting::ALL {
            let raw = format!(
                r#"{{"panels":[{{"id":"p","title":"P","blocks":[
                    {{"kind":"diagram","label":"R","routing":"{routing}","nodes":[{{"id":"a","label":"A"}}]}}
                ]}}]}}"#
            );
            diagram(&raw).unwrap_or_else(|error| panic!("{routing}: {error}"));
        }
        // A layout name outside the set is a serde error, not a silent default.
        let bad = r#"{"panels":[{"id":"p","title":"P","blocks":[
            {"kind":"diagram","label":"X","layout":"spiral","nodes":[{"id":"a","label":"A"}]}
        ]}]}"#;
        assert!(diagram(bad).is_err());
    }
}
