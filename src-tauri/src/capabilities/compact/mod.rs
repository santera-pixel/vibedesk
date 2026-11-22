//! Context compaction at the model-request boundary.
//!
//! This is the replacement for the CLI's own compaction. The mechanism is
//! billion-context's, and it is worth stating plainly because it is unusual:
//! **the kernel never writes a summary.** It assigns every message a stable
//! ref, watches the context grow, and when folding is worth it, tells the model
//! which ranges to fold. The model writes the summary and calls `compress`. The
//! kernel validates the range, records a block, and from then on renders that
//! range as the block's summary.
//!
//! That division is the whole point. A deterministic summariser loses
//! information silently; a model-authored summary is the model's own record of
//! what it decided mattered, and it can be brought back with `decompress`.
//!
//! ```text
//! assign refs → store large results → prune (fold) → measure → nudge
//! ```
//!
//! ## The three tiers
//!
//! A block has a tier, and the tier says what it folded:
//!
//! | Tier | Folds | The model is asked to |
//! | --- | --- | --- |
//! | 1 | raw messages | summarise the range |
//! | 2 | tier-1 blocks | distil the summaries to decisions and outcomes |
//! | 3 | tier-2 blocks | condense those to a lookup index |
//!
//! Escalation is driven by what is left to fold, not by a timer. While there are
//! raw messages worth folding the kernel asks for tier 1, because a summary of
//! raw messages keeps more than a summary of summaries. Only when the foldable
//! messages no longer add up to a worthwhile range do the summaries themselves
//! become the remaining context — and that is what tier 2 is for.
//!
//! A block that folded blocks records them in `direct_block_ids` and inherits
//! their `effective_message_ids`, so `prune` renders it in place of the same
//! messages without knowing which tier produced it. Decompressing a distilled
//! block reactivates the blocks it consumed, which is what undo means here.

pub mod nudge;
pub mod refs;
pub mod rules;
pub mod types;
pub mod wire;

use std::collections::{BTreeMap, BTreeSet};

use types::{
    CompressionBlock, CompressionState, Config, ContentType, CoreMessage, Role, StoredContent,
};

/// The marker a stored tool result is replaced with.
pub const STORED_MARKER: &str = "[acp-stored";
/// How much of a stored result stays visible.
const STORED_HEAD_CHARS: usize = 96;

/// One range the model asked to fold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompressRange {
    pub start_ref: String,
    pub end_ref: String,
    pub summary: String,
    pub topic: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompressError {
    UnknownRef(String),
    ReversedRange(String, String),
    AlreadyCovered(String),
    SummaryTooShort(usize),
    SummaryTooLong(usize),
    TouchesProtectedTail(String),
    /// A range whose ends name different kinds of thing.
    MixedRange(String, String),
    /// A distillation needs at least two blocks to be worth a model call.
    NothingToDistil(usize),
}

impl CompressError {
    pub fn message(&self) -> String {
        match self {
            CompressError::UnknownRef(reference) => {
                format!("ref {reference} does not exist in this session")
            }
            CompressError::ReversedRange(start, end) => {
                format!("range {start}..{end} is reversed")
            }
            CompressError::AlreadyCovered(reference) => {
                format!("{reference} is already inside an active block")
            }
            CompressError::SummaryTooShort(length) => {
                format!("summary is {length} characters; the minimum is 50")
            }
            CompressError::SummaryTooLong(length) => {
                format!("summary is {length} characters; the maximum is 20000")
            }
            CompressError::TouchesProtectedTail(reference) => format!(
                "{reference} is in the protected tail; the active turn is never folded"
            ),
            CompressError::MixedRange(start, end) => format!(
                "range {start}..{end} mixes a message ref with a block ref; a range names one kind"
            ),
            CompressError::NothingToDistil(count) => format!(
                "a distillation needs at least two blocks; this range names {count}"
            ),
        }
    }
}

/// What one turn produced.
#[derive(Debug, Clone)]
pub struct ProcessedTurn {
    /// The message array to send upstream.
    pub messages: Vec<CoreMessage>,
    /// The nudge to inject, if one is due.
    pub nudge: Option<nudge::Nudge>,
    pub tokens_before: u64,
    pub tokens_after: u64,
    /// How many active blocks the view folded.
    pub folded_blocks: usize,
    /// Tokens the model could still reclaim by folding.
    pub pending_tokens: u64,
}

/// The kernel. Stateless itself: all memory lives in [`CompressionState`], which
/// the caller persists per session.
#[derive(Debug, Clone, Default)]
pub struct Kernel {
    pub config: Config,
}

impl Kernel {
    pub fn new(config: Config) -> Self {
        Self { config }
    }

    /// Prepare one request: assign refs, store large results, fold, measure,
    /// and decide whether to nudge.
    pub fn process_turn(
        &self,
        messages: &mut Vec<CoreMessage>,
        state: &mut CompressionState,
    ) -> ProcessedTurn {
        // The caller's array is the thing the kernel annotates: refs are written
        // onto it so a later `apply_compression` and `prune` see the same ids.
        refs::assign_refs(messages, &mut state.refs);

        let tokens_before = messages.iter().map(CoreMessage::tokens).sum();

        if self.config.ccr_enabled {
            store_large_results(messages, state, &self.config);
        }

        let view = self.prune(messages, state);
        let tokens_after = view.iter().map(CoreMessage::tokens).sum();

        let foldable = self.foldable_ids(messages, state);
        let ranges = nudge::compressible_ranges(messages, &state.refs, &foldable);
        let mut pending_tokens: u64 = ranges.iter().map(|range| range.tokens).sum();

        // Escalate only when folding has run out of material. While there are
        // raw messages worth folding, a fold keeps more than a distillation of
        // summaries would, so the second tier waits its turn.
        let (distill, floor) = if pending_tokens < self.config.min_compress_range {
            let candidates = nudge::distillable_ranges(state, &self.config);
            pending_tokens = candidates.iter().map(|range| range.tokens).sum();
            (candidates, self.config.min_distill_range)
        } else {
            (Vec::new(), self.config.min_compress_range)
        };
        let asking_tier = distill
            .first()
            .map(nudge::DistillRange::target_tier)
            .unwrap_or(1);

        let decision = nudge::decide(tokens_after, &self.config, state, pending_tokens, floor);
        let nudge = match decision {
            nudge::NudgeDecision::Send {
                voice,
                pending_tokens,
            } => {
                let growth = tokens_after.saturating_sub(state.nudge.last_shown_tokens);
                let text = nudge::render(
                    voice,
                    tokens_after,
                    growth,
                    &self.config,
                    &ranges,
                    &distill,
                    state,
                    false,
                );
                state.nudge.last_shown_tokens = tokens_after;
                state.nudge.shown_count += 1;
                *state
                    .nudge
                    .last_shown_by_tier
                    .entry(asking_tier)
                    .or_insert(0) = tokens_after;
                Some(nudge::Nudge {
                    voice,
                    text,
                    pending_tokens,
                })
            }
            nudge::NudgeDecision::Hold { .. } => None,
        };

        ProcessedTurn {
            folded_blocks: state.active_blocks().count(),
            messages: view,
            nudge,
            tokens_before,
            tokens_after,
            pending_tokens,
        }
    }

    /// The ids the model is allowed to fold: everything except the protected
    /// tail and anything already covered.
    pub fn foldable_ids(
        &self,
        messages: &[CoreMessage],
        state: &CompressionState,
    ) -> BTreeSet<String> {
        let covered = state.covered_ids();
        let tail = self.protected_tail_start(messages);
        messages
            .iter()
            .enumerate()
            .filter(|(index, message)| *index < tail && !covered.contains(message.id.as_str()))
            .map(|(_, message)| message.id.clone())
            .collect()
    }

    /// The index of the first message in the protected tail.
    ///
    /// Two rules, both upstream's: the last N messages are never folded, and
    /// neither is anything inside the last N tokens. The active turn has to
    /// stay verbatim or the model loses the thread it is holding.
    fn protected_tail_start(&self, messages: &[CoreMessage]) -> usize {
        let by_count = messages.len().saturating_sub(self.config.preserve_recent_messages);
        let mut tokens = 0u64;
        let mut by_tokens = messages.len();
        for (index, message) in messages.iter().enumerate().rev() {
            tokens += message.tokens();
            if tokens > self.config.preserve_recent_tokens {
                by_tokens = index + 1;
                break;
            }
            by_tokens = index;
        }
        by_count.min(by_tokens)
    }

    /// Build the view: covered messages replaced by their block's summary.
    pub fn prune(&self, messages: &[CoreMessage], state: &CompressionState) -> Vec<CoreMessage> {
        // Which block owns each covered id, and where that block first appears.
        let mut owner: BTreeMap<&str, &CompressionBlock> = BTreeMap::new();
        for block in state.active_blocks() {
            for id in &block.effective_message_ids {
                owner.insert(id.as_str(), block);
            }
        }

        let mut emitted: BTreeSet<&str> = BTreeSet::new();
        let mut view: Vec<CoreMessage> = Vec::with_capacity(messages.len());

        for message in messages {
            match owner.get(message.id.as_str()) {
                Some(block) => {
                    if emitted.insert(block.block_id.as_str()) {
                        view.push(CoreMessage {
                            id: format!("summary-{}", block.block_id),
                            role: Role::System,
                            content_type: ContentType::Text,
                            text: block.render(),
                            tool_name: None,
                            tool_call_id: None,
                            blocks: None,
                        });
                    }
                }
                None => view.push(message.clone()),
            }
        }
        view
    }

    /// Fold one or more ranges.
    ///
    /// Ranges are applied in order and each is validated against the state as
    /// it stands, so a batch that overlaps itself fails on the second range
    /// rather than silently double-counting.
    pub fn apply_compression(
        &self,
        ranges: &[CompressRange],
        messages: &[CoreMessage],
        state: &mut CompressionState,
    ) -> Result<Vec<String>, CompressError> {
        // Re-assign so this works whether or not the caller already ran
        // `process_turn` on this exact array. Assignment is idempotent: an
        // existing message keeps its ref.
        let mut messages = messages.to_vec();
        refs::assign_refs(&mut messages, &mut state.refs);

        let mut created = Vec::new();
        for range in ranges {
            let block_id = self.apply_one(range, &messages, state)?;
            created.push(block_id);
        }
        Ok(created)
    }

    fn apply_one(
        &self,
        range: &CompressRange,
        messages: &[CoreMessage],
        state: &mut CompressionState,
    ) -> Result<String, CompressError> {
        let summary_chars = range.summary.chars().count();
        if summary_chars < self.config.min_summary_length {
            return Err(CompressError::SummaryTooShort(summary_chars));
        }
        if summary_chars > self.config.max_summary_length {
            return Err(CompressError::SummaryTooLong(summary_chars));
        }

        let (start_ref, end_ref) = refs::ordered_ref_pair(&range.start_ref, &range.end_ref);
        if start_ref != range.start_ref {
            return Err(CompressError::ReversedRange(
                range.start_ref.clone(),
                range.end_ref.clone(),
            ));
        }

        // A range names either messages or blocks, never both. Which one it is
        // decides whether this is a fold or a distillation, and the two have
        // different validation: a fold must not touch the protected tail, a
        // distillation cannot touch it by construction.
        match (refs::is_block_ref(&start_ref), refs::is_block_ref(&end_ref)) {
            (true, true) => return self.apply_block_range(range, &start_ref, &end_ref, state),
            (false, false) => {}
            _ => return Err(CompressError::MixedRange(start_ref, end_ref)),
        }

        let start_index = self.resolve(&start_ref, messages, state)?;
        let end_index = self.resolve(&end_ref, messages, state)?;
        if start_index > end_index {
            return Err(CompressError::ReversedRange(start_ref, end_ref));
        }

        let tail = self.protected_tail_start(messages);
        if end_index >= tail {
            let reference = state
                .refs
                .by_raw
                .get(&messages[end_index].id)
                .cloned()
                .unwrap_or(end_ref);
            return Err(CompressError::TouchesProtectedTail(reference));
        }

        // A tool result must never be separated from the call that produced it:
        // the provider rejects an orphaned result, and the model loses the
        // pairing. Widen the range to whole pairs.
        let (start_index, end_index) = widen_to_tool_pairs(messages, start_index, end_index);

        // The block records the range it actually covers, not the range that was
        // asked for: otherwise the panel and a later decompress disagree with
        // what was folded.
        let start_ref = state
            .refs
            .by_raw
            .get(&messages[start_index].id)
            .cloned()
            .unwrap_or(start_ref);
        let end_ref = state
            .refs
            .by_raw
            .get(&messages[end_index].id)
            .cloned()
            .unwrap_or(end_ref);

        let covered = state.covered_ids();
        for message in &messages[start_index..=end_index] {
            if covered.contains(message.id.as_str()) {
                let reference = state
                    .refs
                    .by_raw
                    .get(&message.id)
                    .cloned()
                    .unwrap_or_default();
                return Err(CompressError::AlreadyCovered(reference));
            }
        }

        let direct: Vec<String> = messages[start_index..=end_index]
            .iter()
            .map(|message| message.id.clone())
            .collect();
        let compressed_tokens: u64 = messages[start_index..=end_index]
            .iter()
            .map(CoreMessage::tokens)
            .sum();

        let block_id = state.mint_block_id();
        state.blocks.push(CompressionBlock {
            block_id: block_id.clone(),
            tier: 1,
            topic: range.topic.clone(),
            summary: range.summary.trim().to_string(),
            effective_message_ids: direct.clone(),
            direct_message_ids: direct,
            direct_block_ids: Vec::new(),
            compressed_tokens,
            created_at: chrono::Utc::now().timestamp_millis(),
            active: true,
            start_ref,
            end_ref,
        });
        state.stats.compression_count += 1;
        state.stats.tokens_compressed += compressed_tokens;
        Ok(block_id)
    }

    /// Distil a run of active blocks into one.
    ///
    /// The new block inherits the covered blocks' `effective_message_ids`, so
    /// `prune` renders it in place of exactly the same messages. What it does
    /// *not* inherit is their summaries: the model writes a new one, from the
    /// old ones, which is the whole point of the tier.
    fn apply_block_range(
        &self,
        range: &CompressRange,
        start_ref: &str,
        end_ref: &str,
        state: &mut CompressionState,
    ) -> Result<String, CompressError> {
        let start = refs::block_ref_to_index(start_ref)
            .ok_or_else(|| CompressError::UnknownRef(start_ref.to_string()))?;
        let end = refs::block_ref_to_index(end_ref)
            .ok_or_else(|| CompressError::UnknownRef(end_ref.to_string()))?;

        // Oldest first, and every one of them still live. A block that is
        // already inactive was consumed by an earlier distillation, so naming it
        // again is the same mistake as folding an already-covered message.
        let mut covered: Vec<CompressionBlock> = Vec::new();
        for block in &state.blocks {
            let Some(index) = refs::block_ref_to_index(&block.block_id) else {
                continue;
            };
            if index < start || index > end {
                continue;
            }
            if !block.active {
                return Err(CompressError::AlreadyCovered(block.block_id.clone()));
            }
            covered.push(block.clone());
        }

        if covered.len() < 2 {
            return Err(CompressError::NothingToDistil(covered.len()));
        }

        // A distillation is one tier above the highest thing it folded, and
        // tier 3 is the ceiling: there is nothing above a lookup index.
        let tier = covered
            .iter()
            .map(|block| block.tier)
            .max()
            .unwrap_or(1)
            .saturating_add(1)
            .min(3);

        // What this block reclaims is the summaries it replaces, not the raw
        // messages underneath them — those were already counted when the child
        // blocks were created.
        let compressed_tokens: u64 = covered.iter().map(CompressionBlock::summary_tokens).sum();

        let mut effective: Vec<String> = Vec::new();
        for block in &covered {
            for id in &block.effective_message_ids {
                if !effective.contains(id) {
                    effective.push(id.clone());
                }
            }
        }

        let span_start = covered
            .iter()
            .map(|block| block.start_ref.clone())
            .min_by_key(|reference| refs::ref_sort_key(reference))
            .unwrap_or_else(|| start_ref.to_string());
        let span_end = covered
            .iter()
            .map(|block| block.end_ref.clone())
            .max_by_key(|reference| refs::ref_sort_key(reference))
            .unwrap_or_else(|| end_ref.to_string());
        let direct_block_ids: Vec<String> =
            covered.iter().map(|block| block.block_id.clone()).collect();

        for id in &direct_block_ids {
            if let Some(block) = state.block_mut(id) {
                block.active = false;
            }
        }

        let block_id = state.mint_block_id();
        state.blocks.push(CompressionBlock {
            block_id: block_id.clone(),
            tier,
            topic: range.topic.clone(),
            summary: range.summary.trim().to_string(),
            effective_message_ids: effective,
            direct_message_ids: Vec::new(),
            direct_block_ids,
            compressed_tokens,
            created_at: chrono::Utc::now().timestamp_millis(),
            active: true,
            start_ref: span_start,
            end_ref: span_end,
        });
        state.stats.compression_count += 1;
        state.stats.tokens_compressed += compressed_tokens;
        Ok(block_id)
    }

    fn resolve(
        &self,
        reference: &str,
        messages: &[CoreMessage],
        state: &CompressionState,
    ) -> Result<usize, CompressError> {
        let raw = refs::raw_for_ref(&state.refs, reference)
            .ok_or_else(|| CompressError::UnknownRef(reference.to_string()))?;
        messages
            .iter()
            .position(|message| message.id == raw)
            .ok_or_else(|| CompressError::UnknownRef(reference.to_string()))
    }

    /// Bring a folded range back.
    ///
    /// The block is deactivated rather than deleted: its summary stays readable
    /// in the panel, and a later fold can consume it again.
    ///
    /// A distilled block also reactivates the blocks it consumed. Undoing a
    /// distillation means going back to the summaries it was made from, not
    /// straight to the raw messages — otherwise one `decompress` would silently
    /// undo two tiers of work.
    pub fn decompress(
        &self,
        block_id: &str,
        state: &mut CompressionState,
    ) -> Option<CompressionBlock> {
        let block = state.block_mut(block_id)?;
        block.active = false;
        let children = block.direct_block_ids.clone();
        let snapshot = block.clone();
        for child in children {
            if let Some(child_block) = state.block_mut(&child) {
                child_block.active = true;
            }
        }
        Some(snapshot)
    }

    /// The original behind a stored tool result.
    pub fn retrieve(&self, reference: &str, state: &mut CompressionState) -> Option<StoredContent> {
        let stored = state.store.get(reference).cloned()?;
        state.stats.retrieval_count += 1;
        Some(stored)
    }

    /// A one-line status for the panel.
    pub fn status(&self, state: &CompressionState, tokens: u64) -> Status {
        // Only active blocks are counted per tier: a consumed child is no
        // longer part of the structure the model sees, so counting it would
        // make the panel disagree with the transcript.
        let mut tier_counts: BTreeMap<u8, usize> = BTreeMap::new();
        for block in state.active_blocks() {
            *tier_counts.entry(block.tier).or_insert(0) += 1;
        }
        Status {
            tokens,
            limit: self.config.model_context_limit,
            usage: self.config.usage(tokens),
            active_blocks: state.active_blocks().count(),
            total_blocks: state.blocks.len(),
            tier_counts,
            tokens_compressed: state.stats.tokens_compressed,
            compression_count: state.stats.compression_count,
            stored_count: state.store.len() as u64,
            retrieval_count: state.stats.retrieval_count,
            nudges_shown: state.nudge.shown_count,
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub tokens: u64,
    pub limit: u64,
    pub usage: f32,
    pub active_blocks: usize,
    pub total_blocks: usize,
    /// Active blocks per tier, so the panel can show the structure rather than
    /// a single count that hides whether anything was ever distilled.
    pub tier_counts: BTreeMap<u8, usize>,
    pub tokens_compressed: u64,
    pub compression_count: u64,
    pub stored_count: u64,
    pub retrieval_count: u64,
    pub nudges_shown: u64,
}

/// Widen a range so it never splits a tool call from its result.
fn widen_to_tool_pairs(
    messages: &[CoreMessage],
    mut start: usize,
    mut end: usize,
) -> (usize, usize) {
    // If the range starts on a tool result, pull in the call before it.
    while start > 0
        && messages[start].content_type == ContentType::ToolResult
        && messages[start - 1].content_type == ContentType::ToolCall
    {
        start -= 1;
    }
    // If the range ends on a tool call, pull in the result after it.
    while end + 1 < messages.len()
        && messages[end].content_type == ContentType::ToolCall
        && messages[end + 1].content_type == ContentType::ToolResult
    {
        end += 1;
    }
    (start, end)
}

/// Replace large tool results with a pointer, keeping the original in the store.
///
/// This is upstream's CCR. It is lossless in the way that matters: the bytes are
/// still there, one `acp_retrieve` away, and the model is told the ref.
fn store_large_results(
    messages: &mut [CoreMessage],
    state: &mut CompressionState,
    config: &Config,
) {
    for message in messages.iter_mut() {
        if message.content_type != ContentType::ToolResult {
            continue;
        }
        let tokens = message.tokens();
        if tokens < config.ccr_min_tool_tokens {
            continue;
        }
        if message.text.starts_with(STORED_MARKER) {
            continue;
        }
        let Some(reference) = state.refs.by_raw.get(&message.id).cloned() else {
            continue;
        };
        if state.store.contains_key(&reference) {
            continue;
        }

        let head: String = message.text.chars().take(STORED_HEAD_CHARS).collect();
        let head = head.replace('\n', " ");
        let tool = message.tool_name.clone().unwrap_or_else(|| "tool".into());
        state.store.insert(
            reference.clone(),
            StoredContent {
                reference: reference.clone(),
                tool_name: tool.clone(),
                text: message.text.clone(),
                tokens,
                stored_at: chrono::Utc::now().timestamp_millis(),
            },
        );
        state.stats.stored_count += 1;
        message.text = format!(
            "{STORED_MARKER} {reference} {tool} {tokens} tok] {head}… (call acp_retrieve with ref \"{reference}\" for the full output)"
        );
        // The text is now a marker, not a projection of the blocks. Keeping
        // them would put the original payload back on the wire and undo the
        // store.
        message.blocks = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kernel() -> Kernel {
        Kernel::new(Config {
            model_context_limit: 100_000,
            preserve_recent_messages: 2,
            preserve_recent_tokens: 1,
            ccr_min_tool_tokens: 1000,
            ..Config::default()
        })
    }

    fn conversation(count: usize) -> Vec<CoreMessage> {
        (0..count)
            .map(|index| {
                CoreMessage::text(
                    if index % 2 == 0 { Role::User } else { Role::Assistant },
                    format!("message {index} {}", "x".repeat(400)),
                )
            })
            .collect()
    }

    fn tool_result(text: &str) -> CoreMessage {
        CoreMessage {
            id: String::new(),
            role: Role::Tool,
            content_type: ContentType::ToolResult,
            text: text.into(),
            tool_name: Some("exec".into()),
            tool_call_id: Some("call-1".into()),
            blocks: None,
        }
    }

    fn tool_call() -> CoreMessage {
        CoreMessage {
            id: String::new(),
            role: Role::Assistant,
            content_type: ContentType::ToolCall,
            text: "exec".into(),
            tool_name: Some("exec".into()),
            tool_call_id: Some("call-1".into()),
            blocks: None,
        }
    }

    /// Fold two ranges, so there are two tier-1 blocks to distil.
    fn two_blocks(kernel: &Kernel, messages: &[CoreMessage], state: &mut CompressionState) {
        kernel
            .apply_compression(
                &[
                    CompressRange {
                        start_ref: "m00001".into(),
                        end_ref: "m00002".into(),
                        summary: "The user asked for a thing and the assistant did it. ".repeat(2),
                        topic: Some("setup".into()),
                    },
                    CompressRange {
                        start_ref: "m00003".into(),
                        end_ref: "m00004".into(),
                        summary: "Then the assistant ran a command and read the output. ".repeat(2),
                        topic: Some("run".into()),
                    },
                ],
                messages,
                state,
            )
            .unwrap();
    }

    #[test]
    fn a_distillation_folds_blocks_and_inherits_the_messages_under_them() {
        let mut state = CompressionState::default();
        let mut messages = conversation(10);
        let kernel = kernel();
        kernel.process_turn(&mut messages, &mut state);
        two_blocks(&kernel, &messages, &mut state);

        let created = kernel
            .apply_compression(
                &[CompressRange {
                    start_ref: "b1".into(),
                    end_ref: "b2".into(),
                    summary: "Setup and the first command, both settled. ".repeat(2),
                    topic: Some("distilled".into()),
                }],
                &messages,
                &mut state,
            )
            .unwrap();
        assert_eq!(created, vec!["b3"]);

        let distilled = state.block("b3").unwrap();
        assert_eq!(distilled.tier, 2);
        assert_eq!(distilled.direct_block_ids, vec!["b1", "b2"]);
        assert!(distilled.direct_message_ids.is_empty());
        // The four messages the two children held, in order.
        assert_eq!(distilled.effective_message_ids.len(), 4);
        assert_eq!(distilled.start_ref, "m00001");
        assert_eq!(distilled.end_ref, "m00004");

        // The children are consumed, not deleted: the panel still lists them.
        assert!(!state.block("b1").unwrap().active);
        assert!(!state.block("b2").unwrap().active);
        assert_eq!(state.blocks.len(), 3);
    }

    #[test]
    fn a_distillation_renders_in_place_of_exactly_the_same_messages() {
        let mut state = CompressionState::default();
        let mut messages = conversation(10);
        let kernel = kernel();
        kernel.process_turn(&mut messages, &mut state);
        two_blocks(&kernel, &messages, &mut state);
        let before = kernel.prune(&messages, &state);

        kernel
            .apply_compression(
                &[CompressRange {
                    start_ref: "b1".into(),
                    end_ref: "b2".into(),
                    summary: "Setup and the first command, both settled. ".repeat(2),
                    topic: None,
                }],
                &messages,
                &mut state,
            )
            .unwrap();
        let after = kernel.prune(&messages, &state);

        // Two summary lines became one, and nothing else moved: the same raw
        // messages are hidden, the same tail is visible. A distillation that
        // changed which messages were covered would mean it had folded something
        // it did not name.
        assert_eq!(after.len(), before.len() - 1);
        assert_eq!(after[0].text, state.block("b3").unwrap().render());
        assert!(after[0].text.contains("settled"));

        let visible = |view: &[CoreMessage]| -> Vec<String> {
            view.iter()
                .filter(|message| !message.id.starts_with("summary-"))
                .map(|message| message.id.clone())
                .collect()
        };
        assert_eq!(visible(&before), visible(&after));
    }

    #[test]
    fn decompressing_a_distillation_brings_back_the_summaries_it_consumed() {
        let mut state = CompressionState::default();
        let mut messages = conversation(10);
        let kernel = kernel();
        kernel.process_turn(&mut messages, &mut state);
        two_blocks(&kernel, &messages, &mut state);
        kernel
            .apply_compression(
                &[CompressRange {
                    start_ref: "b1".into(),
                    end_ref: "b2".into(),
                    summary: "Setup and the first command, both settled. ".repeat(2),
                    topic: None,
                }],
                &messages,
                &mut state,
            )
            .unwrap();

        kernel.decompress("b3", &mut state).unwrap();

        // Undoing a distillation means going back to the summaries it was made
        // from, not straight to the raw messages.
        assert!(state.block("b1").unwrap().active);
        assert!(state.block("b2").unwrap().active);
        assert!(!state.block("b3").unwrap().active);
        let view = kernel.prune(&messages, &state);
        assert!(view[0].text.contains("setup"));
        assert!(view[1].text.contains("run"));
    }

    #[test]
    fn a_range_that_mixes_a_message_ref_with_a_block_ref_is_refused() {
        let mut state = CompressionState::default();
        let mut messages = conversation(10);
        let kernel = kernel();
        kernel.process_turn(&mut messages, &mut state);
        two_blocks(&kernel, &messages, &mut state);

        let error = kernel
            .apply_compression(
                &[CompressRange {
                    start_ref: "b1".into(),
                    end_ref: "m00004".into(),
                    summary: "A summary long enough to pass the minimum length check. ".repeat(2),
                    topic: None,
                }],
                &messages,
                &mut state,
            )
            .unwrap_err();
        assert!(matches!(error, CompressError::MixedRange(_, _)));
        assert!(error.message().contains("mixes a message ref with a block ref"));
    }

    #[test]
    fn distilling_a_single_block_is_refused_because_it_is_a_rename() {
        let mut state = CompressionState::default();
        let mut messages = conversation(10);
        let kernel = kernel();
        kernel.process_turn(&mut messages, &mut state);
        two_blocks(&kernel, &messages, &mut state);

        let error = kernel
            .apply_compression(
                &[CompressRange {
                    start_ref: "b1".into(),
                    end_ref: "b1".into(),
                    summary: "A summary long enough to pass the minimum length check. ".repeat(2),
                    topic: None,
                }],
                &messages,
                &mut state,
            )
            .unwrap_err();
        assert!(matches!(error, CompressError::NothingToDistil(1)));
    }

    #[test]
    fn distilling_a_block_that_was_already_consumed_is_refused() {
        let mut state = CompressionState::default();
        let mut messages = conversation(10);
        let kernel = kernel();
        kernel.process_turn(&mut messages, &mut state);
        two_blocks(&kernel, &messages, &mut state);
        kernel
            .apply_compression(
                &[CompressRange {
                    start_ref: "b1".into(),
                    end_ref: "b2".into(),
                    summary: "Setup and the first command, both settled. ".repeat(2),
                    topic: None,
                }],
                &messages,
                &mut state,
            )
            .unwrap();

        let error = kernel
            .apply_compression(
                &[CompressRange {
                    start_ref: "b1".into(),
                    end_ref: "b2".into(),
                    summary: "A second distillation of the same two blocks. ".repeat(2),
                    topic: None,
                }],
                &messages,
                &mut state,
            )
            .unwrap_err();
        assert!(matches!(error, CompressError::AlreadyCovered(_)));
    }

    #[test]
    fn the_third_tier_is_the_ceiling() {
        let mut state = CompressionState::default();
        let mut messages = conversation(20);
        let kernel = kernel();
        kernel.process_turn(&mut messages, &mut state);

        // Four tier-1 blocks.
        for index in 0..4 {
            let start = index * 2 + 1;
            kernel
                .apply_compression(
                    &[CompressRange {
                        start_ref: format!("m{start:05}"),
                        end_ref: format!("m{:05}", start + 1),
                        summary: format!("Range {index} was consumed and summarised. ").repeat(2),
                        topic: None,
                    }],
                    &messages,
                    &mut state,
                )
                .unwrap();
        }
        // Two tier-2 blocks.
        kernel
            .apply_compression(
                &[CompressRange {
                    start_ref: "b1".into(),
                    end_ref: "b2".into(),
                    summary: "The first two ranges, distilled to their outcomes. ".repeat(2),
                    topic: None,
                }],
                &messages,
                &mut state,
            )
            .unwrap();
        kernel
            .apply_compression(
                &[CompressRange {
                    start_ref: "b3".into(),
                    end_ref: "b4".into(),
                    summary: "The second two ranges, distilled to their outcomes. ".repeat(2),
                    topic: None,
                }],
                &messages,
                &mut state,
            )
            .unwrap();
        assert_eq!(state.block("b5").unwrap().tier, 2);
        assert_eq!(state.block("b6").unwrap().tier, 2);

        // One tier-3 block.
        kernel
            .apply_compression(
                &[CompressRange {
                    start_ref: "b5".into(),
                    end_ref: "b6".into(),
                    summary: "Everything so far, condensed to a lookup index. ".repeat(2),
                    topic: None,
                }],
                &messages,
                &mut state,
            )
            .unwrap();
        assert_eq!(state.block("b7").unwrap().tier, 3);

        // And a distillation of tier-3 blocks stays at tier 3: there is nothing
        // above a lookup index.
        kernel
            .apply_compression(
                &[CompressRange {
                    start_ref: "b7".into(),
                    end_ref: "b7".into(),
                    summary: "A summary long enough to pass the minimum length check. ".repeat(2),
                    topic: None,
                }],
                &messages,
                &mut state,
            )
            .unwrap_err();
        assert_eq!(state.block("b7").unwrap().tier, 3);
    }

    #[test]
    fn a_distillation_reclaims_the_summaries_it_replaces_not_the_raw_messages() {
        let mut state = CompressionState::default();
        let mut messages = conversation(10);
        let kernel = kernel();
        kernel.process_turn(&mut messages, &mut state);
        two_blocks(&kernel, &messages, &mut state);
        let after_folds = state.stats.tokens_compressed;

        kernel
            .apply_compression(
                &[CompressRange {
                    start_ref: "b1".into(),
                    end_ref: "b2".into(),
                    summary: "Setup and the first command, both settled. ".repeat(2),
                    topic: None,
                }],
                &messages,
                &mut state,
            )
            .unwrap();

        let reclaimed = state.stats.tokens_compressed - after_folds;
        let summaries: u64 = state
            .blocks
            .iter()
            .filter(|block| block.block_id == "b1" || block.block_id == "b2")
            .map(CompressionBlock::summary_tokens)
            .sum();
        assert_eq!(reclaimed, summaries);
        // Counting the raw messages again would double-count them.
        assert!(reclaimed < after_folds);
    }

    #[test]
    fn a_turn_assigns_refs_and_reports_both_sides_of_the_measurement() {
        let mut state = CompressionState::default();
        let turn = kernel().process_turn(&mut conversation(6), &mut state);
        assert_eq!(state.refs.by_raw.len(), 6);
        assert!(turn.tokens_before > 0);
        assert_eq!(turn.tokens_before, turn.tokens_after);
        assert_eq!(turn.folded_blocks, 0);
    }

    #[test]
    fn folding_replaces_the_range_with_one_summary_message() {
        let mut state = CompressionState::default();
        let mut messages = conversation(10);
        let mut kernel = kernel();
        kernel.config.model_context_limit = 10_000_000;
        kernel.process_turn(&mut messages, &mut state);

        let block_id = kernel
            .apply_compression(
                &[CompressRange {
                    start_ref: "m00001".into(),
                    end_ref: "m00004".into(),
                    summary: "The user asked for a thing and the assistant did it. ".repeat(2),
                    topic: Some("setup".into()),
                }],
                &messages,
                &mut state,
            )
            .unwrap();
        assert_eq!(block_id, vec!["b1"]);

        let view = kernel.prune(&messages, &state);
        assert_eq!(view.len(), 7);
        assert!(view[0].text.starts_with("[compressed m00001..m00004 [setup]]"));
        assert_eq!(view[0].role, Role::System);
    }

    #[test]
    fn a_folded_range_is_never_rendered_twice() {
        let mut state = CompressionState::default();
        let mut messages = conversation(10);
        let mut kernel = kernel();
        kernel.config.model_context_limit = 10_000_000;
        kernel.process_turn(&mut messages, &mut state);
        kernel
            .apply_compression(
                &[CompressRange {
                    start_ref: "m00001".into(),
                    end_ref: "m00004".into(),
                    summary: "A summary long enough to pass the minimum length check.".into(),
                    topic: None,
                }],
                &messages,
                &mut state,
            )
            .unwrap();
        let view = kernel.prune(&messages, &state);
        let summaries = view
            .iter()
            .filter(|message| message.text.starts_with("[compressed"))
            .count();
        assert_eq!(summaries, 1);
    }

    #[test]
    fn folding_shrinks_the_view() {
        let mut state = CompressionState::default();
        let mut messages = conversation(20);
        let mut kernel = kernel();
        kernel.config.model_context_limit = 10_000_000;
        let before = kernel.process_turn(&mut messages, &mut state).tokens_after;
        kernel
            .apply_compression(
                &[CompressRange {
                    start_ref: "m00001".into(),
                    end_ref: "m00010".into(),
                    summary: "Ten messages of setup, summarised in one line that is long enough.".into(),
                    topic: None,
                }],
                &messages,
                &mut state,
            )
            .unwrap();
        let after = kernel.process_turn(&mut messages, &mut state).tokens_after;
        assert!(after < before, "before {before}, after {after}");
    }

    #[test]
    fn a_summary_that_is_too_short_is_rejected() {
        let mut state = CompressionState::default();
        let mut messages = conversation(10);
        let kernel = kernel();
        kernel.process_turn(&mut messages, &mut state);
        let error = kernel
            .apply_compression(
                &[CompressRange {
                    start_ref: "m00001".into(),
                    end_ref: "m00002".into(),
                    summary: "too short".into(),
                    topic: None,
                }],
                &messages,
                &mut state,
            )
            .unwrap_err();
        assert!(matches!(error, CompressError::SummaryTooShort(_)));
    }

    #[test]
    fn an_unknown_ref_is_rejected_with_its_name() {
        let mut state = CompressionState::default();
        let mut messages = conversation(4);
        let kernel = kernel();
        kernel.process_turn(&mut messages, &mut state);
        let error = kernel
            .apply_compression(
                &[CompressRange {
                    start_ref: "m09999".into(),
                    end_ref: "m09999".into(),
                    summary: "A summary long enough to pass the minimum length check.".into(),
                    topic: None,
                }],
                &messages,
                &mut state,
            )
            .unwrap_err();
        assert_eq!(error, CompressError::UnknownRef("m09999".into()));
    }

    #[test]
    fn a_reversed_range_is_rejected() {
        let mut state = CompressionState::default();
        let mut messages = conversation(10);
        let kernel = kernel();
        kernel.process_turn(&mut messages, &mut state);
        let error = kernel
            .apply_compression(
                &[CompressRange {
                    start_ref: "m00005".into(),
                    end_ref: "m00002".into(),
                    summary: "A summary long enough to pass the minimum length check.".into(),
                    topic: None,
                }],
                &messages,
                &mut state,
            )
            .unwrap_err();
        assert!(matches!(error, CompressError::ReversedRange(_, _)));
    }

    #[test]
    fn the_protected_tail_is_never_folded() {
        let mut state = CompressionState::default();
        let mut messages = conversation(10);
        let kernel = kernel();
        kernel.process_turn(&mut messages, &mut state);
        let error = kernel
            .apply_compression(
                &[CompressRange {
                    start_ref: "m00009".into(),
                    end_ref: "m00010".into(),
                    summary: "A summary long enough to pass the minimum length check.".into(),
                    topic: None,
                }],
                &messages,
                &mut state,
            )
            .unwrap_err();
        assert!(matches!(error, CompressError::TouchesProtectedTail(_)));
    }

    #[test]
    fn folding_the_same_range_twice_is_rejected() {
        let mut state = CompressionState::default();
        let mut messages = conversation(10);
        let mut kernel = kernel();
        kernel.config.model_context_limit = 10_000_000;
        kernel.process_turn(&mut messages, &mut state);
        let range = CompressRange {
            start_ref: "m00001".into(),
            end_ref: "m00003".into(),
            summary: "A summary long enough to pass the minimum length check.".into(),
            topic: None,
        };
        kernel
            .apply_compression(std::slice::from_ref(&range), &messages, &mut state)
            .unwrap();
        let error = kernel
            .apply_compression(&[range], &messages, &mut state)
            .unwrap_err();
        assert!(matches!(error, CompressError::AlreadyCovered(_)));
    }

    #[test]
    fn a_range_that_starts_on_a_tool_result_pulls_in_its_call() {
        let mut state = CompressionState::default();
        let mut messages = vec![
            CoreMessage::text(Role::User, "run it"),
            tool_call(),
            tool_result("output"),
            CoreMessage::text(Role::Assistant, "done"),
            CoreMessage::text(Role::User, "next"),
            CoreMessage::text(Role::Assistant, "ok"),
        ];
        let mut kernel = kernel();
        kernel.config.model_context_limit = 10_000_000;
        kernel.process_turn(&mut messages, &mut state);
        kernel
            .apply_compression(
                &[CompressRange {
                    start_ref: "m00003".into(),
                    end_ref: "m00003".into(),
                    summary: "A summary long enough to pass the minimum length check.".into(),
                    topic: None,
                }],
                &messages,
                &mut state,
            )
            .unwrap();
        let block = &state.blocks[0];
        assert_eq!(block.direct_message_ids.len(), 2);
        assert_eq!(block.start_ref, "m00002");
    }

    #[test]
    fn decompressing_brings_the_range_back() {
        let mut state = CompressionState::default();
        let mut messages = conversation(10);
        let mut kernel = kernel();
        kernel.config.model_context_limit = 10_000_000;
        kernel.process_turn(&mut messages, &mut state);
        kernel
            .apply_compression(
                &[CompressRange {
                    start_ref: "m00001".into(),
                    end_ref: "m00004".into(),
                    summary: "A summary long enough to pass the minimum length check.".into(),
                    topic: None,
                }],
                &messages,
                &mut state,
            )
            .unwrap();
        assert_eq!(kernel.prune(&messages, &state).len(), 7);
        kernel.decompress("b1", &mut state).unwrap();
        assert_eq!(kernel.prune(&messages, &state).len(), 10);
    }

    #[test]
    fn a_large_tool_result_is_stored_and_replaced_by_a_pointer() {
        let mut state = CompressionState::default();
        let mut messages = vec![
            CoreMessage::text(Role::User, "run it"),
            tool_call(),
            tool_result(&"log line\n".repeat(2000)),
        ];
        let kernel = kernel();
        kernel.process_turn(&mut messages, &mut state);
        assert_eq!(state.store.len(), 1);
        let stored = state.store.values().next().unwrap();
        assert!(stored.text.contains("log line"));
        assert_eq!(state.stats.stored_count, 1);
    }

    #[test]
    fn a_stored_result_can_be_retrieved_byte_for_byte() {
        let mut state = CompressionState::default();
        let original = "log line\n".repeat(2000);
        let mut messages = vec![
            CoreMessage::text(Role::User, "run it"),
            tool_call(),
            tool_result(&original),
        ];
        let kernel = kernel();
        kernel.process_turn(&mut messages, &mut state);
        let reference = state.store.keys().next().unwrap().clone();
        let stored = kernel.retrieve(&reference, &mut state).unwrap();
        assert_eq!(stored.text, original);
        assert_eq!(state.stats.retrieval_count, 1);
    }

    #[test]
    fn a_small_tool_result_is_left_alone() {
        let mut state = CompressionState::default();
        let mut messages = vec![
            CoreMessage::text(Role::User, "run it"),
            tool_call(),
            tool_result("short output"),
        ];
        let kernel = kernel();
        kernel.process_turn(&mut messages, &mut state);
        assert!(state.store.is_empty());
    }

    #[test]
    fn a_stored_result_is_not_stored_twice() {
        let mut state = CompressionState::default();
        let mut messages = vec![
            CoreMessage::text(Role::User, "run it"),
            tool_call(),
            tool_result(&"log line\n".repeat(2000)),
        ];
        let kernel = kernel();
        kernel.process_turn(&mut messages, &mut state);
        kernel.process_turn(&mut messages, &mut state);
        assert_eq!(state.store.len(), 1);
        assert_eq!(state.stats.stored_count, 1);
    }

    #[test]
    fn a_nudge_is_injected_once_the_context_passes_the_pressure_band() {
        let mut state = CompressionState::default();
        let mut kernel = kernel();
        kernel.config.model_context_limit = 1000;
        kernel.config.min_compress_range = 1;
        let turn = kernel.process_turn(&mut conversation(20), &mut state);
        let nudge = turn.nudge.expect("a nudge was due");
        assert_eq!(nudge.voice, nudge::Voice::Emergency);
        assert!(nudge.text.contains("Context limit reached"));
        assert_eq!(state.nudge.shown_count, 1);
    }

    #[test]
    fn a_quiet_context_gets_no_nudge() {
        let mut state = CompressionState::default();
        let mut kernel = kernel();
        kernel.config.model_context_limit = 10_000_000;
        let turn = kernel.process_turn(&mut conversation(4), &mut state);
        assert!(turn.nudge.is_none());
        assert_eq!(state.nudge.shown_count, 0);
    }

    #[test]
    fn status_reports_the_shape_the_panel_needs() {
        let mut state = CompressionState::default();
        let mut messages = conversation(10);
        let mut kernel = kernel();
        kernel.config.model_context_limit = 10_000_000;
        let turn = kernel.process_turn(&mut messages, &mut state);
        kernel
            .apply_compression(
                &[CompressRange {
                    start_ref: "m00001".into(),
                    end_ref: "m00004".into(),
                    summary: "A summary long enough to pass the minimum length check.".into(),
                    topic: None,
                }],
                &messages,
                &mut state,
            )
            .unwrap();
        let status = kernel.status(&state, turn.tokens_after);
        assert_eq!(status.active_blocks, 1);
        assert_eq!(status.total_blocks, 1);
        assert_eq!(status.compression_count, 1);
        assert!(status.tokens_compressed > 0);
        assert_eq!(status.tier_counts.get(&1), Some(&1));
    }

    #[test]
    fn status_counts_only_the_active_blocks_per_tier() {
        // A distillation leaves two blocks behind: the tier-2 parent that is
        // active and the two tier-1 children it consumed. The panel must show
        // one block at tier 2 and nothing at tier 1, because that is what the
        // model's transcript contains.
        let mut state = CompressionState::default();
        let mut messages = conversation(12);
        let mut kernel = kernel();
        kernel.config.model_context_limit = 10_000_000;
        kernel.process_turn(&mut messages, &mut state);
        kernel
            .apply_compression(
                &[
                    CompressRange {
                        start_ref: "m00001".into(),
                        end_ref: "m00003".into(),
                        summary: "First summary, long enough to pass the minimum length check.".into(),
                        topic: None,
                    },
                    CompressRange {
                        start_ref: "m00004".into(),
                        end_ref: "m00006".into(),
                        summary: "Second summary, long enough to pass the minimum length check.".into(),
                        topic: None,
                    },
                ],
                &messages,
                &mut state,
            )
            .unwrap();
        let before = kernel.status(&state, 0);
        assert_eq!(before.tier_counts.get(&1), Some(&2));

        kernel
            .apply_compression(
                &[CompressRange {
                    start_ref: "b1".into(),
                    end_ref: "b2".into(),
                    summary: "A distillation of both summaries, long enough to pass the check.".into(),
                    topic: Some("distilled".into()),
                }],
                &messages,
                &mut state,
            )
            .unwrap();

        let after = kernel.status(&state, 0);
        assert_eq!(after.tier_counts.get(&1), None);
        assert_eq!(after.tier_counts.get(&2), Some(&1));
        assert_eq!(after.active_blocks, 1);
        assert_eq!(after.total_blocks, 3);
    }
}
