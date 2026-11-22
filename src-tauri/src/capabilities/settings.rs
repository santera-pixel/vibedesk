//! Persisted capability choices.
//!
//! Deliberately its own file rather than a field on [`crate::settings::Settings`]:
//! the capability layer is additive, and a separate file means a concurrent
//! change to the main settings shape cannot collide with it. It also means a
//! corrupt capability file costs the user their toggles, not their workstations.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::paths;

/// Per-capability configuration that is not just on/off.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CapabilitySettings {
    /// `capability-key -> enabled`. A key that is absent keeps its default.
    pub enabled: BTreeMap<String, bool>,
    /// Compaction proxy settings.
    pub compaction: CompactionSettings,
    /// Terminal compaction settings.
    pub terminal: TerminalSettings,
    /// Sparse reading settings.
    pub reading: ReadingSettings,
    /// Memory settings.
    pub memory: MemorySettings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CompactionSettings {
    /// The model window the kernel plans against, in tokens. Zero means "read
    /// it from the active model's `max_context_length`".
    pub context_limit: u64,
    /// Fraction of the window at which the nudge gate opens.
    pub nudge_threshold: f64,
    /// Fraction of the window at which the emergency truncation valve opens.
    pub truncate_threshold: f64,
    /// Messages at the tail that are never compressed.
    pub preserve_recent: u32,
    /// Tool names whose results are never compressed.
    pub protected_tools: Vec<String>,
    /// Whether lossless tool-result offload (CCR) is on.
    pub ccr: bool,
    /// Tool results at or above this many tokens are offloaded.
    pub ccr_min_tokens: u64,
    /// Pending tokens at or above which the nudge asks for a compression.
    /// Below it, the nudge asks for a distillation instead: folding two
    /// summaries is cheaper than folding raw messages, so it is the right ask
    /// while the conversation is still small.
    pub min_compress_range: u64,
    /// Pending tokens at or above which a distillation is worth asking for.
    /// Lower than `min_compress_range` because a distillation produces a
    /// summary, not a transcript, so the saving is real at a smaller size.
    pub min_distill_range: u64,
    /// The provider the proxy forwards to. Recorded when compaction is switched
    /// on, so the proxy never has to guess which upstream it stands in front of.
    pub upstream_provider: String,
    /// The loopback port the proxy listens on. Zero means "not started yet".
    pub proxy_port: u16,
}

impl Default for CompactionSettings {
    fn default() -> Self {
        Self {
            context_limit: 0,
            nudge_threshold: 0.45,
            truncate_threshold: 0.92,
            preserve_recent: 6,
            protected_tools: vec![
                "read_file".into(),
                "write_file".into(),
                "edit".into(),
                "search_replace".into(),
            ],
            ccr: true,
            ccr_min_tokens: 1200,
            min_compress_range: 5000,
            min_distill_range: 600,
            upstream_provider: String::new(),
            proxy_port: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TerminalSettings {
    /// The largest output, in characters, the agent is handed inline. Anything
    /// longer is summarised to head and tail.
    pub max_inline_chars: u64,
    /// Rule ids the user switched off.
    pub disabled_rules: Vec<String>,
    /// Commands whose output is never touched, on top of the built-in
    /// file-inspection passthrough.
    pub passthrough_commands: Vec<String>,
}

impl Default for TerminalSettings {
    fn default() -> Self {
        Self {
            max_inline_chars: 1200,
            disabled_rules: Vec::new(),
            passthrough_commands: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ReadingSettings {
    /// Files at or above this many bytes are candidates for a card.
    pub document_floor: u64,
    /// A directory listing at or above this many entries is a collection.
    pub collection_floor: u64,
    /// `auto`, `force` or `native`.
    pub mode: String,
}

impl Default for ReadingSettings {
    fn default() -> Self {
        Self {
            document_floor: 4096,
            collection_floor: 8,
            mode: "auto".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MemorySettings {
    /// How many memories are kept before the prune phase starts dropping.
    pub max_memories: u32,
    /// Sessions that must accumulate before a consolidation pass is worth it.
    pub min_sessions: u32,
    /// Whether recalled memories are injected into the proxy's system prompt.
    pub inject_recall: bool,
}

impl Default for MemorySettings {
    fn default() -> Self {
        Self {
            max_memories: 40,
            min_sessions: 2,
            inject_recall: true,
        }
    }
}

pub fn path() -> PathBuf {
    paths::app_data_dir().join("capabilities.json")
}

pub fn load() -> CapabilitySettings {
    let Ok(raw) = std::fs::read_to_string(path()) else {
        return CapabilitySettings::default();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

pub fn save(settings: &CapabilitySettings) -> Result<(), String> {
    let path = path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let body = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    std::fs::write(&path, body).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The file is the contract between builds. A field added later must not
    /// stop an older file from loading, and the defaults must be the ones the
    /// registry assumes.
    #[test]
    fn an_empty_file_yields_the_documented_defaults() {
        let settings: CapabilitySettings = serde_json::from_str("{}").unwrap();
        assert!(settings.enabled.is_empty());
        assert_eq!(settings.compaction.nudge_threshold, 0.45);
        assert_eq!(settings.compaction.preserve_recent, 6);
        assert!(settings.compaction.ccr);
        // Distillation must be reachable before compression is: it is the
        // cheaper ask, and a higher gate would make tiers 2 and 3 dead code.
        assert!(settings.compaction.min_distill_range < settings.compaction.min_compress_range);
        assert_eq!(settings.terminal.max_inline_chars, 1200);
        assert_eq!(settings.reading.mode, "auto");
        assert_eq!(settings.memory.max_memories, 40);
    }

    #[test]
    fn a_partial_file_keeps_the_untouched_defaults() {
        let settings: CapabilitySettings =
            serde_json::from_str(r#"{"compaction":{"ccr":false}}"#).unwrap();
        assert!(!settings.compaction.ccr);
        // Untouched siblings keep their defaults rather than zeroing.
        assert_eq!(settings.compaction.nudge_threshold, 0.45);
        assert_eq!(settings.terminal.max_inline_chars, 1200);
    }

    #[test]
    fn the_wire_shape_is_camel_case() {
        let body = serde_json::to_string(&CapabilitySettings::default()).unwrap();
        assert!(body.contains(r#""maxInlineChars""#), "{body}");
        assert!(body.contains(r#""nudgeThreshold""#), "{body}");
        assert!(!body.contains("max_inline_chars"), "{body}");
    }
}
