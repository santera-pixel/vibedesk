//! Where a customization comes from, and who has to say yes.
//!
//! Two sources beyond the user's own files:
//!
//! * **Plugins** ship a `vibedesk/` folder next to their manifest. Discovery is
//!   automatic; application is not. A plugin that could restyle the app just by
//!   being installed would be a supply-chain hole, so its configs are listed and
//!   wait for the user to pick one.
//! * **The agent** writes a proposal into `<data>/config/pending/`. VibeDesk
//!   notices it, validates it, and asks. The agent's own file-write permission
//!   is a different authority from this one: that gate is about touching the
//!   filesystem, this one is about changing the app's own chrome. Both must pass.
//!
//! Neither source gets a shortcut through validation. `normalise` runs the same
//! `validate` the user's own files go through, so protected surfaces, value
//! escapes, closed enums and the no-eager-read rule apply identically. There is
//! deliberately no "trusted" flag and no source that skips the check.
//!
//! What gets written is the **normalised** body, not the original bytes: parse,
//! validate, re-serialise, then write. Anything serde ignored never reaches
//! disk, and the digest the user approved is a digest of exactly what lands.
//!
//! That digest is also the fix for a real race. Between showing a proposal and
//! applying it the file can change — the agent is a process that writes files,
//! and it does not have to wait for the user. So `apply` takes the digest the UI
//! displayed and refuses a mismatch. Without it, a user could approve "3 tokens"
//! and promote whatever was written a moment later.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::actions::{self, ActionsConfig};
use crate::customize::{self, ThemeConfig};
use crate::layout::{self, LayoutConfig};
use crate::panels::{self, PanelsConfig};

/// Emitted when the pending directory changes.
pub const EVENT_PENDING: &str = "config://pending";

/// The four kinds of customization a source may propose. Closed, like every
/// other enum in this module's neighbourhood: a source cannot invent a fifth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConfigKind {
    Theme,
    Layout,
    Actions,
    Panels,
}

impl ConfigKind {
    pub const ALL: [ConfigKind; 4] = [Self::Theme, Self::Layout, Self::Actions, Self::Panels];

    pub fn file_name(self) -> &'static str {
        match self {
            Self::Theme => "theme.json",
            Self::Layout => "layout.json",
            Self::Actions => "actions.json",
            Self::Panels => "panels.json",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Theme => "theme",
            Self::Layout => "layout",
            Self::Actions => "actions",
            Self::Panels => "panels",
        }
    }

    pub fn max_bytes(self) -> u64 {
        match self {
            Self::Theme => customize::MAX_THEME_BYTES,
            Self::Layout => layout::MAX_LAYOUT_BYTES,
            Self::Actions => actions::MAX_ACTIONS_BYTES,
            Self::Panels => panels::MAX_PANELS_BYTES,
        }
    }

    /// Where the outcome of a decision is written, next to the proposal.
    ///
    /// The agent writes a proposal and then has no way to learn what happened to
    /// it: whether the user applied it, discarded it, or whether it never became
    /// a proposal at all because it failed validation. Without that the agent
    /// cannot iterate, which is the whole point of letting it customize the app.
    pub fn receipt_name(self) -> &'static str {
        match self {
            Self::Theme => "theme.result.json",
            Self::Layout => "layout.result.json",
            Self::Actions => "actions.result.json",
            Self::Panels => "panels.result.json",
        }
    }
}

/// A validated proposal waiting for a decision.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Proposal {
    pub kind: ConfigKind,
    pub source: String,
    pub path: String,
    pub summary: String,
    /// What actually moves, field by field. The summary says "7 tokens"; this
    /// says which seven and what they become, which is the difference between a
    /// decision the user can make and one they have to guess at.
    pub changes: Vec<Change>,
    /// sha256 of the normalised body. `apply` refuses a mismatch.
    pub digest: String,
}

/// One field a proposal moves.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Change {
    /// The token name, layout field, or action/panel id, as the file names it.
    pub field: String,
    /// What it is now. Empty when the field is new.
    pub from: String,
    /// What it becomes. Empty when the field is removed.
    pub to: String,
}

/// The most changes the bar will show. A theme that moves forty tokens is a
/// rewrite, not a decision, and a list that long is not read — the count in the
/// summary still tells the truth about the rest.
pub const MAX_CHANGES: usize = 24;

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingState {
    pub proposals: Vec<Proposal>,
    pub issues: Vec<String>,
    /// The last decision per kind, so the approval bar can show the user exactly
    /// what the agent is about to read.
    pub receipts: Vec<Receipt>,
}

/// What happened to a proposal, written where the agent can read it.
///
/// `applied` is the only field the agent has to branch on. `error` carries the
/// validator's own message when a file never became a proposal, which is the
/// case that used to be invisible: the agent wrote something invalid, the UI
/// listed it as an issue, and the agent was never told why.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Receipt {
    pub kind: ConfigKind,
    pub applied: bool,
    pub at: String,
    /// Digest of the body that was decided on, or of the rejected bytes.
    pub digest: String,
    pub summary: String,
    pub error: Option<String>,
}

/// A customization a plugin ships. `valid` is false when the file is present but
/// would be rejected, so the UI can show why instead of hiding it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginSource {
    pub plugin: String,
    pub scope: String,
    pub kind: ConfigKind,
    pub path: String,
    pub summary: String,
    pub digest: String,
    pub valid: bool,
    pub error: Option<String>,
}

/* ------------------------------------------------------------------ *
 * Paths
 * ------------------------------------------------------------------ */

pub fn pending_dir() -> PathBuf {
    customize::config_dir().join("pending")
}

pub fn pending_path(kind: ConfigKind) -> PathBuf {
    pending_dir().join(kind.file_name())
}

pub fn receipt_path(kind: ConfigKind) -> PathBuf {
    pending_dir().join(kind.receipt_name())
}

/// Write the outcome of a decision where the agent will look for it.
///
/// Best-effort: a receipt that cannot be written must not fail the apply that
/// already succeeded, so the error is returned for the caller to log and the
/// config change stands either way.
///
/// `digest` is what makes this idempotent. The pending directory is polled every
/// 400 ms, and an invalid file stays invalid, so without the digest check the
/// same failure receipt would be rewritten on every tick — and the agent would
/// see a fresh timestamp for a decision the user made once.
pub fn write_receipt_at(
    dir: &Path,
    kind: ConfigKind,
    applied: bool,
    digest: &str,
    summary: &str,
    error: Option<&str>,
) -> Result<(), String> {
    let path = dir.join(kind.receipt_name());

    // Same decision, same file: leave the original timestamp alone.
    if let Ok(raw) = std::fs::read_to_string(&path) {
        if let Ok(existing) = serde_json::from_str::<Receipt>(&raw) {
            if existing.digest == digest && existing.applied == applied {
                return Ok(());
            }
        }
    }

    let receipt = Receipt {
        kind,
        applied,
        at: chrono::Utc::now().to_rfc3339(),
        digest: digest.to_string(),
        summary: summary.to_string(),
        error: error.map(|e| e.to_string()),
    };
    let body = serde_json::to_string_pretty(&receipt)
        .map_err(|e| format!("could not serialise the receipt: {e}"))?;

    std::fs::create_dir_all(dir)
        .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, &body)
        .map_err(|e| format!("could not write {}: {e}", temp.display()))?;
    std::fs::rename(&temp, &path)
        .map_err(|e| format!("could not replace {}: {e}", path.display()))?;
    Ok(())
}

/* ------------------------------------------------------------------ *
 * The one place a body becomes trustworthy
 * ------------------------------------------------------------------ */

/// Parse, validate, re-serialise. The returned string is exactly what would be
/// written, which is what makes the digest meaningful.
fn normalise(kind: ConfigKind, raw: &str) -> Result<String, String> {
    if raw.trim().is_empty() {
        return Err(format!("{} is empty", kind.file_name()));
    }

    match kind {
        ConfigKind::Theme => {
            let value: ThemeConfig = serde_json::from_str(raw)
                .map_err(|e| format!("theme.json is not valid JSON: {e}"))?;
            customize::validate(&value)?;
            serde_json::to_string_pretty(&value)
                .map_err(|e| format!("could not serialise the theme: {e}"))
        }
        ConfigKind::Layout => {
            let value: LayoutConfig = serde_json::from_str(raw)
                .map_err(|e| format!("layout.json is not valid JSON: {e}"))?;
            // Layout has no separate `validate`: an unknown enum is a parse
            // error, and numbers are clamped rather than rejected.
            let value = value.clamped();
            serde_json::to_string_pretty(&value)
                .map_err(|e| format!("could not serialise the layout: {e}"))
        }
        ConfigKind::Actions => {
            let value: ActionsConfig = serde_json::from_str(raw)
                .map_err(|e| format!("actions.json is not valid JSON: {e}"))?;
            actions::validate(&value)?;
            serde_json::to_string_pretty(&value)
                .map_err(|e| format!("could not serialise the actions: {e}"))
        }
        ConfigKind::Panels => {
            let value: PanelsConfig = serde_json::from_str(raw)
                .map_err(|e| format!("panels.json is not valid JSON: {e}"))?;
            panels::validate(&value)?;
            serde_json::to_string_pretty(&value.clamped())
                .map_err(|e| format!("could not serialise the panels: {e}"))
        }
    }
}

fn digest_of(body: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(body.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Read a file that a source controls, refusing anything that is not a plain
/// regular file of a sane size.
///
/// `symlink_metadata` rather than `metadata`: a plugin must not be able to point
/// `vibedesk/theme.json` at a file it does not own, and a directory named
/// `theme.json` must not be read as one.
fn read_limited(path: &Path, kind: ConfigKind) -> Result<String, String> {
    let meta = std::fs::symlink_metadata(path)
        .map_err(|e| format!("could not read {}: {e}", path.display()))?;
    if !meta.file_type().is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    if meta.len() > kind.max_bytes() {
        return Err(format!(
            "{} is {} bytes; the limit is {}",
            kind.file_name(),
            meta.len(),
            kind.max_bytes()
        ));
    }
    std::fs::read_to_string(path).map_err(|e| format!("could not read {}: {e}", path.display()))
}

/// Write a normalised body to the live config file. The body has already been
/// validated, and the `save_*_to` functions validate again — belt and braces,
/// because this is the one function that changes what the user sees.
fn promote(live_dir: &Path, kind: ConfigKind, body: &str) -> Result<(), String> {
    match kind {
        ConfigKind::Theme => {
            let value: ThemeConfig = serde_json::from_str(body)
                .map_err(|e| format!("could not re-read the theme: {e}"))?;
            customize::save_theme_to(&live_dir.join(kind.file_name()), &value).map(|_| ())
        }
        ConfigKind::Layout => {
            let value: LayoutConfig = serde_json::from_str(body)
                .map_err(|e| format!("could not re-read the layout: {e}"))?;
            layout::save_layout_to(&live_dir.join(kind.file_name()), &value).map(|_| ())
        }
        ConfigKind::Actions => {
            let value: ActionsConfig = serde_json::from_str(body)
                .map_err(|e| format!("could not re-read the actions: {e}"))?;
            actions::save_actions_to(&live_dir.join(kind.file_name()), &value).map(|_| ())
        }
        ConfigKind::Panels => {
            let value: PanelsConfig = serde_json::from_str(body)
                .map_err(|e| format!("could not re-read the panels: {e}"))?;
            panels::save_panels_to(&live_dir.join(kind.file_name()), &value).map(|_| ())
        }
    }
}

/// A one-line description of what applying this would do, for the approval bar.
/// The live config a proposal would replace, as raw bytes.
///
/// Read from the real config directory rather than the pending one: the point of
/// the diff is what the proposal does to what is *running*, not to whatever else
/// happens to be sitting in `pending/`.
fn current_body(kind: ConfigKind) -> Option<String> {
    if let Ok(body) = std::fs::read_to_string(customize::config_dir().join(kind.file_name())) {
        return Some(body);
    }
    // No file means the app is running the built-in defaults, and a diff against
    // "nothing" would render every field as an addition — `— → #ff7414` tells the
    // user nothing about what they are changing. Synthesise the baseline the app
    // is actually wearing so the "from" column is true.
    baseline(kind)
}

/// The config the app runs when the corresponding file is absent.
///
/// Theme is deliberately absent. A `theme.json` *merges* over the built-in
/// stylesheet — `applyTheme` writes a `<style>` element and any token it omits
/// keeps the value from `globals.css`. So "no theme.json" means "nothing is
/// overridden", not "every token is removed", and synthesising the full built-in
/// vocabulary here would report 23 phantom removals for a one-token proposal.
/// The UI fills the blank "from" cell from the live computed value instead, which
/// is the only place that knows which theme is on screen.
fn baseline(kind: ConfigKind) -> Option<String> {
    let body = match kind {
        ConfigKind::Theme => return None,
        ConfigKind::Layout => serde_json::to_string(&LayoutConfig::default()),
        ConfigKind::Actions => serde_json::to_string(&ActionsConfig::default()),
        ConfigKind::Panels => serde_json::to_string(&PanelsConfig::default()),
    };
    body.ok()
}

/// What a proposal moves, field by field.
///
/// Pure, and takes the current body as an argument rather than reading it, so it
/// can be tested without a config directory. A missing or unparseable current
/// file is treated as the defaults — which is what the app is actually running in
/// that case, so the diff stays truthful.
pub fn diff(kind: ConfigKind, proposed: &str, current: Option<&str>) -> Vec<Change> {
    let mut changes = match kind {
        ConfigKind::Theme => {
            let Ok(to) = serde_json::from_str::<ThemeConfig>(proposed) else {
                return Vec::new();
            };
            let from = current
                .and_then(|body| serde_json::from_str::<ThemeConfig>(body).ok())
                .unwrap_or_default();
            diff_theme(&to, &from)
        }
        ConfigKind::Layout => {
            let Ok(to) = serde_json::from_str::<LayoutConfig>(proposed) else {
                return Vec::new();
            };
            let from = current
                .and_then(|body| serde_json::from_str::<LayoutConfig>(body).ok())
                .unwrap_or_default();
            diff_layout(&to, &from)
        }
        ConfigKind::Actions => {
            let Ok(to) = serde_json::from_str::<ActionsConfig>(proposed) else {
                return Vec::new();
            };
            let from = current
                .and_then(|body| serde_json::from_str::<ActionsConfig>(body).ok())
                .unwrap_or_default();
            let to: Vec<(String, String)> = to
                .actions
                .iter()
                .map(|a| (a.id.clone(), a.label.clone()))
                .collect();
            let from: Vec<(String, String)> = from
                .actions
                .iter()
                .map(|a| (a.id.clone(), a.label.clone()))
                .collect();
            diff_by_id(&to, &from)
        }
        ConfigKind::Panels => {
            let Ok(to) = serde_json::from_str::<PanelsConfig>(proposed) else {
                return Vec::new();
            };
            let from = current
                .and_then(|body| serde_json::from_str::<PanelsConfig>(body).ok())
                .unwrap_or_default();
            let to: Vec<(String, String)> = to
                .panels
                .iter()
                .map(|p| (p.id.clone(), p.title.clone()))
                .collect();
            let from: Vec<(String, String)> = from
                .panels
                .iter()
                .map(|p| (p.id.clone(), p.title.clone()))
                .collect();
            diff_by_id(&to, &from)
        }
    };
    changes.truncate(MAX_CHANGES);
    changes
}

fn diff_theme(to: &ThemeConfig, from: &ThemeConfig) -> Vec<Change> {
    let mut changes = Vec::new();
    // `BTreeMap`, so the order is the file's own and stable between polls.
    for (name, value) in &to.tokens {
        let before = from.tokens.get(name).cloned().unwrap_or_default();
        if &before != value {
            changes.push(Change {
                field: name.clone(),
                from: before,
                to: value.clone(),
            });
        }
    }
    for (name, value) in &from.tokens {
        if !to.tokens.contains_key(name) {
            changes.push(Change {
                field: name.clone(),
                from: value.clone(),
                to: String::new(),
            });
        }
    }
    if to.rules.len() != from.rules.len() {
        changes.push(Change {
            field: "rules".into(),
            from: from.rules.len().to_string(),
            to: to.rules.len().to_string(),
        });
    }
    changes
}

fn diff_layout(to: &LayoutConfig, from: &LayoutConfig) -> Vec<Change> {
    let pairs = [
        (
            "railWidth",
            from.rail_width.to_string(),
            to.rail_width.to_string(),
        ),
        (
            "sidebarWidth",
            from.sidebar_width.to_string(),
            to.sidebar_width.to_string(),
        ),
        (
            "sidebarSide",
            format!("{:?}", from.sidebar_side).to_lowercase(),
            format!("{:?}", to.sidebar_side).to_lowercase(),
        ),
        (
            "sidebarCollapsed",
            from.sidebar_collapsed.to_string(),
            to.sidebar_collapsed.to_string(),
        ),
        (
            "transcriptMax",
            from.transcript_max.to_string(),
            to.transcript_max.to_string(),
        ),
        (
            "composerMax",
            from.composer_max.to_string(),
            to.composer_max.to_string(),
        ),
        (
            "density",
            format!("{:?}", from.density).to_lowercase(),
            format!("{:?}", to.density).to_lowercase(),
        ),
    ];
    pairs
        .into_iter()
        .filter(|(_, before, after)| before != after)
        .map(|(field, from, to)| Change {
            field: field.into(),
            from,
            to,
        })
        .collect()
}

/// Actions and panels are lists keyed by id, so the interesting question is not
/// "how many" but "which one appeared, vanished or was renamed".
fn diff_by_id(to: &[(String, String)], from: &[(String, String)]) -> Vec<Change> {
    let mut changes = Vec::new();
    for (id, label) in to {
        match from.iter().find(|(other, _)| other == id) {
            None => changes.push(Change {
                field: id.clone(),
                from: String::new(),
                to: label.clone(),
            }),
            Some((_, before)) if before != label => changes.push(Change {
                field: id.clone(),
                from: before.clone(),
                to: label.clone(),
            }),
            Some(_) => {}
        }
    }
    for (id, label) in from {
        if !to.iter().any(|(other, _)| other == id) {
            changes.push(Change {
                field: id.clone(),
                from: label.clone(),
                to: String::new(),
            });
        }
    }
    changes
}

fn summarise(kind: ConfigKind, body: &str) -> String {
    match kind {
        ConfigKind::Theme => serde_json::from_str::<ThemeConfig>(body)
            .map(|theme| {
                let tokens = theme.tokens.len();
                let rules = theme.rules.len();
                match (tokens, rules) {
                    (0, 0) => "no tokens, no rules".to_string(),
                    (t, 0) => format!("{t} token{}", plural(t)),
                    (0, r) => format!("{r} rule{}", plural(r)),
                    (t, r) => format!("{t} token{}, {r} rule{}", plural(t), plural(r)),
                }
            })
            .unwrap_or_else(|_| "theme".into()),
        ConfigKind::Layout => serde_json::from_str::<LayoutConfig>(body)
            .map(|l| {
                format!(
                    "rail {}px, sidebar {}px {:?}, {:?}",
                    l.rail_width, l.sidebar_width, l.sidebar_side, l.density
                )
                .to_lowercase()
            })
            .unwrap_or_else(|_| "layout".into()),
        ConfigKind::Actions => serde_json::from_str::<ActionsConfig>(body)
            .map(|config| {
                let labels: Vec<&str> = config
                    .actions
                    .iter()
                    .take(3)
                    .map(|a| a.label.as_str())
                    .collect();
                let more = config.actions.len().saturating_sub(labels.len());
                let tail = if more > 0 {
                    format!(" +{more} more")
                } else {
                    String::new()
                };
                format!(
                    "{} button{}: {}{tail}",
                    config.actions.len(),
                    plural(config.actions.len()),
                    labels.join(", ")
                )
            })
            .unwrap_or_else(|_| "actions".into()),
        ConfigKind::Panels => serde_json::from_str::<PanelsConfig>(body)
            .map(|config| {
                let titles: Vec<&str> = config
                    .panels
                    .iter()
                    .take(3)
                    .map(|p| p.title.as_str())
                    .collect();
                let more = config.panels.len().saturating_sub(titles.len());
                let tail = if more > 0 {
                    format!(" +{more} more")
                } else {
                    String::new()
                };
                format!(
                    "{} panel{}: {}{tail}",
                    config.panels.len(),
                    plural(config.panels.len()),
                    titles.join(", ")
                )
            })
            .unwrap_or_else(|_| "panels".into()),
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/* ------------------------------------------------------------------ *
 * Agent proposals
 * ------------------------------------------------------------------ */

pub fn read_pending() -> PendingState {
    read_pending_at(&pending_dir(), customize::safe_mode())
}

/// Scan the pending directory. A file that fails validation is reported as an
/// issue rather than offered, so the approval bar never shows something that
/// would be refused on apply.
pub fn read_pending_at(dir: &Path, safe: bool) -> PendingState {
    let mut state = PendingState::default();
    if safe {
        return state;
    }

    for kind in ConfigKind::ALL {
        let path = dir.join(kind.file_name());
        if !path.exists() {
            continue;
        }
        let raw = match read_limited(&path, kind) {
            Ok(raw) => raw,
            Err(error) => {
                // The agent wrote something that never became a proposal. Tell
                // it why, or it will write the same file again.
                let _ = write_receipt_at(dir, kind, false, &digest_of(&error), "", Some(&error));
                state.issues.push(error);
                continue;
            }
        };
        let body = match normalise(kind, &raw) {
            Ok(body) => body,
            Err(error) => {
                let _ = write_receipt_at(
                    dir,
                    kind,
                    false,
                    &digest_of(&raw),
                    "",
                    Some(&format!("{}: {error}", kind.file_name())),
                );
                state.issues.push(format!("{}: {error}", kind.file_name()));
                continue;
            }
        };
        state.proposals.push(Proposal {
            kind,
            source: "agent".into(),
            path: path.display().to_string(),
            summary: summarise(kind, &body),
            changes: diff(kind, &body, current_body(kind).as_deref()),
            digest: digest_of(&body),
        });
    }

    // Anything else in the directory is a mistake worth naming: the agent may
    // have invented a fifth kind, and silently ignoring it would leave it
    // waiting forever.
    //
    // The comparison is case-insensitive because the filesystem is: on Windows
    // `Theme.JSON` *is* `theme.json`, so it was already read above and calling
    // it a stray would report the same file twice.
    //
    // Receipts live in this directory too, so they are excluded by name — they
    // are ours, not the agent's, and reporting them as strays would put four
    // permanent issues in the approval bar.
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let known = ConfigKind::ALL.iter().any(|kind| {
                kind.file_name().eq_ignore_ascii_case(name)
                    || kind.receipt_name().eq_ignore_ascii_case(name)
            });
            if !known {
                state.issues.push(format!(
                    "`{name}` is not one of theme.json, layout.json, actions.json, panels.json"
                ));
            }
        }
    }

    // Receipts are collected *after* the proposal loop, not before. A file that
    // fails validation writes its receipt during that loop, and the pending
    // directory is only re-read when a proposal file's stamp moves — so reading
    // receipts first would mean the UI never sees the receipt for the very
    // failure it just reported.
    for kind in ConfigKind::ALL {
        if let Ok(raw) = std::fs::read_to_string(dir.join(kind.receipt_name())) {
            if let Ok(receipt) = serde_json::from_str::<Receipt>(&raw) {
                state.receipts.push(receipt);
            }
        }
    }

    state
}

pub fn apply_pending(kind: ConfigKind, digest: &str) -> Result<(), String> {
    apply_pending_at(
        &pending_dir(),
        &customize::config_dir(),
        kind,
        digest,
        customize::safe_mode(),
    )
}

/// Promote a pending proposal to the live config.
///
/// The digest is the caller's proof that it is applying what it showed. A
/// mismatch means the file moved underneath the UI, and the honest answer is to
/// make the user look again rather than to promote the new content.
pub fn apply_pending_at(
    pending: &Path,
    live: &Path,
    kind: ConfigKind,
    digest: &str,
    safe: bool,
) -> Result<(), String> {
    if safe {
        return Err("safe mode is on; customization is disabled".into());
    }

    let path = pending.join(kind.file_name());
    let raw = read_limited(&path, kind)?;
    let body = normalise(kind, &raw)?;

    if digest_of(&body) != digest {
        return Err(
            "the proposal changed after it was shown; review it again before applying".into(),
        );
    }

    let summary = summarise(kind, &body);
    promote(live, kind, &body)?;
    std::fs::remove_file(&path).ok();
    // The agent's only way to learn that its proposal landed.
    let _ = write_receipt_at(pending, kind, true, digest, &summary, None);
    Ok(())
}

pub fn reject_pending(kind: ConfigKind) -> Result<(), String> {
    reject_pending_at(&pending_dir(), kind)
}

pub fn reject_pending_at(pending: &Path, kind: ConfigKind) -> Result<(), String> {
    let path = pending.join(kind.file_name());
    // Digest the body before deleting it, so the receipt names what was
    // discarded rather than a file that no longer exists.
    let digest = std::fs::read_to_string(&path)
        .map(|raw| digest_of(&raw))
        .unwrap_or_default();
    if path.exists() {
        std::fs::remove_file(&path)
            .map_err(|e| format!("could not remove {}: {e}", path.display()))?;
    }
    let _ = write_receipt_at(
        pending,
        kind,
        false,
        &digest,
        "",
        Some("discarded by the user"),
    );
    Ok(())
}

/* ------------------------------------------------------------------ *
 * Plugin sources
 * ------------------------------------------------------------------ */

pub fn discover_plugin_sources(cwd: Option<&Path>) -> Vec<PluginSource> {
    discover_plugin_sources_at(&crate::config::plugins_dirs(cwd), customize::safe_mode())
}

/// Look for `<plugin>/vibedesk/{theme,layout,actions,panels}.json`.
///
/// Nothing here writes. Discovery and application are separate on purpose: a
/// plugin that could restyle the app by being installed would be a supply-chain
/// hole, and the user would have no way to tell which plugin did it.
pub fn discover_plugin_sources_at(dirs: &[PathBuf], safe: bool) -> Vec<PluginSource> {
    let mut out = Vec::new();
    if safe {
        return out;
    }

    for dir in dirs {
        let scope = if dir.starts_with(crate::paths::vibe_home()) {
            "user"
        } else {
            "project"
        };
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let plugin_dir = entry.path();
            if !plugin_dir.is_dir() {
                continue;
            }
            let Some(plugin) = plugin_dir.file_name().and_then(|n| n.to_str()) else {
                continue;
            };

            let custom = plugin_dir.join("vibedesk");
            // `symlink_metadata` so a plugin cannot point `vibedesk/` at a
            // directory it does not own.
            let Ok(meta) = std::fs::symlink_metadata(&custom) else {
                continue;
            };
            if !meta.is_dir() {
                continue;
            }

            for kind in ConfigKind::ALL {
                let path = custom.join(kind.file_name());
                if !path.exists() {
                    continue;
                }
                let (summary, digest, valid, error) = match read_limited(&path, kind)
                    .and_then(|raw| normalise(kind, &raw))
                {
                    Ok(body) => (summarise(kind, &body), digest_of(&body), true, None),
                    Err(error) => (String::new(), String::new(), false, Some(error)),
                };
                out.push(PluginSource {
                    plugin: plugin.to_string(),
                    scope: scope.to_string(),
                    kind,
                    path: path.display().to_string(),
                    summary,
                    digest,
                    valid,
                    error,
                });
            }
        }
    }

    out.sort_by(|a, b| {
        a.plugin
            .cmp(&b.plugin)
            .then_with(|| a.kind.label().cmp(b.kind.label()))
    });
    out
}

pub fn apply_plugin_source(
    cwd: Option<&Path>,
    plugin: &str,
    kind: ConfigKind,
    digest: &str,
) -> Result<(), String> {
    apply_plugin_source_at(
        &crate::config::plugins_dirs(cwd),
        &customize::config_dir(),
        plugin,
        kind,
        digest,
        customize::safe_mode(),
    )
}

/// Apply a plugin's config.
///
/// The caller names a plugin and a kind, never a path. Re-discovering here means
/// the frontend cannot hand in an arbitrary file to be read and promoted, and
/// the digest check means the file cannot change between the listing and the
/// click.
pub fn apply_plugin_source_at(
    dirs: &[PathBuf],
    live: &Path,
    plugin: &str,
    kind: ConfigKind,
    digest: &str,
    safe: bool,
) -> Result<(), String> {
    if safe {
        return Err("safe mode is on; customization is disabled".into());
    }

    let found = discover_plugin_sources_at(dirs, safe)
        .into_iter()
        .find(|source| source.plugin == plugin && source.kind == kind)
        .ok_or_else(|| format!("plugin `{plugin}` ships no {}", kind.file_name()))?;

    if !found.valid {
        return Err(found
            .error
            .unwrap_or_else(|| format!("plugin `{plugin}`'s {} is invalid", kind.file_name())));
    }
    if found.digest != digest {
        return Err(
            "the plugin's file changed after it was shown; review it again before applying".into(),
        );
    }

    let raw = read_limited(Path::new(&found.path), kind)?;
    let body = normalise(kind, &raw)?;
    if digest_of(&body) != digest {
        return Err(
            "the plugin's file changed after it was shown; review it again before applying".into(),
        );
    }

    promote(live, kind, &body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "vibedesk-sources-{}-{name}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_theme_diff_names_the_tokens_that_move() {
        let current = r##"{"tokens":{"--vd-accent":"#ff7a1a","--vd-canvas":"#12100e"}}"##;
        let proposed =
            r##"{"tokens":{"--vd-accent":"#ff7414","--vd-canvas":"#12100e","--vd-line":"#2a2622"}}"##;

        let changes = diff(ConfigKind::Theme, proposed, Some(current));
        assert_eq!(
            changes,
            vec![
                Change {
                    field: "--vd-accent".into(),
                    from: "#ff7a1a".into(),
                    to: "#ff7414".into(),
                },
                Change {
                    field: "--vd-line".into(),
                    from: String::new(),
                    to: "#2a2622".into(),
                },
            ]
        );
    }

    #[test]
    fn a_theme_diff_reports_a_removed_token() {
        let current = r##"{"tokens":{"--vd-accent":"#ff7a1a","--vd-glow":"rgba(0,0,0,.1)"}}"##;
        let proposed = r##"{"tokens":{"--vd-accent":"#ff7a1a"}}"##;
        let changes = diff(ConfigKind::Theme, proposed, Some(current));
        assert_eq!(
            changes,
            vec![Change {
                field: "--vd-glow".into(),
                from: "rgba(0,0,0,.1)".into(),
                to: String::new(),
            }]
        );
    }

    #[test]
    fn a_layout_diff_names_the_fields_that_move() {
        let current = r#"{"railWidth":48,"sidebarWidth":252,"density":"comfortable"}"#;
        let proposed = r#"{"railWidth":44,"sidebarWidth":252,"density":"roomy"}"#;
        let changes = diff(ConfigKind::Layout, proposed, Some(current));
        assert_eq!(
            changes,
            vec![
                Change {
                    field: "railWidth".into(),
                    from: "48".into(),
                    to: "44".into(),
                },
                Change {
                    field: "density".into(),
                    from: "comfortable".into(),
                    to: "roomy".into(),
                },
            ]
        );
    }

    #[test]
    fn an_actions_diff_names_the_buttons_that_appear_and_vanish() {
        let current = r#"{"actions":[{"id":"review","label":"Review diff","run":{"kind":"prompt","text":"x"}},{"id":"gone","label":"Gone","run":{"kind":"prompt","text":"x"}}]}"#;
        let proposed = r#"{"actions":[{"id":"review","label":"Review the diff","run":{"kind":"prompt","text":"x"}},{"id":"fresh","label":"Fresh","run":{"kind":"prompt","text":"x"}}]}"#;
        let changes = diff(ConfigKind::Actions, proposed, Some(current));
        assert_eq!(
            changes,
            vec![
                Change {
                    field: "review".into(),
                    from: "Review diff".into(),
                    to: "Review the diff".into(),
                },
                Change {
                    field: "fresh".into(),
                    from: String::new(),
                    to: "Fresh".into(),
                },
                Change {
                    field: "gone".into(),
                    from: "Gone".into(),
                    to: String::new(),
                },
            ]
        );
    }

    /// A missing live file means the app is running the defaults, so the diff is
    /// against the defaults rather than empty. Otherwise the first proposal on a
    /// fresh install would show no changes at all.
    #[test]
    fn a_diff_against_no_current_file_uses_the_defaults() {
        let changes = diff(
            ConfigKind::Layout,
            r#"{"railWidth":44}"#,
            None,
        );
        assert_eq!(
            changes,
            vec![Change {
                field: "railWidth".into(),
                from: "48".into(),
                to: "44".into(),
            }]
        );
    }

    /// The baseline is what the app wears when the file is absent. Without it a
    /// first layout proposal on a fresh install would render every field as an
    /// addition, hiding the thing the user is deciding about.
    #[test]
    fn the_baseline_is_the_built_in_config() {
        // Theme has no baseline on purpose: a theme merges over the stylesheet,
        // so an absent file overrides nothing and every token is an addition.
        assert!(baseline(ConfigKind::Theme).is_none());
        let changes = diff(
            ConfigKind::Theme,
            r##"{"tokens":{"--vd-accent":"#ff7414"}}"##,
            baseline(ConfigKind::Theme).as_deref(),
        );
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].field, "--vd-accent");
        assert!(changes[0].from.is_empty());

        let layout = baseline(ConfigKind::Layout).expect("layout baseline");
        let parsed: LayoutConfig = serde_json::from_str(&layout).expect("parses");
        assert_eq!(parsed.rail_width, LayoutConfig::default().rail_width);

        // A layout proposal against the baseline must show a real "from".
        let changes = diff(ConfigKind::Layout, r#"{"railWidth":44}"#, Some(&layout));
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].from, "48");
        assert_eq!(changes[0].to, "44");
    }

    #[test]
    fn a_diff_is_capped_and_never_panics_on_garbage() {
        let tokens: Vec<String> = (0..40)
            .map(|i| format!(r##""--vd-t{i}":"#{i:06x}""##))
            .collect();
        let proposed = format!(r#"{{"tokens":{{{}}}}}"#, tokens.join(","));
        let changes = diff(ConfigKind::Theme, &proposed, None);
        assert_eq!(changes.len(), MAX_CHANGES);

        assert!(diff(ConfigKind::Theme, "not json", None).is_empty());
        assert!(diff(ConfigKind::Layout, "{}", Some("not json")).is_empty());
    }

    #[test]
    fn a_pending_theme_is_offered_but_not_applied() {
        let root = scratch("pending-offer");
        let pending = root.join("pending");
        let live = root.join("live");
        std::fs::create_dir_all(&pending).unwrap();
        std::fs::create_dir_all(&live).unwrap();

        std::fs::write(
            pending.join("theme.json"),
            r##"{"tokens":{"--vd-accent":"#ff6b35"}}"##,
        )
        .unwrap();

        let state = read_pending_at(&pending, false);
        assert_eq!(state.proposals.len(), 1);
        assert_eq!(state.proposals[0].kind, ConfigKind::Theme);
        assert!(state.proposals[0].summary.contains("1 token"));

        // Discovery must not have written anything.
        assert!(!live.join("theme.json").exists());

        apply_pending_at(&pending, &live, ConfigKind::Theme, &state.proposals[0].digest, false).unwrap();
        assert!(live.join("theme.json").exists());
        assert!(!pending.join("theme.json").exists());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_proposal_that_changed_after_it_was_shown_is_refused() {
        let root = scratch("toctou");
        let pending = root.join("pending");
        let live = root.join("live");
        std::fs::create_dir_all(&pending).unwrap();
        std::fs::create_dir_all(&live).unwrap();

        let path = pending.join("theme.json");
        std::fs::write(&path, r##"{"tokens":{"--vd-accent":"#ff6b35"}}"##).unwrap();
        let digest = read_pending_at(&pending, false).proposals[0].digest.clone();

        // The agent rewrites the file between the prompt and the click.
        std::fs::write(&path, r##"{"tokens":{"--vd-accent":"#000000"}}"##).unwrap();

        let error = apply_pending_at(&pending, &live, ConfigKind::Theme, &digest, false).unwrap_err();
        assert!(error.contains("changed after it was shown"), "{error}");
        assert!(!live.join("theme.json").exists());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_protected_surface_is_never_offered() {
        let root = scratch("protected");
        let pending = root.join("pending");
        std::fs::create_dir_all(&pending).unwrap();

        std::fs::write(
            pending.join("theme.json"),
            r#"{"rules":[{"selector":".vd-approval","declarations":{"opacity":"0"}}]}"#,
        )
        .unwrap();

        let state = read_pending_at(&pending, false);
        assert!(state.proposals.is_empty());
        assert_eq!(state.issues.len(), 1);
        assert!(state.issues[0].contains("protected"), "{:?}", state.issues);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_plugin_source_is_listed_and_only_applied_on_request() {
        let root = scratch("plugin");
        let plugins = root.join("plugins");
        let live = root.join("live");
        let custom = plugins.join("acme").join("vibedesk");
        std::fs::create_dir_all(&custom).unwrap();
        std::fs::create_dir_all(&live).unwrap();

        std::fs::write(
            custom.join("actions.json"),
            r#"{"actions":[{"id":"go","label":"Go","run":{"kind":"command","command":"new-chat"}}]}"#,
        )
        .unwrap();

        let sources = discover_plugin_sources_at(&[plugins.clone()], false);
        assert_eq!(sources.len(), 1);
        assert!(sources[0].valid);
        assert_eq!(sources[0].plugin, "acme");
        assert!(sources[0].summary.contains("1 button"));

        // Listing is not applying.
        assert!(!live.join("actions.json").exists());

        apply_plugin_source_at(
            &[plugins.clone()],
            &live,
            "acme",
            ConfigKind::Actions,
            &sources[0].digest,
            false,
        )
        .unwrap();
        assert!(live.join("actions.json").exists());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn an_invalid_plugin_source_is_reported_and_refused() {
        let root = scratch("plugin-invalid");
        let plugins = root.join("plugins");
        let live = root.join("live");
        let custom = plugins.join("bad").join("vibedesk");
        std::fs::create_dir_all(&custom).unwrap();
        std::fs::create_dir_all(&live).unwrap();

        std::fs::write(
            custom.join("theme.json"),
            r##"{"tokens":{"--vd-accent":"url(http://evil)"}}"##,
        )
        .unwrap();

        let sources = discover_plugin_sources_at(&[plugins.clone()], false);
        assert_eq!(sources.len(), 1);
        assert!(!sources[0].valid);
        assert!(sources[0].error.as_deref().unwrap_or("").contains("url("));

        let error =
            apply_plugin_source_at(&[plugins.clone()], &live, "bad", ConfigKind::Theme, "", false)
                .unwrap_err();
        assert!(error.contains("url("), "{error}");
        assert!(!live.join("theme.json").exists());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_directory_named_like_a_config_is_not_read() {
        let root = scratch("dir-as-file");
        let pending = root.join("pending");
        std::fs::create_dir_all(pending.join("theme.json")).unwrap();

        let state = read_pending_at(&pending, false);
        assert!(state.proposals.is_empty());
        assert_eq!(state.issues.len(), 1);
        assert!(state.issues[0].contains("not a regular file"));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_stray_name_is_named_rather_than_ignored() {
        let root = scratch("stray");
        let pending = root.join("pending");
        std::fs::create_dir_all(&pending).unwrap();
        std::fs::write(pending.join("mytheme.json"), "{}").unwrap();

        let state = read_pending_at(&pending, false);
        assert!(state.proposals.is_empty());
        assert!(state.issues.iter().any(|i| i.contains("mytheme.json")));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn what_lands_is_the_normalised_body_not_the_raw_bytes() {
        let root = scratch("normalise");
        let pending = root.join("pending");
        let live = root.join("live");
        std::fs::create_dir_all(&pending).unwrap();
        std::fs::create_dir_all(&live).unwrap();

        // `expanded` is not a field of a file block, and `sneaky` is not a field
        // of a panel. Both must be gone from what reaches disk.
        std::fs::write(
            pending.join("panels.json"),
            r#"{"panels":[{"id":"p","title":"P","sneaky":true,"blocks":[{"kind":"file","path":"a.md","expanded":true}]}]}"#,
        )
        .unwrap();

        let digest = read_pending_at(&pending, false).proposals[0].digest.clone();
        apply_pending_at(&pending, &live, ConfigKind::Panels, &digest, false).unwrap();

        let written = std::fs::read_to_string(live.join("panels.json")).unwrap();
        assert!(!written.contains("sneaky"), "{written}");
        assert!(!written.contains("expanded"), "{written}");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn an_oversized_proposal_is_refused() {
        let root = scratch("oversized");
        let pending = root.join("pending");
        std::fs::create_dir_all(&pending).unwrap();

        let big = "x".repeat(layout::MAX_LAYOUT_BYTES as usize + 1);
        std::fs::write(pending.join("layout.json"), &big).unwrap();

        let state = read_pending_at(&pending, false);
        assert!(state.proposals.is_empty());
        assert!(state.issues[0].contains("limit"));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn an_empty_proposal_is_refused() {
        let root = scratch("empty");
        let pending = root.join("pending");
        std::fs::create_dir_all(&pending).unwrap();
        std::fs::write(pending.join("theme.json"), "   \n").unwrap();

        let state = read_pending_at(&pending, false);
        assert!(state.proposals.is_empty());
        assert!(state.issues[0].contains("empty"));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn rejecting_removes_the_proposal_without_touching_the_live_config() {
        let root = scratch("reject");
        let pending = root.join("pending");
        let live = root.join("live");
        std::fs::create_dir_all(&pending).unwrap();
        std::fs::create_dir_all(&live).unwrap();

        std::fs::write(pending.join("theme.json"), r##"{"tokens":{"--vd-accent":"#fff"}}"##).unwrap();
        std::fs::write(live.join("theme.json"), r##"{"tokens":{"--vd-accent":"#111"}}"##).unwrap();

        reject_pending_at(&pending, ConfigKind::Theme).unwrap();
        assert!(!pending.join("theme.json").exists());

        let live_body = std::fs::read_to_string(live.join("theme.json")).unwrap();
        assert!(live_body.contains("#111"), "{live_body}");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_plugin_cannot_name_a_path_outside_its_own_folder() {
        let root = scratch("plugin-path");
        let plugins = root.join("plugins");
        let live = root.join("live");
        std::fs::create_dir_all(plugins.join("acme")).unwrap();
        std::fs::create_dir_all(&live).unwrap();

        // No `vibedesk/` folder at all, so nothing is discovered and the apply
        // has nothing to find — the caller cannot substitute a path.
        let sources = discover_plugin_sources_at(&[plugins.clone()], false);
        assert!(sources.is_empty());

        let error =
            apply_plugin_source_at(&[plugins.clone()], &live, "acme", ConfigKind::Theme, "abc", false)
                .unwrap_err();
        assert!(error.contains("ships no"), "{error}");

        std::fs::remove_dir_all(&root).ok();
    }

    /* -------------------------------------------------------------- *
     * Receipts
     * -------------------------------------------------------------- */

    fn read_receipt(dir: &Path, kind: ConfigKind) -> Receipt {
        let raw = std::fs::read_to_string(dir.join(kind.receipt_name())).unwrap();
        serde_json::from_str(&raw).unwrap()
    }

    #[test]
    fn applying_writes_a_receipt_the_agent_can_read() {
        let root = scratch("receipt-apply");
        let pending = root.join("pending");
        let live = root.join("live");
        std::fs::create_dir_all(&pending).unwrap();
        std::fs::create_dir_all(&live).unwrap();

        std::fs::write(
            pending.join("theme.json"),
            r##"{"tokens":{"--vd-accent":"#ff6b35"}}"##,
        )
        .unwrap();

        let state = read_pending_at(&pending, false);
        let digest = state.proposals[0].digest.clone();
        apply_pending_at(&pending, &live, ConfigKind::Theme, &digest, false).unwrap();

        let receipt = read_receipt(&pending, ConfigKind::Theme);
        assert!(receipt.applied);
        assert_eq!(receipt.kind, ConfigKind::Theme);
        assert_eq!(receipt.digest, digest);
        assert!(receipt.summary.contains("1 token"), "{}", receipt.summary);
        assert!(receipt.error.is_none());
        assert!(!receipt.at.is_empty());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn rejecting_writes_a_receipt_naming_the_discard() {
        let root = scratch("receipt-reject");
        let pending = root.join("pending");
        std::fs::create_dir_all(&pending).unwrap();

        std::fs::write(
            pending.join("layout.json"),
            r#"{"sidebarWidth":380}"#,
        )
        .unwrap();

        reject_pending_at(&pending, ConfigKind::Layout).unwrap();

        let receipt = read_receipt(&pending, ConfigKind::Layout);
        assert!(!receipt.applied);
        assert_eq!(receipt.error.as_deref(), Some("discarded by the user"));
        // The digest names what was discarded, not an empty file.
        assert!(!receipt.digest.is_empty());

        std::fs::remove_dir_all(&root).ok();
    }

    /// The case that used to be invisible: the agent wrote something invalid, it
    /// never became a proposal, and the agent was never told why.
    #[test]
    fn an_invalid_proposal_writes_a_receipt_with_the_validator_message() {
        let root = scratch("receipt-invalid");
        let pending = root.join("pending");
        std::fs::create_dir_all(&pending).unwrap();

        std::fs::write(
            pending.join("theme.json"),
            r##"{"tokens":{"--vd-accent":"url(http://x)"}}"##,
        )
        .unwrap();

        let state = read_pending_at(&pending, false);
        assert!(state.proposals.is_empty());
        assert_eq!(state.issues.len(), 1);

        let receipt = read_receipt(&pending, ConfigKind::Theme);
        assert!(!receipt.applied);
        let error = receipt.error.unwrap();
        assert!(error.contains("url("), "{error}");

        std::fs::remove_dir_all(&root).ok();
    }

    /// The pending directory is polled every 400 ms and an invalid file stays
    /// invalid, so the receipt must not be rewritten on every tick.
    #[test]
    fn a_repeated_failure_does_not_rewrite_the_receipt() {
        let root = scratch("receipt-idempotent");
        let pending = root.join("pending");
        std::fs::create_dir_all(&pending).unwrap();

        std::fs::write(
            pending.join("theme.json"),
            r##"{"tokens":{"--vd-accent":"javascript:alert(1)"}}"##,
        )
        .unwrap();

        read_pending_at(&pending, false);
        let first = read_receipt(&pending, ConfigKind::Theme);

        // Three more polls.
        for _ in 0..3 {
            read_pending_at(&pending, false);
        }
        let second = read_receipt(&pending, ConfigKind::Theme);

        assert_eq!(first.at, second.at, "the timestamp moved on a repeat poll");
        assert_eq!(first.digest, second.digest);

        std::fs::remove_dir_all(&root).ok();
    }

    /// Receipts live in the pending directory, so the stray-name check has to
    /// know about them or the approval bar shows four permanent issues.
    #[test]
    fn a_receipt_is_not_reported_as_a_stray() {
        let root = scratch("receipt-not-stray");
        let pending = root.join("pending");
        std::fs::create_dir_all(&pending).unwrap();

        write_receipt_at(&pending, ConfigKind::Theme, true, "abc", "1 token", None).unwrap();

        let state = read_pending_at(&pending, false);
        assert!(state.issues.is_empty(), "{:?}", state.issues);
        assert_eq!(state.receipts.len(), 1);
        assert!(state.receipts[0].applied);

        std::fs::remove_dir_all(&root).ok();
    }

    /// A receipt for a kind with no pending file is still surfaced, so the UI can
    /// show the last outcome after the proposal is gone.
    #[test]
    fn a_receipt_survives_the_proposal_it_describes() {
        let root = scratch("receipt-survives");
        let pending = root.join("pending");
        let live = root.join("live");
        std::fs::create_dir_all(&pending).unwrap();
        std::fs::create_dir_all(&live).unwrap();

        std::fs::write(pending.join("panels.json"), r#"{"panels":[]}"#).unwrap();
        let state = read_pending_at(&pending, false);
        let digest = state.proposals[0].digest.clone();
        apply_pending_at(&pending, &live, ConfigKind::Panels, &digest, false).unwrap();

        let after = read_pending_at(&pending, false);
        assert!(after.proposals.is_empty());
        assert_eq!(after.receipts.len(), 1);
        assert!(after.receipts[0].applied);

        std::fs::remove_dir_all(&root).ok();
    }
}
