//! The card: what the agent receives instead of a 200 KB dump.
//!
//! Ported from `sparseread/core/preview.py`, with one deliberate change of
//! shape. Upstream returns a JSON `PreviewPack` because its consumer is an MCP
//! tool that can carry structure. VibeDesk's consumer is `fs/read_text_file`,
//! whose reply is a single `content` string, so the card is rendered as text —
//! but every field upstream computes is still computed, and the anchors are
//! line ranges so the agent can page straight to them.
//!
//! The rule this file follows: a preview that does not tell the agent how to
//! get the rest of the file is a wall, not a preview. Every card ends with
//! concrete `line`/`limit` calls.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;

use super::detect::FileInfo;
use super::gate::Decision;
use super::ReadTool;
use super::units::{self, Section};

/// How many lines a single suggested page covers.
const PAGE_LINES: usize = 200;
/// How many samples a card carries before it stops being a preview.
const MAX_SAMPLES: usize = 6;
const MAX_HEADINGS: usize = 24;

/// How many section calls the card offers before falling back to the whole file.
///
/// A card with forty roads is a card the agent skims. Eight is enough to cover
/// the sections of a real document and few enough that each one is read.
const MAX_SECTION_ACTIONS: usize = 8;

/// A section shorter than this is not worth its own call: the call costs more
/// tokens than the lines it fetches.
const MIN_SECTION_LINES: usize = 3;
const MAX_SIGNALS: usize = 8;
const CLIP_CHARS: usize = 200;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Sample {
    pub anchor: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Signal {
    pub kind: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "shape", rename_all = "camelCase")]
pub enum Structure {
    Text {
        unit_count: usize,
        headings: Vec<String>,
        /// Each heading with the lines it owns, so the card can offer a call per
        /// section rather than only "read the whole file".
        sections: Vec<Section>,
    },
    Log {
        line_count: usize,
        levels: BTreeMap<String, usize>,
        repeated: Vec<String>,
    },
    Tabular {
        columns: Vec<String>,
        column_types: BTreeMap<String, String>,
        row_count: usize,
    },
    Mapping {
        container: String,
        keys: Vec<String>,
        length: Option<usize>,
    },
    Markup {
        root: String,
        children: BTreeMap<String, usize>,
        attributes: Vec<String>,
    },
    Collection {
        file_count: usize,
        kind_counts: BTreeMap<String, usize>,
        total_bytes: u64,
    },
    /// A shape this port cannot preview. Reported, never faked.
    Unsupported {
        reason: String,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Card {
    pub path: String,
    pub kind: String,
    pub size_bytes: u64,
    pub line_count: usize,
    pub structure: Structure,
    pub samples: Vec<Sample>,
    pub signals: Vec<Signal>,
    /// The gate's verdict, so the card can explain why it exists.
    pub code: String,
    pub reason: String,
    pub action: String,
    pub confidence: f32,
    /// The read tool the next actions name, so they are calls the agent can
    /// actually make rather than a dead end dressed up as a next step.
    pub tool: ReadTool,
}

impl Card {
    /// The exact calls that get the agent the rest of the file.
    pub fn next_actions(&self) -> Vec<String> {
        let total = self.line_count.max(1);
        let tool = &self.tool;
        let call = |start: usize, count: usize| {
            format!(
                "{} {{ path, {}: {}, {}: {count} }}",
                tool.name,
                tool.offset,
                tool.offset_for(start),
                tool.limit
            )
        };
        let mut out = Vec::new();

        // A document with headings gets a call per section. This is the whole
        // point of the card: the agent asked a question about one part of a long
        // file, and "read all 168 lines" is not an answer to that — it is the
        // cost the gate exists to avoid. Offering only the full file is what
        // drove the agent to `sed -n` through the shell instead.
        if let Structure::Text { sections, .. } = &self.structure {
            let useful: Vec<&Section> = sections
                .iter()
                .filter(|section| section.lines() >= MIN_SECTION_LINES)
                .collect();
            if useful.len() >= 2 {
                for section in useful.iter().take(MAX_SECTION_ACTIONS) {
                    out.push(format!(
                        "L{}-L{} {}: {}",
                        section.start_line,
                        section.end_line,
                        section.title,
                        call(section.start_line, section.lines())
                    ));
                }
                if useful.len() > MAX_SECTION_ACTIONS {
                    out.push(format!(
                        "whole file ({} lines): {}",
                        total,
                        call(1, total)
                    ));
                }
                return out;
            }
        }

        if total <= PAGE_LINES {
            out.push(format!("full content: {}", call(1, total)));
            return out;
        }
        let mut start = 1;
        while start <= total && out.len() < 4 {
            let end = std::cmp::min(start + PAGE_LINES - 1, total);
            out.push(format!("lines {start}-{end}: {}", call(start, end - start + 1)));
            start = end + 1;
        }
        if start <= total {
            out.push(format!(
                "remaining lines {start}-{total}: {}",
                call(start, total - start + 1)
            ));
        }
        out
    }

    /// Render the card as the text the agent will actually read.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "[sparse-read] {} — {}, {} lines, {}\n",
            self.path,
            human_bytes(self.size_bytes),
            self.line_count,
            self.kind
        ));
        out.push_str(&format!(
            "gate: {} ({}, confidence {:.2}) — {}\n",
            self.action, self.code, self.confidence, self.reason
        ));

        match &self.structure {
            Structure::Text {
                unit_count,
                headings,
                sections,
            } => {
                out.push_str(&format!(
                    "structure: {unit_count} text units, {} headings\n",
                    headings.len()
                ));
                if !sections.is_empty() {
                    // Line ranges, not just titles. A title the agent cannot
                    // address is a label, and a label is not a map.
                    out.push_str("sections:\n");
                    for section in sections.iter().take(MAX_HEADINGS) {
                        out.push_str(&format!(
                            "  L{}-L{}  {}\n",
                            section.start_line, section.end_line, section.title
                        ));
                    }
                    if sections.len() > MAX_HEADINGS {
                        out.push_str(&format!(
                            "  ... {} more\n",
                            sections.len() - MAX_HEADINGS
                        ));
                    }
                } else if !headings.is_empty() {
                    out.push_str("headings:\n");
                    for heading in headings.iter().take(MAX_HEADINGS) {
                        out.push_str(&format!("  {heading}\n"));
                    }
                }
            }
            Structure::Log {
                line_count,
                levels,
                repeated,
            } => {
                out.push_str(&format!("structure: log, {line_count} lines\n"));
                if !levels.is_empty() {
                    let rendered: Vec<String> = levels
                        .iter()
                        .map(|(level, count)| format!("{level}={count}"))
                        .collect();
                    out.push_str(&format!("levels: {}\n", rendered.join(" ")));
                }
                if !repeated.is_empty() {
                    out.push_str("repeated:\n");
                    for line in repeated.iter().take(MAX_SIGNALS) {
                        out.push_str(&format!("  {line}\n"));
                    }
                }
            }
            Structure::Tabular {
                columns,
                column_types,
                row_count,
            } => {
                out.push_str(&format!(
                    "structure: table, {row_count} rows x {} columns\n",
                    columns.len()
                ));
                out.push_str("columns:\n");
                for column in columns.iter().take(MAX_HEADINGS) {
                    let kind = column_types
                        .get(column)
                        .map(String::as_str)
                        .unwrap_or("string");
                    out.push_str(&format!("  {column}: {kind}\n"));
                }
                if columns.len() > MAX_HEADINGS {
                    out.push_str(&format!("  ... {} more\n", columns.len() - MAX_HEADINGS));
                }
            }
            Structure::Mapping {
                container,
                keys,
                length,
            } => {
                out.push_str(&format!("structure: {container}"));
                if let Some(length) = length {
                    out.push_str(&format!(", {length} elements"));
                }
                out.push('\n');
                if !keys.is_empty() {
                    out.push_str(&format!("keys: {}\n", keys.join(", ")));
                }
            }
            Structure::Markup {
                root,
                children,
                attributes,
            } => {
                out.push_str(&format!("structure: xml root <{root}>\n"));
                if !attributes.is_empty() {
                    out.push_str(&format!("attributes: {}\n", attributes.join(", ")));
                }
                if !children.is_empty() {
                    let rendered: Vec<String> = children
                        .iter()
                        .take(MAX_HEADINGS)
                        .map(|(tag, count)| format!("{tag}={count}"))
                        .collect();
                    out.push_str(&format!("children: {}\n", rendered.join(" ")));
                }
            }
            Structure::Collection {
                file_count,
                kind_counts,
                total_bytes,
            } => {
                out.push_str(&format!(
                    "structure: collection, {file_count} files, {}\n",
                    human_bytes(*total_bytes)
                ));
                let rendered: Vec<String> = kind_counts
                    .iter()
                    .map(|(kind, count)| format!("{kind}={count}"))
                    .collect();
                out.push_str(&format!("kinds: {}\n", rendered.join(" ")));
            }
            Structure::Unsupported { reason } => {
                out.push_str(&format!("structure: not previewed — {reason}\n"));
            }
        }

        if !self.samples.is_empty() {
            out.push_str("samples:\n");
            for sample in &self.samples {
                out.push_str(&format!("  {}  {}\n", sample.anchor, sample.text));
            }
        }
        if !self.signals.is_empty() {
            out.push_str("signals:\n");
            for signal in &self.signals {
                out.push_str(&format!("  [{}] {}\n", signal.kind, signal.detail));
            }
        }

        out.push_str("next:\n");
        for action in self.next_actions() {
            out.push_str(&format!("  {action}\n"));
        }
        out.push_str(
            "note: this is a preview, not the file. Read only the section you need, with the \
             calls above — they use the read tool's own offset/limit. Do not re-read the whole \
             file, and do not dump it through the shell: a `cat` of this file is carded too.\n",
        );
        out
    }
}

pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn clip(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= CLIP_CHARS {
        return trimmed.to_string();
    }
    let head: String = trimmed.chars().take(CLIP_CHARS - 15).collect();
    format!("{}...[clipped]", head.trim_end())
}

fn count_lines(text: &str) -> usize {
    if text.is_empty() {
        0
    } else {
        text.lines().count()
    }
}

/// Build the card for `info`, given the gate's verdict.
///
/// `content` is the already-read file body. Reading is the caller's job so this
/// function stays pure and testable.
pub fn build(info: &FileInfo, decision: &Decision, content: &str, tool: &ReadTool) -> Card {
    let mut card = Card {
        path: info.path.to_string_lossy().into_owned(),
        kind: info.kind.clone(),
        size_bytes: info.size_bytes,
        line_count: count_lines(content),
        structure: Structure::Unsupported {
            reason: "no preview recipe for this shape".into(),
        },
        samples: Vec::new(),
        signals: Vec::new(),
        code: decision.code.to_string(),
        reason: decision.reason.to_string(),
        action: decision.action().to_string(),
        confidence: decision.confidence,
        tool: tool.clone(),
    };

    if info.is_collection() {
        build_collection(&mut card, info);
        return card;
    }

    // Dispatch on the suffix, not the detected kind: `.log` and `.md` are both
    // kind `text`, but only one of them wants level counts.
    match info.extension().as_str() {
        ".csv" | ".tsv" => build_tabular(&mut card, &info.path, content),
        ".json" => build_json(&mut card, content),
        ".xml" => build_xml(&mut card, content),
        ".log" => build_log(&mut card, content),
        ".pdf" | ".xlsx" => {
            card.structure = Structure::Unsupported {
                reason: format!(
                    "the {} reader is not ported; page with line/limit or use a native tool",
                    info.kind
                ),
            };
        }
        _ => build_text(&mut card, content),
    }
    card
}

fn build_text(card: &mut Card, content: &str) {
    let (units, headings) = units::segment(content);
    card.samples = units
        .iter()
        .take(MAX_SAMPLES)
        .map(|unit| Sample {
            anchor: unit.anchor.clone(),
            text: clip(&unit.text),
        })
        .collect();
    if !headings.is_empty() {
        card.signals.push(Signal {
            kind: "headings".into(),
            detail: format!("{} headings; see structure", headings.len()),
        });
    }
    card.structure = Structure::Text {
        unit_count: units.len(),
        sections: units::sections(&units),
        headings,
    };
}

fn build_log(card: &mut Card, content: &str) {
    let mut levels: BTreeMap<String, usize> = BTreeMap::new();
    let mut signatures: BTreeMap<String, usize> = BTreeMap::new();
    let mut notable: Vec<Sample> = Vec::new();

    for (index, line) in content.lines().take(5000).enumerate() {
        if let Some(level) = log_severity(line) {
            *levels.entry(level.clone()).or_insert(0) += 1;
            if matches!(level.as_str(), "ERROR" | "WARN" | "FATAL" | "CRITICAL")
                && notable.len() < MAX_SAMPLES
            {
                notable.push(Sample {
                    anchor: format!("L{}", index + 1),
                    text: clip(line),
                });
            }
        }
        let signature = log_signature(line);
        if !signature.is_empty() {
            *signatures.entry(signature).or_insert(0) += 1;
        }
    }

    let mut repeated: Vec<(String, usize)> = signatures
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .collect();
    repeated.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    repeated.truncate(MAX_SIGNALS);

    if notable.is_empty() {
        notable = content
            .lines()
            .take(MAX_SAMPLES)
            .enumerate()
            .map(|(index, line)| Sample {
                anchor: format!("L{}", index + 1),
                text: clip(line),
            })
            .collect();
    }
    card.samples = notable;
    card.signals = repeated
        .iter()
        .map(|(signature, count)| Signal {
            kind: "repeated_lines".into(),
            detail: format!("x{count} {signature}"),
        })
        .collect();
    card.structure = Structure::Log {
        line_count: count_lines(content),
        levels,
        repeated: repeated
            .iter()
            .map(|(signature, count)| format!("x{count} {signature}"))
            .collect(),
    };
}

fn log_severity(line: &str) -> Option<String> {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r"\b(TRACE|DEBUG|INFO|WARN|WARNING|ERROR|CRITICAL|FATAL)\b").unwrap()
    });
    let found = re.find(line)?;
    let level = found.as_str().to_uppercase();
    Some(if level == "WARNING" {
        "WARN".to_string()
    } else {
        level
    })
}

fn log_signature(line: &str) -> String {
    static TIMESTAMP: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static BRACKET: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static HEX: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static NUMBER: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static SPACE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();

    let timestamp = TIMESTAMP.get_or_init(|| {
        regex::Regex::new(r"^\s*\d{4}[-/]\d{2}[-/]\d{2}[ T]\d{2}:\d{2}:\d{2}(?:[.,]\d+)?\s*")
            .unwrap()
    });
    let bracket = BRACKET.get_or_init(|| regex::Regex::new(r"^\s*\[[^\]]*\]\s*").unwrap());
    let hex = HEX.get_or_init(|| regex::Regex::new(r"\b0x[0-9a-fA-F]+\b").unwrap());
    let number = NUMBER.get_or_init(|| regex::Regex::new(r"\b\d{4,}\b").unwrap());
    let space = SPACE.get_or_init(|| regex::Regex::new(r"\s+").unwrap());

    let text = timestamp.replace(line, "");
    let text = bracket.replace(&text, "");
    let text = hex.replace_all(&text, "0x<hex>");
    let text = number.replace_all(&text, "<num>");
    clip(&space.replace_all(&text, " ").trim().to_string())
}

/// Split one CSV record, honouring double-quoted fields.
fn split_record(line: &str, delimiter: char) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if quoted {
            if ch == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    current.push('"');
                } else {
                    quoted = false;
                }
            } else {
                current.push(ch);
            }
            continue;
        }
        match ch {
            '"' => quoted = true,
            c if c == delimiter => {
                fields.push(std::mem::take(&mut current));
            }
            c => current.push(c),
        }
    }
    fields.push(current);
    fields
}

fn looks_int(value: &str) -> bool {
    let trimmed = value.trim();
    !trimmed.is_empty()
        && trimmed
            .strip_prefix(['-', '+'])
            .unwrap_or(trimmed)
            .chars()
            .all(|ch| ch.is_ascii_digit())
}

fn looks_float(value: &str) -> bool {
    let trimmed = value.trim();
    let body = trimmed.strip_prefix(['-', '+']).unwrap_or(trimmed);
    if body.is_empty() {
        return false;
    }
    let mut dots = 0;
    for ch in body.chars() {
        if ch == '.' {
            dots += 1;
            if dots > 1 {
                return false;
            }
        } else if !ch.is_ascii_digit() {
            return false;
        }
    }
    true
}

fn looks_date(value: &str) -> bool {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"\b\d{4}[-/]\d{1,2}[-/]\d{1,2}\b").unwrap())
        .is_match(value)
}

fn interesting_value(value: &str) -> bool {
    let low = value.to_lowercase();
    [
        "error",
        "warn",
        "fail",
        "exception",
        "timeout",
        "rate_limited",
        "critical",
        "panic",
    ]
    .iter()
    .any(|term| low.contains(term))
}

fn build_tabular(card: &mut Card, path: &Path, content: &str) {
    let delimiter = if path
        .extension()
        .map(|ext| ext.to_string_lossy().to_lowercase() == "tsv")
        .unwrap_or(false)
    {
        '\t'
    } else {
        ','
    };

    let mut lines = content.lines();
    let columns = lines
        .next()
        .map(|header| split_record(header, delimiter))
        .unwrap_or_default();
    let mut row_count = 0usize;
    let mut sampled: Vec<Vec<String>> = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        row_count += 1;
        if sampled.len() < 200 {
            sampled.push(split_record(line, delimiter));
        }
    }

    let mut column_types = BTreeMap::new();
    for (index, column) in columns.iter().enumerate() {
        let values: Vec<&str> = sampled
            .iter()
            .filter_map(|row| row.get(index))
            .map(String::as_str)
            .filter(|value| !value.trim().is_empty())
            .collect();
        let kind = if values.is_empty() {
            "empty"
        } else if values.iter().take(50).all(|value| looks_int(value)) {
            "integer"
        } else if values.iter().take(50).all(|value| looks_float(value)) {
            "number"
        } else if values.iter().take(50).all(|value| looks_date(value)) {
            "date"
        } else {
            "string"
        };
        column_types.insert(column.clone(), kind.to_string());
    }

    card.samples = sampled
        .iter()
        .take(3)
        .enumerate()
        .map(|(index, row)| Sample {
            anchor: format!("L{}", index + 2),
            text: clip(&row.join(" | ")),
        })
        .collect();

    for (index, column) in columns.iter().enumerate() {
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for row in &sampled {
            if let Some(value) = row.get(index) {
                if !value.trim().is_empty() {
                    *counts.entry(value.as_str()).or_insert(0) += 1;
                }
            }
        }
        let rare: Vec<&str> = counts
            .iter()
            .filter(|(value, count)| **count == 1 && interesting_value(value))
            .map(|(value, _)| *value)
            .take(8)
            .collect();
        if !rare.is_empty() {
            card.signals.push(Signal {
                kind: "rare_or_error_values".into(),
                detail: format!("{column}: {}", rare.join(", ")),
            });
        }
        if card.signals.len() >= MAX_SIGNALS {
            break;
        }
    }

    card.structure = Structure::Tabular {
        columns,
        column_types,
        row_count,
    };
}

fn build_json(card: &mut Card, content: &str) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(content) else {
        // Malformed JSON is still worth a text preview rather than an error.
        build_text(card, content);
        return;
    };

    let (container, keys, length) = match &value {
        serde_json::Value::Object(map) => (
            "object".to_string(),
            map.keys().take(24).cloned().collect::<Vec<_>>(),
            None,
        ),
        serde_json::Value::Array(items) => (
            "array".to_string(),
            items
                .first()
                .and_then(|first| first.as_object())
                .map(|map| map.keys().take(24).cloned().collect::<Vec<_>>())
                .unwrap_or_default(),
            Some(items.len()),
        ),
        other => (json_shape(other).to_string(), Vec::new(), None),
    };

    card.samples = match &value {
        serde_json::Value::Array(items) => items
            .iter()
            .take(3)
            .enumerate()
            .map(|(index, item)| Sample {
                anchor: format!("$[{index}]"),
                text: clip(&serde_json::to_string(item).unwrap_or_default()),
            })
            .collect(),
        serde_json::Value::Object(map) => vec![Sample {
            anchor: "$".into(),
            text: clip(&serde_json::to_string(map).unwrap_or_default()),
        }],
        other => vec![Sample {
            anchor: "$".into(),
            text: clip(&serde_json::to_string(other).unwrap_or_default()),
        }],
    };

    let mut signals = Vec::new();
    walk_scalars(&value, "$", &mut signals);
    card.signals = signals;

    card.structure = Structure::Mapping {
        container,
        keys,
        length,
    };
}

fn json_shape(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

fn walk_scalars(value: &serde_json::Value, path: &str, out: &mut Vec<Signal>) {
    if out.len() >= MAX_SIGNALS {
        return;
    }
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                walk_scalars(child, &format!("{path}.{key}"), out);
            }
        }
        serde_json::Value::Array(items) => {
            for (index, child) in items.iter().take(200).enumerate() {
                walk_scalars(child, &format!("{path}[{index}]"), out);
            }
        }
        serde_json::Value::String(text) if interesting_value(text) => out.push(Signal {
            kind: "interesting_scalar".into(),
            detail: format!("{path} = {}", clip(text)),
        }),
        _ => {}
    }
}

fn build_xml(card: &mut Card, content: &str) {
    let mut root = String::new();
    let mut children: BTreeMap<String, usize> = BTreeMap::new();
    let mut attributes: Vec<String> = Vec::new();
    let mut samples: Vec<Sample> = Vec::new();
    let mut depth = 0usize;
    let mut rest = content;

    while let Some(open) = rest.find('<') {
        let Some(close) = rest[open..].find('>') else {
            break;
        };
        let body = &rest[open + 1..open + close];
        rest = &rest[open + close + 1..];
        if body.starts_with('?') || body.starts_with('!') {
            continue;
        }
        let closing = body.starts_with('/');
        let name: String = body
            .trim_start_matches('/')
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | ':'))
            .collect();
        if name.is_empty() {
            continue;
        }
        if closing {
            depth = depth.saturating_sub(1);
            continue;
        }
        if depth == 0 && root.is_empty() {
            root = name.clone();
            for token in body.split_whitespace().skip(1) {
                if let Some((key, _)) = token.split_once('=') {
                    attributes.push(key.to_string());
                }
            }
        } else if depth == 1 {
            *children.entry(name.clone()).or_insert(0) += 1;
        }
        if samples.len() < MAX_SAMPLES {
            let text = rest
                .split('<')
                .next()
                .map(|text| text.trim())
                .unwrap_or_default();
            if !text.is_empty() {
                samples.push(Sample {
                    anchor: format!("<{name}>"),
                    text: clip(text),
                });
            }
        }
        if !body.ends_with('/') {
            depth += 1;
        }
    }

    card.samples = samples;
    card.structure = Structure::Markup {
        root: if root.is_empty() {
            "unknown".into()
        } else {
            root
        },
        children,
        attributes,
    };
}

fn build_collection(card: &mut Card, info: &FileInfo) {
    let mut kind_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut sized: Vec<(u64, String)> = Vec::new();
    for member in &info.members {
        let kind = super::detect::file_type(member);
        *kind_counts.entry(kind).or_insert(0) += 1;
        let size = std::fs::metadata(member).map(|meta| meta.len()).unwrap_or(0);
        sized.push((
            size,
            member
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
        ));
    }
    sized.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));

    card.samples = sized
        .iter()
        .take(MAX_SAMPLES)
        .map(|(size, name)| Sample {
            anchor: human_bytes(*size),
            text: name.clone(),
        })
        .collect();
    card.line_count = info.members.len();
    card.structure = Structure::Collection {
        file_count: info.members.len(),
        kind_counts,
        total_bytes: info.size_bytes,
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::sparse::detect;
    use crate::capabilities::sparse::gate::{self, GateContext};
    use std::path::PathBuf;

    fn card_for(name: &str, content: &str) -> Card {
        let path = PathBuf::from(name);
        let info = FileInfo {
            path: path.clone(),
            kind: detect::file_type(&path),
            size_bytes: content.len() as u64,
            structured: matches!(
                detect::file_type(&path).as_str(),
                "csv" | "json" | "xml" | "yaml"
            ),
            supported: true,
            large: true,
            members: Vec::new(),
        };
        let decision = gate::decide(&info, &GateContext::default());
        build(&info, &decision, content, &ReadTool::default())
    }

    #[test]
    fn a_markdown_card_lists_headings_and_offers_pages() {
        let content = format!(
            "# Title\n\nintro\n\n## Section\n\n{}\n\n## Other\n\n{}\n",
            "body\n".repeat(300),
            "more\n".repeat(300)
        );
        let card = card_for("spec.md", &content);
        let rendered = card.render();
        assert!(rendered.contains("[sparse-read] spec.md"));
        assert!(rendered.contains("## Section"));
        assert!(rendered.contains("this is a preview, not the file"));

        // The map carries line ranges, and the roads lead to each section rather
        // than only to the front door. "Read all 600 lines" is the cost the gate
        // exists to avoid, not an answer to a question about one section.
        assert!(rendered.contains("sections:"), "no section map:\n{rendered}");
        assert!(rendered.contains("L1-L"), "no line ranges:\n{rendered}");

        let actions = card.next_actions();
        assert!(actions.len() >= 2, "expected a call per section: {actions:?}");
        assert!(
            actions.iter().any(|action| action.contains("## Section")),
            "no call for the Section heading: {actions:?}"
        );
        assert!(
            actions.iter().all(|action| action.contains("fs/read_text_file")),
            "a call named a tool the agent does not have: {actions:?}"
        );
    }

    #[test]
    fn a_card_always_names_a_way_to_get_the_whole_file() {
        let content = "line\n".repeat(900);
        let card = card_for("notes.md", &content);
        let actions = card.next_actions();
        assert!(!actions.is_empty());
        let last = actions.last().unwrap();
        assert!(last.contains("801-900"), "last action was {last}");
        assert!(last.contains("limit: 100"), "last action was {last}");
    }

    #[test]
    fn a_short_file_gets_one_full_content_action() {
        let card = card_for("notes.md", "a\nb\nc\n");
        let actions = card.next_actions();
        assert_eq!(actions.len(), 1);
        assert!(actions[0].contains("limit: 3"));
    }

    #[test]
    fn a_csv_card_reports_columns_types_and_row_count() {
        let mut content = String::from("id,name,amount,when\n");
        for index in 0..50 {
            content.push_str(&format!("{index},item{index},{index}.5,2024-01-0{}\n", index % 9 + 1));
        }
        let card = card_for("rows.csv", &content);
        match &card.structure {
            Structure::Tabular {
                columns,
                column_types,
                row_count,
            } => {
                assert_eq!(columns, &vec!["id", "name", "amount", "when"]);
                assert_eq!(column_types.get("id").unwrap(), "integer");
                assert_eq!(column_types.get("amount").unwrap(), "number");
                assert_eq!(column_types.get("when").unwrap(), "date");
                assert_eq!(column_types.get("name").unwrap(), "string");
                assert_eq!(*row_count, 50);
            }
            other => panic!("expected a table, got {other:?}"),
        }
    }

    #[test]
    fn a_csv_card_surfaces_error_values_as_signals() {
        let mut content = String::from("status,detail\n");
        for index in 0..40 {
            content.push_str(&format!("ok,row{index}\n"));
        }
        content.push_str("error,disk full\n");
        let card = card_for("rows.csv", &content);
        assert!(card
            .signals
            .iter()
            .any(|signal| signal.kind == "rare_or_error_values"));
    }

    #[test]
    fn a_quoted_csv_field_containing_a_comma_stays_one_field() {
        let fields = split_record(r#"1,"a,b",3"#, ',');
        assert_eq!(fields, vec!["1", "a,b", "3"]);
    }

    #[test]
    fn an_escaped_quote_inside_a_csv_field_survives() {
        let fields = split_record(r#"1,"say ""hi""",3"#, ',');
        assert_eq!(fields, vec!["1", r#"say "hi""#, "3"]);
    }

    #[test]
    fn a_tsv_card_uses_tabs_as_the_delimiter() {
        let content = "a\tb\n1\t2\n3\t4\n";
        let card = card_for("rows.tsv", content);
        match &card.structure {
            Structure::Tabular { columns, .. } => assert_eq!(columns, &vec!["a", "b"]),
            other => panic!("expected a table, got {other:?}"),
        }
    }

    #[test]
    fn a_json_object_card_reports_its_keys() {
        let content = r#"{"name":"x","count":3,"nested":{"a":1}}"#;
        let card = card_for("data.json", content);
        match &card.structure {
            Structure::Mapping { container, keys, .. } => {
                assert_eq!(container, "object");
                assert!(keys.contains(&"name".to_string()));
                assert!(keys.contains(&"nested".to_string()));
            }
            other => panic!("expected a mapping, got {other:?}"),
        }
    }

    #[test]
    fn a_json_array_card_reports_its_length() {
        let content = r#"[{"a":1},{"a":2},{"a":3}]"#;
        let card = card_for("data.json", content);
        match &card.structure {
            Structure::Mapping { container, length, .. } => {
                assert_eq!(container, "array");
                assert_eq!(*length, Some(3));
            }
            other => panic!("expected a mapping, got {other:?}"),
        }
    }

    #[test]
    fn malformed_json_falls_back_to_a_text_preview_instead_of_failing() {
        let card = card_for("data.json", "{not json at all\nmore\n");
        assert!(matches!(card.structure, Structure::Text { .. }));
    }

    #[test]
    fn a_json_card_surfaces_error_strings_as_signals() {
        let content = r#"{"status":"error","detail":"timeout after 30s"}"#;
        let card = card_for("data.json", content);
        assert!(card
            .signals
            .iter()
            .any(|signal| signal.kind == "interesting_scalar"));
    }

    #[test]
    fn an_xml_card_reports_the_root_and_child_counts() {
        let content = "<root a=\"1\"><item/><item/><other/></root>";
        let card = card_for("data.xml", content);
        match &card.structure {
            Structure::Markup {
                root,
                children,
                attributes,
            } => {
                assert_eq!(root, "root");
                assert_eq!(children.get("item"), Some(&2));
                assert_eq!(children.get("other"), Some(&1));
                assert_eq!(attributes, &vec!["a"]);
            }
            other => panic!("expected markup, got {other:?}"),
        }
    }

    #[test]
    fn a_log_card_counts_levels_and_names_repeats() {
        let mut content = String::new();
        for index in 0..40 {
            content.push_str(&format!("2024-01-01T00:00:00Z INFO request {index} ok\n"));
        }
        content.push_str("2024-01-01T00:00:01Z ERROR disk full\n");
        let card = card_for("app.log", &content);
        match &card.structure {
            Structure::Log { levels, .. } => {
                assert_eq!(levels.get("INFO"), Some(&40));
                assert_eq!(levels.get("ERROR"), Some(&1));
            }
            other => panic!("expected a log, got {other:?}"),
        }
        assert!(card.samples.iter().any(|sample| sample.text.contains("disk full")));
    }

    #[test]
    fn a_pdf_is_reported_as_not_previewed_rather_than_faked() {
        let card = card_for("paper.pdf", "%PDF-1.4 binary");
        match &card.structure {
            Structure::Unsupported { reason } => assert!(reason.contains("not ported")),
            other => panic!("expected unsupported, got {other:?}"),
        }
    }

    #[test]
    fn a_collection_card_lists_its_largest_members() {
        let dir = std::env::temp_dir().join("vibedesk-sparse-card-collection");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("big.md"), "x".repeat(5000)).unwrap();
        std::fs::write(dir.join("small.md"), "x").unwrap();
        std::fs::write(dir.join("rows.csv"), "a,b\n").unwrap();

        let info = detect::inspect(&dir);
        let decision = gate::decide(&info, &GateContext::default());
        let card = build(&info, &decision, "", &ReadTool::default());
        match &card.structure {
            Structure::Collection {
                file_count,
                kind_counts,
                ..
            } => {
                assert_eq!(*file_count, 3);
                assert_eq!(kind_counts.get("text"), Some(&2));
                assert_eq!(kind_counts.get("csv"), Some(&1));
            }
            other => panic!("expected a collection, got {other:?}"),
        }
        assert_eq!(card.samples[0].text, "big.md");
    }

    #[test]
    fn human_bytes_reads_the_way_a_person_would_say_it() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2.0 KB");
        assert_eq!(human_bytes(5 * 1024 * 1024), "5.0 MB");
    }

    #[test]
    fn a_long_sample_is_clipped_with_a_marker() {
        let clipped = clip(&"x".repeat(500));
        assert!(clipped.ends_with("...[clipped]"));
        assert!(clipped.chars().count() <= CLIP_CHARS);
    }
}
