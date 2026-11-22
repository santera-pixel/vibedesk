//! The compaction data model.
//!
//! Ported from billion-context's `kernel/src/types.ts`, reduced to the fields
//! this port actually maintains. The reduction is deliberate and is recorded in
//! `docs/PLAN-capabilities.md`: the upstream type carries a dozen optional
//! fields for host-specific lanes (ledger numbering, fork adoption, media
//! handles, emergency-truncate latches) that only exist because upstream serves
//! seven different clients. VibeDesk is one client with one lane.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
    System,
    Tool,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::System => "system",
            Role::Tool => "tool",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ContentType {
    Text,
    ToolCall,
    ToolResult,
    Reasoning,
}

/// One message, in the shape the kernel reasons about.
///
/// `id` is the stable raw id. On the wire there is no id, so the proxy derives
/// one from the message's own bytes — see [`super::refs::raw_id`]. That is what
/// makes a ref survive across turns: the same message hashes to the same id, so
/// it keeps the same `mNNNNN`.
///
/// `text` is a projection, not the message. A content array can carry blocks
/// the kernel has no opinion about — an image, most importantly — and folding
/// the message down to its text would delete them. `blocks` keeps the original
/// array so [`super::wire::encode`] can put it back.
///
/// The invariant: `blocks` is only meaningful while `text` is still exactly the
/// text projection of `blocks`. Anything that rewrites `text` must clear it,
/// because the two would otherwise disagree and the stale array would win.
#[derive(Debug, Clone, PartialEq)]
pub struct CoreMessage {
    pub id: String,
    pub role: Role,
    pub content_type: ContentType,
    pub text: String,
    pub tool_name: Option<String>,
    pub tool_call_id: Option<String>,
    /// The original `content` array, when the message had one.
    pub blocks: Option<Value>,
}

impl CoreMessage {
    pub fn text(role: Role, text: impl Into<String>) -> Self {
        Self {
            id: String::new(),
            role,
            content_type: ContentType::Text,
            text: text.into(),
            tool_name: None,
            tool_call_id: None,
            blocks: None,
        }
    }

    /// A rough token count. Upstream takes a `countTokens` port; the proxy has
    /// no tokenizer, so this is the standard four-characters-per-token estimate.
    /// It is used for thresholds and reporting, never for billing.
    pub fn tokens(&self) -> u64 {
        estimate_tokens(&self.text)
    }
}

pub fn estimate_tokens(text: &str) -> u64 {
    estimate_tokens_chars(text.chars().count() as u64)
}

/// The same estimate from a character count.
///
/// The capabilities that measure their own payload already know how many
/// characters crossed them, and re-walking the string to count them again is
/// work for nothing. One rule, two entry points, so a token figure from the
/// terminal and a token figure from the kernel are the same unit.
pub fn estimate_tokens_chars(chars: u64) -> u64 {
    chars.div_ceil(4)
}

/// One folded range of the conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompressionBlock {
    /// `b1`, `b2`, ... — the id the model cites when distilling.
    pub block_id: String,
    pub tier: u8,
    pub topic: Option<String>,
    /// The model's summary. This is the only record of the folded range.
    pub summary: String,
    /// Messages folded directly by this block.
    pub direct_message_ids: Vec<String>,
    /// Direct messages plus everything inherited from consumed child blocks.
    pub effective_message_ids: Vec<String>,
    /// Blocks this one consumed, when it was a distillation.
    pub direct_block_ids: Vec<String>,
    pub compressed_tokens: u64,
    pub created_at: i64,
    /// False once a parent block consumed this one.
    pub active: bool,
    /// The ref span, kept so the panel and the nudge can name it.
    pub start_ref: String,
    pub end_ref: String,
}

impl CompressionBlock {
    pub fn summary_tokens(&self) -> u64 {
        estimate_tokens(&self.summary)
    }

    /// The line the model sees in place of the folded range.
    pub fn render(&self) -> String {
        let topic = self
            .topic
            .as_deref()
            .map(|topic| format!(" [{topic}]"))
            .unwrap_or_default();
        format!(
            "[compressed {}..{}{}] {}",
            self.start_ref, self.end_ref, topic, self.summary
        )
    }
}

/// Raw id ↔ ref, both directions.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageRefMap {
    pub by_raw: BTreeMap<String, String>,
    pub by_ref: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompressionStats {
    pub tokens_compressed: u64,
    pub compression_count: u64,
    pub stored_count: u64,
    pub retrieval_count: u64,
}

/// The nudge cadence state.
///
/// Upstream carries a dozen more fields for harvest-stall detection and
/// estimate-hold backstops. This port keeps the three that decide *when* to
/// speak: the last shown token count, the per-tier baselines, and the growth
/// reference.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NudgeState {
    /// Token count at the last nudge that was actually injected.
    pub last_shown_tokens: u64,
    /// Per-tier baseline: the token count at which that tier last fired.
    pub last_shown_by_tier: BTreeMap<u8, u64>,
    /// How many nudges have been injected this session.
    pub shown_count: u64,
}

/// One stored original, kept so a folded range can be brought back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredContent {
    pub reference: String,
    pub tool_name: String,
    pub text: String,
    pub tokens: u64,
    pub stored_at: i64,
}

/// Everything the kernel remembers about one session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompressionState {
    pub blocks: Vec<CompressionBlock>,
    pub refs: MessageRefMap,
    pub stats: CompressionStats,
    pub nudge: NudgeState,
    /// Next block number. Monotonic: a block id is never reused.
    pub next_block: u64,
    /// The CCR store, keyed by ref.
    pub store: BTreeMap<String, StoredContent>,
    /// When this conversation was first seen, in epoch milliseconds.
    ///
    /// The agent has no clock. It cannot tell a two-second turn from a
    /// twenty-minute one, so it keeps exploring long past the point where the
    /// user wanted a decision. These three fields are what let the transient
    /// block tell it how long it has actually been working.
    #[serde(default)]
    pub first_seen_ms: u64,
    /// When the previous model request arrived. Zero before the second one.
    #[serde(default)]
    pub last_request_ms: u64,
    /// How many model requests this conversation has made.
    #[serde(default)]
    pub requests: u64,
}

impl CompressionState {
    pub fn active_blocks(&self) -> impl Iterator<Item = &CompressionBlock> {
        self.blocks.iter().filter(|block| block.active)
    }

    /// Every message id currently hidden behind an active block.
    pub fn covered_ids(&self) -> std::collections::BTreeSet<&str> {
        self.active_blocks()
            .flat_map(|block| block.effective_message_ids.iter().map(String::as_str))
            .collect()
    }

    pub fn block(&self, block_id: &str) -> Option<&CompressionBlock> {
        self.blocks.iter().find(|block| block.block_id == block_id)
    }

    pub fn block_mut(&mut self, block_id: &str) -> Option<&mut CompressionBlock> {
        self.blocks
            .iter_mut()
            .find(|block| block.block_id == block_id)
    }

    pub fn mint_block_id(&mut self) -> String {
        self.next_block += 1;
        format!("b{}", self.next_block)
    }

    /// Record that a model request arrived at `now_ms`.
    ///
    /// Called once per request, before the kernel folds anything, so the gap
    /// this measures is the agent's own work between two model calls — tool
    /// runs, file reads, builds — which is exactly the time the user spent
    /// waiting without seeing anything.
    pub fn stamp_request(&mut self, now_ms: u64) {
        if self.first_seen_ms == 0 {
            self.first_seen_ms = now_ms;
        }
        self.last_request_ms = now_ms;
        self.requests += 1;
    }
}

/// The thresholds, with upstream's defaults.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub model_context_limit: u64,
    /// Fraction of the window at which a nudge becomes worth sending.
    pub min_context_limit_pct: f32,
    /// Fraction at which the nudge stops being advice and becomes pressure.
    pub max_context_limit_pct: f32,
    /// Fraction at which the nudge is an emergency.
    pub emergency_threshold_pct: f32,
    /// Growth, in tokens, that must accumulate before another nudge.
    pub growth_floor: u64,
    /// A range smaller than this is not worth folding.
    pub min_compress_range: u64,
    /// A run of summaries smaller than this is not worth distilling.
    ///
    /// Lower than `min_compress_range` on purpose. A distillation replaces N
    /// summaries with one, so the saving is `(N - 1) × summary`, and summaries
    /// are small: a floor tuned for raw messages would mean a session had to
    /// accumulate forty of them before the second tier ever fired.
    pub min_distill_range: u64,
    /// The largest summary accepted, in characters.
    pub max_summary_length: usize,
    /// The smallest summary accepted, in characters.
    pub min_summary_length: usize,
    /// Trailing messages never folded: the active turn plus the previous one.
    pub preserve_recent_messages: usize,
    /// Trailing tokens never folded.
    pub preserve_recent_tokens: u64,
    /// Tool results at or above this size are stored and replaced by a pointer.
    pub ccr_min_tool_tokens: u64,
    /// Whether the CCR store is on.
    pub ccr_enabled: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            // A conservative default. The proxy overrides this from the model
            // the user actually selected.
            model_context_limit: 200_000,
            min_context_limit_pct: 0.45,
            max_context_limit_pct: 0.75,
            emergency_threshold_pct: 0.95,
            growth_floor: 50_000,
            min_compress_range: 5_000,
            min_distill_range: 600,
            max_summary_length: 20_000,
            min_summary_length: 50,
            preserve_recent_messages: 3,
            preserve_recent_tokens: 5_000,
            ccr_min_tool_tokens: 4_000,
            ccr_enabled: true,
        }
    }
}

impl Config {
    pub fn usage(&self, tokens: u64) -> f32 {
        if self.model_context_limit == 0 {
            return 0.0;
        }
        tokens as f32 / self.model_context_limit as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_estimate_is_four_characters_per_token() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("abcde"), 2);
        assert_eq!(estimate_tokens(&"x".repeat(4000)), 1000);
    }

    #[test]
    fn a_block_renders_its_span_topic_and_summary() {
        let block = CompressionBlock {
            block_id: "b3".into(),
            tier: 1,
            topic: Some("auth refactor".into()),
            summary: "chose X over Y because Z".into(),
            direct_message_ids: vec![],
            effective_message_ids: vec![],
            direct_block_ids: vec![],
            compressed_tokens: 9000,
            created_at: 0,
            active: true,
            start_ref: "m00010".into(),
            end_ref: "m00042".into(),
        };
        assert_eq!(
            block.render(),
            "[compressed m00010..m00042 [auth refactor]] chose X over Y because Z"
        );
    }

    #[test]
    fn a_block_without_a_topic_renders_without_brackets() {
        let block = CompressionBlock {
            block_id: "b1".into(),
            tier: 1,
            topic: None,
            summary: "done".into(),
            direct_message_ids: vec![],
            effective_message_ids: vec![],
            direct_block_ids: vec![],
            compressed_tokens: 0,
            created_at: 0,
            active: true,
            start_ref: "m00001".into(),
            end_ref: "m00002".into(),
        };
        assert_eq!(block.render(), "[compressed m00001..m00002] done");
    }

    #[test]
    fn block_ids_are_monotonic_and_never_reused() {
        let mut state = CompressionState::default();
        assert_eq!(state.mint_block_id(), "b1");
        assert_eq!(state.mint_block_id(), "b2");
        state.blocks.clear();
        assert_eq!(state.mint_block_id(), "b3");
    }

    #[test]
    fn only_active_blocks_cover_messages() {
        let mut state = CompressionState::default();
        state.blocks.push(CompressionBlock {
            block_id: "b1".into(),
            tier: 1,
            topic: None,
            summary: "s".into(),
            direct_message_ids: vec!["a".into()],
            effective_message_ids: vec!["a".into()],
            direct_block_ids: vec![],
            compressed_tokens: 0,
            created_at: 0,
            active: false,
            start_ref: "m00001".into(),
            end_ref: "m00001".into(),
        });
        assert!(state.covered_ids().is_empty());
    }

    #[test]
    fn usage_is_a_fraction_of_the_window() {
        let config = Config {
            model_context_limit: 100_000,
            ..Config::default()
        };
        assert_eq!(config.usage(50_000), 0.5);
        assert_eq!(config.usage(0), 0.0);
    }
}
