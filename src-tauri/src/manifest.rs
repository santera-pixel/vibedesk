//! The manifest: what the agent may change, and what it currently is.
//!
//! The preamble used to recite the schemas in prose. That cost a paragraph of
//! context on every session and still left the agent guessing — it could not see
//! the current token values, the valid icon names, or the clamps, so a wrong
//! guess was silently dropped by serde and the agent never found out.
//!
//! This module writes one JSON document that answers all of it, and the preamble
//! now says "read this first" instead. The important property is that **every
//! list here is derived from the enum that validates it**: `ActionIcon::ALL`,
//! `BuiltinCommand::ALL`, `PanelBlock::KINDS`, the layout clamp tuples, the
//! protected-selector list. Adding an icon to the enum makes it appear in the
//! manifest with no second edit, so the document cannot drift from what the
//! validator accepts. The tests below fail if a variant is added without being
//! listed in its `ALL`.
//!
//! The manifest is read-only for the agent. It is regenerated on boot and on
//! every config change, from the same loaders the UI uses, so a proposal is
//! always written against the state the user is actually looking at.

use std::path::PathBuf;

use serde::Serialize;

use crate::actions::{
    self, ActionIcon, ActionRun, ActionSlot, ActionsConfig, BuiltinCommand, ElicitationAnswer,
    PermissionScope,
};
use crate::customize::{self, ThemeConfig};
use crate::layout::{self, Density, LayoutConfig, Motion, Side};
use crate::panels::{self, PanelBlock, PanelsConfig};
use crate::sources::{self, ConfigKind};

/// The file the agent reads. Lives next to the configs it describes.
pub const MANIFEST_FILE: &str = "vibedesk.manifest.json";

/// A manifest larger than this is a bug, not a document. It is written by us,
/// so the cap is a tripwire rather than a defence.
pub const MAX_MANIFEST_BYTES: usize = 256 * 1024;

pub fn manifest_path() -> PathBuf {
    customize::config_dir().join(MANIFEST_FILE)
}

/// The stylesheet is the source of truth for the token vocabulary, so it is
/// embedded rather than restated. A token added to `globals.css` appears in the
/// manifest with no second edit, and one removed disappears — the list cannot
/// drift from what the app actually defines.
const STYLESHEET: &str = include_str!("../../src/styles/globals.css");

/// Tokens owned by `layout.json`. A theme may set them, but `applyLayout` writes
/// the same names inline on `<html>` and outranks the stylesheet, so a theme
/// value would silently lose. Better to leave them out than to publish a knob
/// that does not turn.
const LAYOUT_OWNED: &[&str] = &[
    "--vd-rail-w",
    "--vd-sidebar-w",
    "--vd-transcript-max",
    "--vd-composer-max",
    "--vd-density",
];

/// Every `--vd-` token the stylesheet declares, with the first value it is given
/// (the ink theme, since `:root` comes first). Sorted, so the manifest is stable
/// between runs.
fn token_vocabulary() -> Vec<TokenInfo> {
    let mut seen: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();

    for line in STYLESHEET.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("--vd-") else {
            continue;
        };
        let Some((name, value)) = rest.split_once(':') else {
            continue;
        };
        let name = format!("--vd-{}", name.trim());
        if LAYOUT_OWNED.contains(&name.as_str()) {
            continue;
        }
        // Drop the trailing `;` and any `/* comment */` after it.
        let value = value.split("/*").next().unwrap_or(value).trim();
        let value = value.trim_end_matches(';').trim();
        if value.is_empty() {
            continue;
        }
        seen.entry(name).or_insert_with(|| value.to_string());
    }

    seen.into_iter()
        .map(|(name, value)| TokenInfo { name, value })
        .collect()
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub version: u32,
    pub generated_at: String,
    /// Where to write a proposal, and which names are accepted there.
    pub pending: PendingInfo,
    pub surfaces: Surfaces,
    pub protected: Protected,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingInfo {
    pub dir: String,
    /// The only file names the pending directory accepts.
    pub files: Vec<&'static str>,
    /// Absolute path of the receipt for each kind, written after a decision.
    pub receipts: Vec<String>,
    /// What the agent must do to propose: write one file, then stop.
    pub how: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Surfaces {
    pub theme: ThemeSurface,
    pub layout: LayoutSurface,
    pub actions: ActionsSurface,
    pub panels: PanelsSurface,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemeSurface {
    pub path: String,
    pub current: ThemeConfig,
    /// Every token the app defines, with its built-in value. This is the
    /// vocabulary a proposal may set — not the user's overrides, which are in
    /// `current`. An empty list here would tell the agent that nothing is
    /// customizable, which is exactly the wrong answer on a fresh install.
    pub tokens: Vec<TokenInfo>,
    pub limits: ThemeLimits,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenInfo {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemeLimits {
    pub max_bytes: u64,
    pub max_rules: usize,
    pub max_value_len: usize,
    /// Substrings that may never appear in a declaration value.
    pub forbidden_values: Vec<&'static str>,
    /// Properties that may never appear in a rule.
    pub forbidden_properties: Vec<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LayoutSurface {
    pub path: String,
    pub current: LayoutConfig,
    pub fields: Vec<FieldInfo>,
    pub sides: Vec<Side>,
    pub densities: Vec<Density>,
    pub motions: Vec<Motion>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldInfo {
    pub name: &'static str,
    pub kind: &'static str,
    pub min: u16,
    pub max: u16,
    pub default: u16,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionsSurface {
    pub path: String,
    pub current: ActionsConfig,
    pub icons: Vec<ActionIcon>,
    pub commands: Vec<BuiltinCommand>,
    pub slots: Vec<ActionSlot>,
    pub run_kinds: Vec<&'static str>,
    /// The two scopes `approve` and `reject` accept.
    pub permission_scopes: Vec<&'static str>,
    /// The three answers `answer` accepts.
    pub elicitation_answers: Vec<&'static str>,
    pub limits: ActionsLimits,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionsLimits {
    pub max_bytes: u64,
    pub max_actions: usize,
    pub max_id: usize,
    pub max_label: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PanelsSurface {
    pub path: String,
    pub current: PanelsConfig,
    pub block_kinds: Vec<&'static str>,
    /// Container kinds a `layout` tree may use, tagged by `node`.
    pub layout_nodes: Vec<&'static str>,
    /// Fields a container may carry beside its `node` tag. `when` is one of
    /// them, so a container can be guarded exactly as a block can.
    pub layout_node_fields: Vec<&'static str>,
    /// The rule that makes a guarded container worth having, stated where the
    /// agent will read it.
    pub layout_rule: &'static str,
    pub chart_styles: Vec<&'static str>,
    pub chart_stacks: Vec<&'static str>,
    pub chart_scales: Vec<&'static str>,
    /// The four layouts a `diagram` may use, and what each one is for.
    pub diagram_layouts: Vec<&'static str>,
    /// The semantic tones a node may resolve to. A closed set, so a
    /// diagram follows the theme instead of hard-coding a colour.
    pub diagram_tones: Vec<&'static str>,
    /// How a diagram may draw its edges.
    pub diagram_routings: Vec<&'static str>,
    /// Every path the current panels bind, per panel. A path here is a promise
    /// the panel is making; nothing serves it until a source declares it, so
    /// this is the list to check before wondering why a panel is empty.
    pub bindings: Vec<PanelBindings>,
    /// The closed set of source kinds, and what each may address.
    pub source_kinds: Vec<&'static str>,
    pub builtin_sources: Vec<&'static str>,
    pub http_modes: Vec<&'static str>,
    /// The rule that matters, stated where the agent will read it.
    pub source_rule: &'static str,
    pub expressions: ExpressionsSurface,
    pub limits: PanelsLimits,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PanelBindings {
    pub id: String,
    pub paths: Vec<String>,
}

/// The binding language, described so the agent can author a `when` or a `data`
/// value without guessing. Every operator listed here is one `expr.rs` accepts;
/// the test below fails if the two lists drift.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExpressionsSurface {
    pub operators: Vec<&'static str>,
    pub formats: Vec<&'static str>,
    pub max_depth: usize,
    pub max_nodes: usize,
    /// A path is `/`-separated, starts with `/`, has no `.` or `..`, no empty
    /// segment, and no array index syntax.
    pub path_grammar: &'static str,
    /// A complete panel that uses every new feature, so the shape is unambiguous.
    pub example: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PanelsLimits {
    pub max_bytes: u64,
    pub max_panels: usize,
    pub max_blocks: usize,
    pub max_layout_nodes: usize,
    pub max_layout_depth: usize,
    pub max_tabs: usize,
    pub max_tab_label: usize,
    pub max_columns: usize,
    pub max_sources_per_panel: usize,
    pub interval_min: u64,
    pub interval_max: u64,
    pub timeout_min: u64,
    pub timeout_max: u64,
    pub max_body_bytes: usize,
    pub max_json_depth: usize,
    pub max_json_nodes: usize,
    pub max_concurrent: usize,
    pub width_min: u16,
    pub width_max: u16,
    pub sandbox_height_min: u16,
    pub sandbox_height_max: u16,
    pub sandbox_max_html: usize,
    pub chart_height_min: u16,
    pub chart_height_max: u16,
    pub chart_max_series: usize,
    pub max_chart_references: usize,
    /// What a chart draws that a sparkline does not, so the agent picks the
    /// style on purpose instead of defaulting to a strip.
    pub chart_rule: &'static str,
    pub diagram_height_min: u16,
    pub diagram_height_max: u16,
    pub diagram_max_nodes: usize,
    pub diagram_max_edges: usize,
    /// What a diagram is, and the one thing it will not do: free-form
    /// positioning. That is a widget's job.
    pub diagram_rule: &'static str,
    /// Substrings a sandbox block may not contain. Listed so the agent can see
    /// the rule instead of discovering it through a refused proposal.
    pub sandbox_forbidden: Vec<&'static str>,
    pub widget_height_min: u16,
    pub widget_height_max: u16,
    pub widget_max_html: usize,
    pub widget_max_capabilities: usize,
    /// Substrings a widget block may not contain: the loaders that would fail
    /// silently under the widget's own policy.
    pub widget_forbidden: Vec<&'static str>,
    /// The capability kinds a widget block may declare.
    pub widget_capabilities: Vec<&'static str>,
    /// The origin a widget document is served from, so the agent can reason
    /// about what the frame can and cannot reach.
    pub widget_origin: &'static str,
    /// What a widget is, in one paragraph, including what it may not do.
    pub widget_rule: &'static str,
    /// Where a widget's html may live, and why a file is the better shape.
    pub widget_src_rule: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Protected {
    /// Selectors no rule may target, from any source.
    pub selectors: Vec<&'static str>,
    /// Why they are protected, in one line, so the agent does not try to work
    /// around the rule.
    pub why: &'static str,
}

/// Build the document from the live config. Each surface falls back to its
/// default when the file on disk is invalid, which mirrors what the UI shows:
/// a broken theme does not stop a valid layout.
pub fn build() -> Manifest {
    let theme = customize::load_theme().unwrap_or_default();
    let layout = layout::load_layout().unwrap_or_default();
    let actions = actions::load_actions().unwrap_or_default();
    let panels = panels::load_panels().unwrap_or_default();

    Manifest {
        version: 1,
        generated_at: chrono::Utc::now().to_rfc3339(),
        pending: PendingInfo {
            dir: sources::pending_dir().display().to_string(),
            files: ConfigKind::ALL.iter().map(|k| k.file_name()).collect(),
            receipts: ConfigKind::ALL
                .iter()
                .map(|k| sources::receipt_path(*k).display().to_string())
                .collect(),
            how: "Write exactly one of the files above into the pending directory, then stop and tell the user what you proposed. Do not write the live config. VibeDesk validates the file, shows it to the user, and applies it only if they approve. After they decide, read the matching receipt file to learn the outcome.",
        },
        surfaces: Surfaces {
            theme: ThemeSurface {
                path: customize::theme_path().display().to_string(),
                tokens: token_vocabulary(),
                current: theme,
                limits: ThemeLimits {
                    max_bytes: customize::MAX_THEME_BYTES,
                    max_rules: customize::MAX_RULES,
                    max_value_len: customize::MAX_VALUE_LEN,
                    forbidden_values: customize::FORBIDDEN_VALUE.to_vec(),
                    forbidden_properties: customize::FORBIDDEN_PROPERTY.to_vec(),
                },
            },
            layout: LayoutSurface {
                path: layout::layout_path().display().to_string(),
                fields: vec![
                    FieldInfo {
                        name: "railWidth",
                        kind: "int",
                        min: layout::RAIL_WIDTH.0,
                        max: layout::RAIL_WIDTH.1,
                        default: LayoutConfig::default().rail_width,
                    },
                    FieldInfo {
                        name: "sidebarWidth",
                        kind: "int",
                        min: layout::SIDEBAR_WIDTH.0,
                        max: layout::SIDEBAR_WIDTH.1,
                        default: LayoutConfig::default().sidebar_width,
                    },
                    FieldInfo {
                        name: "transcriptMax",
                        kind: "int",
                        min: layout::TRANSCRIPT_MAX.0,
                        max: layout::TRANSCRIPT_MAX.1,
                        default: LayoutConfig::default().transcript_max,
                    },
                    FieldInfo {
                        name: "composerMax",
                        kind: "int",
                        min: layout::COMPOSER_MAX.0,
                        max: layout::COMPOSER_MAX.1,
                        default: LayoutConfig::default().composer_max,
                    },
                ],
                sides: Side::ALL.to_vec(),
                densities: Density::ALL.to_vec(),
                motions: Motion::ALL.to_vec(),
                current: layout,
            },
            actions: ActionsSurface {
                path: actions::actions_path().display().to_string(),
                icons: ActionIcon::ALL.to_vec(),
                commands: BuiltinCommand::ALL.to_vec(),
                slots: ActionSlot::ALL.to_vec(),
                run_kinds: ActionRun::KINDS.to_vec(),
                permission_scopes: PermissionScope::ALL.to_vec(),
                elicitation_answers: ElicitationAnswer::ALL.to_vec(),
                limits: ActionsLimits {
                    max_bytes: actions::MAX_ACTIONS_BYTES,
                    max_actions: actions::MAX_ACTIONS,
                    max_id: actions::MAX_ID,
                    max_label: actions::MAX_LABEL,
                },
                current: actions,
            },
            panels: PanelsSurface {
                path: panels::panels_path().display().to_string(),
                block_kinds: PanelBlock::KINDS.to_vec(),
                layout_nodes: panels::LayoutNode::KINDS.to_vec(),
                layout_node_fields: panels::LayoutNode::FIELDS.to_vec(),
                layout_rule: "`when` is a field on every block and on every layout node. A node whose condition is false hides its whole subtree, so a `tabs`, `split` or `stack` can be guarded as one unit instead of guarding each block inside it. A hidden node's blocks still appear in the derived `blocks` view, because that view is structural: it says what the panel contains, not what it currently shows.",
                chart_styles: panels::ChartStyle::ALL.to_vec(),
                chart_stacks: panels::ChartStack::ALL.to_vec(),
                chart_scales: panels::ChartScale::ALL.to_vec(),
                diagram_layouts: panels::DiagramLayout::ALL.to_vec(),
                diagram_tones: panels::DiagramTone::ALL.to_vec(),
                diagram_routings: panels::DiagramRouting::ALL.to_vec(),
                bindings: panels
                    .panels
                    .iter()
                    .map(|panel| PanelBindings {
                        id: panel.id.clone(),
                        paths: panel.tree().paths(),
                    })
                    .collect(),
                source_kinds: vec!["builtin", "http"],
                builtin_sources: crate::data::BuiltinName::ALL.to_vec(),
                http_modes: crate::data::HttpMode::ALL.to_vec(),
                source_rule: "A source in panels.json may only address loopback: http://127.0.0.1, http://[::1] or http://localhost. https is refused, redirects are not followed, and a non-loopback host is not expressible in this file at all — it would need a settings.json entry the user edits by hand. Declare a source, then bind a path into it: source id `ci` is read as `/ci/...`.",
                expressions: ExpressionsSurface {
                    operators: crate::expr::operators(),
                    formats: crate::expr::FormatKind::ALL.to_vec(),
                    max_depth: crate::expr::MAX_EXPR_DEPTH,
                    max_nodes: crate::expr::MAX_EXPR_NODES,
                    path_grammar: "`/`-separated, starts with `/`, no `.` or `..`, no empty segment, no array index",
                    example: serde_json::json!({
                        "panels": [{
                            "id": "ci",
                            "title": "CI",
                            "width": 320,
                            "sources": [{
                                "id": "ci",
                                "type": "http",
                                "url": "http://127.0.0.1:8787/status",
                                "mode": "poll",
                                "intervalMs": 2000,
                                "timeoutMs": 5000
                            }],
                            "layout": {
                                "node": "tabs",
                                "tabs": [
                                    {
                                        "label": "Now",
                                        "children": [
                                            { "kind": "data", "label": "Queue", "value": { "@path": "/ci/queue/depth" }, "hint": "jobs" },
                                            { "kind": "progress", "label": "Budget", "value": { "@path": "/ci/spend" }, "max": 100 },
                                            {
                                                "kind": "text",
                                                "text": "Nothing is queued.",
                                                "when": { "@eq": [{ "@path": "/ci/queue/depth" }, 0] }
                                            }
                                        ]
                                    },
                                    {
                                        "label": "History",
                                        "children": [
                                            {
                                                "node": "stack",
                                                "when": { "@exists": "/ci/runs" },
                                                "children": [
                                                    { "kind": "chart", "label": "Throughput", "style": "line", "values": { "@path": "/ci/throughput" } },
                                                    { "kind": "table", "label": "Runs", "columns": ["id", "state"], "rows": { "@path": "/ci/runs" } }
                                                ]
                                            }
                                        ]
                                    }
                                ]
                            }
                        }]
                    }),
                },
                limits: PanelsLimits {
                    max_bytes: panels::MAX_PANELS_BYTES,
                    max_panels: panels::MAX_PANELS,
                    max_blocks: panels::MAX_BLOCKS,
                    max_layout_nodes: panels::MAX_LAYOUT_NODES,
                    max_layout_depth: panels::MAX_LAYOUT_DEPTH,
                    max_tabs: panels::MAX_TABS,
                    max_tab_label: panels::MAX_TAB_LABEL,
                    max_columns: panels::MAX_COLUMNS,
                    max_sources_per_panel: crate::data::MAX_SOURCES_PER_PANEL,
                    interval_min: crate::data::INTERVAL_MS.0,
                    interval_max: crate::data::INTERVAL_MS.1,
                    timeout_min: crate::data::TIMEOUT_MS.0,
                    timeout_max: crate::data::TIMEOUT_MS.1,
                    max_body_bytes: crate::data::MAX_BODY_BYTES,
                    max_json_depth: crate::data::MAX_JSON_DEPTH,
                    max_json_nodes: crate::data::MAX_JSON_NODES,
                    max_concurrent: crate::data::MAX_CONCURRENT,
                    width_min: panels::PANEL_WIDTH.0,
                    width_max: panels::PANEL_WIDTH.1,
                    sandbox_height_min: panels::SANDBOX_HEIGHT.0,
                    sandbox_height_max: panels::SANDBOX_HEIGHT.1,
                    sandbox_max_html: panels::MAX_SANDBOX_HTML,
                    chart_height_min: panels::CHART_HEIGHT.0,
                    chart_height_max: panels::CHART_HEIGHT.1,
                    chart_max_series: panels::MAX_CHART_SERIES,
                    max_chart_references: panels::MAX_CHART_REFERENCES,
                    diagram_height_min: panels::DIAGRAM_HEIGHT.0,
                    diagram_height_max: panels::DIAGRAM_HEIGHT.1,
                    diagram_max_nodes: panels::MAX_DIAGRAM_NODES,
                    diagram_max_edges: panels::MAX_DIAGRAM_EDGES,
                    diagram_rule: "A `diagram` draws a graph: `nodes` are boxes, `edges` are arrows, and `layout` picks the algorithm. `layered` ranks by longest path and orders each rank by barycentre; `radial` finds the cycle and closes it into a ring; `tree` is a tidy tree with children centred under their parent; `sequence` is lifelines and messages. Every visible field is an expression: a node's `label`, `detail`, `tone` and `when`, and an edge's `label`. `tone` resolves to one of the closed set above, so a node turns `danger` when a source says the service is down. A node with an `action` fires that entry from actions.json when clicked, which makes the diagram a control surface. `routing` is `orthogonal` (elbows through the gaps between ranks) or `curved`. There is no free-form x/y: that is a widget's job.",
                    chart_rule: "A `chart` draws a real plot: a y-axis with whole-number ticks, gridlines, x labels, up to four named series with a legend, and a hover readout. `values` is the single-series shorthand; `series` is `[{name, values}]`. `x` supplies one label per point and `time: true` reads those labels as epoch milliseconds. `yMin`/`yMax` pin the scale; without them a bar chart starts at zero and a line chart fits its data. `unit` is appended to every label. `style: \"sparkline\"` is the axis-free strip, kept as an explicit choice — use it only when the shape is all that matters. `stack` is `none` (side by side), `stacked` (accumulated, so the top is the total) or `percent` (accumulated and normalised to 100); only `bar` and `area` can stack, because a line has no thickness to accumulate. `scale: \"log\"` makes each decade the same height and drops non-positive points; it cannot be combined with `yMin`. `reference` is up to four horizontal threshold lines, `[{value, label, tone}]`, where `value` and `tone` are expressions — a budget that moves with a source, drawn in the same tone vocabulary a diagram node uses. Clicking a legend entry hides that series.", 
                    sandbox_forbidden: panels::SANDBOX_FORBIDDEN.to_vec(),
                    widget_height_min: panels::WIDGET_HEIGHT.0,
                    widget_height_max: panels::WIDGET_HEIGHT.1,
                    widget_max_html: panels::MAX_WIDGET_HTML,
                    widget_max_capabilities: panels::MAX_WIDGET_CAPABILITIES,
                    widget_forbidden: panels::WIDGET_FORBIDDEN.to_vec(),
                    widget_capabilities: vec!["prompt", "action", "file"],
                    widget_origin: crate::widget::ORIGIN,
                    widget_rule: "A `widget` block runs its own scripts. It is served from its own origin with `default-src 'none'; script-src 'unsafe-inline'; connect-src 'none'`, so it can draw anything and reach nothing: no network, no storage, no access to the app. It receives the panel's live data model and the live theme tokens, and it may only act through the capabilities its block declares — `{\"kind\":\"prompt\"}` to send text to the agent, `{\"kind\":\"action\",\"id\":\"<id>\"}` to fire one entry from actions.json, `{\"kind\":\"file\"}` to read a workspace file by relative path through `vibedesk.read(path)`. Anything else is refused and the refusal is reported back to the widget. `prompt` and an `action` whose run is a prompt resolve when the app has accepted and dispatched the request, not when the agent has answered — a turn takes minutes and a button cannot wait for it. A `file` read is capped at 2 MB and at 20 reads per 10 seconds, and a path that is absolute or climbs out of the workspace is refused. A widget may also declare its own `sources`, which run under the panel id and share the panel's source budget; that is how a widget draws a graph over a feed only it reads. Use a `sandbox` block when the content is markup only; use a `widget` when it has to compute, animate or respond.",
                    widget_src_rule: "A widget's html may be inline in `html`, or in a file named by `src` — `\"src\": \"widgets/<name>.html\"`, relative to the config directory, `.html` only. Prefer `src`: the file is read into `html` on load and stripped back out on save, so the two never drift, and a 64 KB widget stays a 64 KB file instead of a 64 KB escaped JSON string. A missing or oversized file is a load error naming the path, not a blank frame.",
                },
                current: panels,
            },
        },
        protected: Protected {
            selectors: customize::PROTECTED_SELECTORS.to_vec(),
            why: "These surfaces tell the user what the agent is asking. A theme that could restyle them could make the UI lie, so no source may target them.",
        },
    }
}

/// Write the manifest. Called on boot and whenever a config file changes.
///
/// A failure here is not fatal: the app works without it, the agent just loses
/// its map. So the error is returned rather than panicked on, and the caller
/// logs it.
pub fn write() -> Result<PathBuf, String> {
    write_to(&manifest_path())
}

pub fn write_to(path: &std::path::Path) -> Result<PathBuf, String> {
    write_manifest_to(path, &build())
}

/// The path-taking half, so the size tripwire can be tested with a synthetic
/// document instead of a real 256 KB config on disk.
pub fn write_manifest_to(path: &std::path::Path, manifest: &Manifest) -> Result<PathBuf, String> {
    let body = serde_json::to_string_pretty(manifest)
        .map_err(|e| format!("could not serialise the manifest: {e}"))?;
    if body.len() > MAX_MANIFEST_BYTES {
        return Err(format!(
            "the manifest is {} bytes; the limit is {MAX_MANIFEST_BYTES}",
            body.len()
        ));
    }

    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    }

    // Same temp-then-rename as the config writers: the agent may read this file
    // at any moment, and a half-written manifest is worse than a stale one.
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, &body)
        .map_err(|e| format!("could not write {}: {e}", temp.display()))?;
    std::fs::rename(&temp, path)
        .map_err(|e| format!("could not replace {}: {e}", path.display()))?;

    Ok(path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The drift guard. If a variant is added to one of these enums without
    /// being added to its `ALL`, the manifest would silently omit it and the
    /// agent would be told a valid name is invalid. These fail instead.
    #[test]
    fn every_icon_is_listed() {
        assert_eq!(ActionIcon::ALL.len(), 20);
        let mut names: Vec<String> = ActionIcon::ALL
            .iter()
            .map(|icon| serde_json::to_string(icon).unwrap().trim_matches('"').to_string())
            .collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), ActionIcon::ALL.len(), "an icon is listed twice");
    }

    #[test]
    fn every_command_is_listed() {
        assert_eq!(BuiltinCommand::ALL.len(), 15);
        let mut names: Vec<String> = BuiltinCommand::ALL
            .iter()
            .map(|c| serde_json::to_string(c).unwrap().trim_matches('"').to_string())
            .collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), BuiltinCommand::ALL.len());
    }

    #[test]
    fn every_block_kind_is_listed() {
        assert_eq!(PanelBlock::KINDS.len(), 12);
        let mut kinds = PanelBlock::KINDS.to_vec();
        kinds.sort();
        kinds.dedup();
        assert_eq!(kinds.len(), PanelBlock::KINDS.len());
    }

    /// A widget is the one block that runs scripts, so the manifest has to say
    /// what it may do and what it may not. An agent that only sees the word
    /// `widget` in `blockKinds` will guess, and the guess will be a CDN script
    /// tag that the validator refuses.
    #[test]
    fn the_manifest_explains_the_widget_boundary() {
        let manifest = build();
        let surface = &manifest.surfaces.panels;

        assert!(surface.block_kinds.contains(&"widget"));
        assert_eq!(
            surface.limits.widget_capabilities,
            vec!["prompt", "action", "file"]
        );
        assert_eq!(surface.limits.widget_origin, crate::widget::ORIGIN);
        assert!(surface.limits.widget_rule.contains("refused"));
        assert!(surface.limits.widget_rule.contains("no network"));
        assert!(surface.limits.widget_rule.contains("vibedesk.read"));
        assert!(surface.limits.widget_rule.contains("sources"));
        assert!(surface.limits.widget_src_rule.contains("widgets/<name>.html"));
        assert!(surface.limits.widget_forbidden.contains(&"<script src"));
        assert_eq!(surface.limits.widget_height_min, panels::WIDGET_HEIGHT.0);
        assert_eq!(surface.limits.widget_height_max, panels::WIDGET_HEIGHT.1);
        assert_eq!(surface.limits.widget_max_html, panels::MAX_WIDGET_HTML);
    }

    #[test]
    fn every_layout_node_is_listed() {
        assert_eq!(panels::LayoutNode::KINDS.len(), 3);
        let mut kinds = panels::LayoutNode::KINDS.to_vec();
        kinds.sort();
        kinds.dedup();
        assert_eq!(kinds.len(), panels::LayoutNode::KINDS.len());
    }

    /// A guarded container is the reason `when` is on nodes and not only on
    /// blocks, so the manifest has to publish the field and the example has to
    /// show it. An agent that only ever sees `when` on a block will guard every
    /// leaf instead of the subtree.
    #[test]
    fn the_manifest_says_a_layout_node_can_be_guarded() {
        let manifest = build();
        let surface = &manifest.surfaces.panels;

        assert_eq!(
            surface.layout_node_fields,
            panels::LayoutNode::FIELDS.to_vec()
        );
        assert!(surface.layout_node_fields.contains(&"when"));
        assert!(surface.layout_rule.contains("hides its whole subtree"));

        let example = &surface.expressions.example;
        let guarded = example["panels"][0]["layout"]["tabs"][1]["children"][0]
            .get("when")
            .is_some();
        assert!(guarded, "the example does not guard a layout node: {example}");
    }

    #[test]
    fn every_builtin_source_and_http_mode_is_listed() {
        let manifest = build();
        let surface = &manifest.surfaces.panels;

        assert_eq!(surface.builtin_sources.len(), 6);
        for name in crate::data::BuiltinName::ALL {
            assert!(
                surface.builtin_sources.contains(&name),
                "{name} is missing from the manifest"
            );
        }

        assert_eq!(surface.http_modes.len(), 2);
        for mode in crate::data::HttpMode::ALL {
            assert!(surface.http_modes.contains(&mode), "{mode} is missing");
        }

        assert_eq!(surface.source_kinds, vec!["builtin", "http"]);
        // The rule has to name the hosts, or the agent will guess one.
        assert!(surface.source_rule.contains("127.0.0.1"));
        assert!(surface.source_rule.contains("localhost"));
        assert!(surface.source_rule.contains("https"));
    }

    /// The example is the thing the agent copies. Its source must be one the
    /// validator accepts, or the documentation teaches a refused shape.
    #[test]
    fn the_published_example_declares_a_valid_source() {
        let manifest = build();
        let example = manifest.surfaces.panels.expressions.example;
        let raw = serde_json::to_string(&example).unwrap();
        let parsed: PanelsConfig = serde_json::from_str(&raw).unwrap();
        let upgraded = parsed.clamped();
        panels::validate(&upgraded).unwrap();

        let sources = &upgraded.panels[0].sources;
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].id, "ci");
        assert!(sources[0].http().is_some());
    }

    /// The manifest's operator list is what the agent reads before writing a
    /// binding. If it drifts from `expr.rs`, the agent authors something the
    /// parser rejects and the receipt blames the agent for a documentation bug.
    #[test]
    fn the_manifest_operators_are_exactly_the_parsers() {
        let manifest = build();
        let listed = manifest.surfaces.panels.expressions.operators;
        let mut sorted = listed.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), listed.len(), "an operator is listed twice");

        for token in &listed {
            let probe = match *token {
                "@path" | "@exists" | "@missing" => serde_json::json!({ *token: "/a" }),
                "@not" => serde_json::json!({ "@not": true }),
                "@and" | "@or" => serde_json::json!({ *token: [true, false] }),
                "@format" => serde_json::json!({ "@format": "number", "@of": 1 }),
                "@if" => serde_json::json!({ "@if": [true, 1, 2] }),
                _ => serde_json::json!({ *token: [1, 2] }),
            };
            assert!(
                crate::expr::Expr::checked(&probe).is_ok(),
                "the manifest lists `{token}` but the parser rejects it"
            );
        }
    }

    /// The example is the one thing the agent is most likely to copy. It has to
    /// load, validate, and survive a round trip, or it teaches a broken shape.
    #[test]
    fn the_published_example_is_a_valid_panel() {
        let manifest = build();
        let example = manifest.surfaces.panels.expressions.example;
        let raw = serde_json::to_string(&example).unwrap();
        let parsed: PanelsConfig = serde_json::from_str(&raw).unwrap();
        let upgraded = parsed.clamped();
        panels::validate(&upgraded).unwrap();

        let panel = &upgraded.panels[0];
        assert_eq!(panel.blocks.len(), 5, "the example lost a block");
        assert!(panel.layout.is_some());

        let again = serde_json::to_string(&upgraded).unwrap();
        let reparsed: PanelsConfig = serde_json::from_str(&again).unwrap();
        panels::validate(&reparsed).unwrap();
    }

    /// The manifest must name the same files the pending scanner accepts, or the
    /// agent writes a file that is reported as a stray.
    #[test]
    fn pending_files_match_the_scanner() {
        let manifest = build();
        let expected: Vec<&str> = ConfigKind::ALL.iter().map(|k| k.file_name()).collect();
        assert_eq!(manifest.pending.files, expected);
        assert_eq!(manifest.pending.receipts.len(), expected.len());
    }

    /// Every layout field the manifest publishes must be one the struct has, and
    /// its bounds must be the ones the clamp uses.
    #[test]
    fn layout_fields_match_the_clamp() {
        let manifest = build();
        let fields = &manifest.surfaces.layout.fields;
        assert_eq!(fields.len(), 4);
        let rail = fields.iter().find(|f| f.name == "railWidth").unwrap();
        assert_eq!((rail.min, rail.max), layout::RAIL_WIDTH);
        let sidebar = fields.iter().find(|f| f.name == "sidebarWidth").unwrap();
        assert_eq!((sidebar.min, sidebar.max), layout::SIDEBAR_WIDTH);
    }

    /// The protected list in the manifest is the one the validator enforces.
    #[test]
    fn protected_selectors_are_the_enforced_ones() {
        let manifest = build();
        assert_eq!(manifest.protected.selectors, customize::PROTECTED_SELECTORS);
        assert!(manifest.protected.selectors.contains(&".vd-approval"));
    }

    #[test]
    fn writes_a_parseable_document() {
        let dir = std::env::temp_dir().join(format!("vd-manifest-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(MANIFEST_FILE);

        write_to(&path).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();

        assert_eq!(value["version"], 1);
        assert!(value["surfaces"]["actions"]["icons"].is_array());
        assert!(value["surfaces"]["panels"]["blockKinds"].is_array());
        assert!(value["pending"]["dir"].is_string());
        // The temp file must not survive.
        assert!(!path.with_extension("json.tmp").exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The vocabulary must come from the stylesheet, not from the user's
    /// overrides. An empty list on a fresh install told the agent that nothing
    /// was customizable, and it correctly refused to propose anything.
    #[test]
    fn the_token_vocabulary_is_the_stylesheet_not_the_overrides() {
        let manifest = build();
        let tokens = &manifest.surfaces.theme.tokens;

        assert!(tokens.len() > 20, "only {} tokens", tokens.len());
        assert!(
            tokens.iter().any(|t| t.name == "--vd-accent"),
            "--vd-accent is missing from the vocabulary"
        );
        assert!(tokens.iter().all(|t| t.name.starts_with("--vd-")));
        assert!(tokens.iter().all(|t| !t.value.is_empty()));

        // Sorted and unique, so the document is stable between runs.
        let mut names: Vec<&str> = tokens.iter().map(|t| t.name.as_str()).collect();
        let sorted = {
            let mut copy = names.clone();
            copy.sort();
            copy
        };
        assert_eq!(names, sorted);
        names.dedup();
        assert_eq!(names.len(), tokens.len());

        // Layout owns its own tokens; publishing them would offer a knob that
        // `applyLayout` silently overrides.
        for owned in LAYOUT_OWNED {
            assert!(
                !tokens.iter().any(|t| t.name == *owned),
                "{owned} is owned by layout.json and must not be in the theme vocabulary"
            );
        }
    }

    /// A user override must not change the vocabulary, only `current`.
    #[test]
    fn an_override_does_not_shrink_the_vocabulary() {
        let manifest = build();
        let vocabulary = manifest.surfaces.theme.tokens.len();
        // `current` is whatever is on disk; the vocabulary is the stylesheet.
        assert!(vocabulary > 20);
        assert!(manifest.surfaces.theme.current.tokens.len() <= vocabulary);
    }

    /// A manifest is written by us, so it must stay small even with a large
    /// theme. This is the tripwire, not a defence.
    #[test]
    fn stays_under_the_cap() {
        let manifest = build();
        let body = serde_json::to_string_pretty(&manifest).unwrap();
        assert!(body.len() < MAX_MANIFEST_BYTES);
    }

    /// The tripwire itself: a document past the cap is refused rather than
    /// written, so a runaway config cannot produce a manifest the agent would
    /// have to read in pieces.
    #[test]
    fn a_document_past_the_cap_is_refused() {
        let dir = std::env::temp_dir().join(format!("vd-manifest-cap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(MANIFEST_FILE);

        let mut manifest = build();
        // 10 000 tokens is far past anything a real theme holds.
        for index in 0..10_000 {
            manifest.surfaces.theme.tokens.push(TokenInfo {
                name: format!("--vd-filler-{index}"),
                value: "#ff6b35".into(),
            });
        }

        let error = write_manifest_to(&path, &manifest).unwrap_err();
        assert!(error.contains("the limit is"), "{error}");
        assert!(!path.exists(), "a refused manifest must not land");

        std::fs::remove_dir_all(&dir).ok();
    }
}
