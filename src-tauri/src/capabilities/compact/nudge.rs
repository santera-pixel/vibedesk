//! When to speak, and what to say.
//!
//! Ported from billion-context's `kernel/src/nudge-text.ts` and the decision
//! half of `compress.ts`. The nudge is the whole mechanism: the kernel never
//! writes a summary itself, it tells the model which ranges are worth folding
//! and lets the model write them. Everything here exists to make that message
//! arrive at the right moment and name the right ranges.
//!
//! ## The three bands, and the three tiers
//!
//! The bands decide *whether* to speak: gentle, pressure, emergency, with a
//! growth gate so the model is not nagged every turn. The tiers decide *what to
//! ask for*: fold raw messages, distil the summaries, or condense those.
//!
//! Escalation is by what is left, not by a timer. While the foldable messages
//! still add up to a worthwhile range the nudge asks for a fold. Only when they
//! do not do the summaries become the remaining context, and the nudge switches
//! to asking for a distillation — with the tier's own rule text, because
//! "summarise this range" and "distil these summaries" want different things
//! kept.

use std::collections::BTreeMap;

use super::rules;
use super::types::{CompressionBlock, CompressionState, Config, CoreMessage};

/// One contiguous run of foldable messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompressibleRange {
    pub start_ref: String,
    pub end_ref: String,
    pub count: usize,
    pub tokens: u64,
    /// How many of the messages are tool results — the cheapest thing to fold.
    pub tool_count: usize,
    /// How many are user messages — the most expensive thing to fold.
    pub user_count: usize,
}

/// A run of active blocks that could be distilled into one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DistillRange {
    /// The block refs, which is what the model cites.
    pub start_ref: String,
    pub end_ref: String,
    pub block_count: usize,
    /// The summaries' tokens — what the distillation would reclaim.
    pub tokens: u64,
    /// The tier the covered blocks are at, so the nudge can name the right one.
    pub tier: u8,
}

impl DistillRange {
    /// The tier the new block will be.
    pub fn target_tier(&self) -> u8 {
        self.tier.saturating_add(1).min(3)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Voice {
    Gentle,
    Pressure,
    Emergency,
}

impl Voice {
    pub fn as_str(self) -> &'static str {
        match self {
            Voice::Gentle => "gentle",
            Voice::Pressure => "pressure",
            Voice::Emergency => "emergency",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Nudge {
    pub voice: Voice,
    pub text: String,
    /// Tokens the listed ranges would reclaim if folded.
    pub pending_tokens: u64,
}

fn format_k(tokens: u64) -> String {
    if tokens >= 1000 {
        format!("{:.1}K", tokens as f64 / 1000.0)
    } else {
        tokens.to_string()
    }
}

/// Group the foldable messages into contiguous ref runs.
///
/// `foldable` is the set of message ids the caller has decided may be folded —
/// everything except the protected tail and anything already covered.
pub fn compressible_ranges(
    messages: &[CoreMessage],
    refs: &super::types::MessageRefMap,
    foldable: &std::collections::BTreeSet<String>,
) -> Vec<CompressibleRange> {
    let mut ranges: Vec<CompressibleRange> = Vec::new();
    let mut current: Option<CompressibleRange> = None;

    for message in messages {
        let Some(reference) = refs.by_raw.get(&message.id) else {
            continue;
        };
        if !foldable.contains(&message.id) {
            if let Some(range) = current.take() {
                ranges.push(range);
            }
            continue;
        }
        let tokens = message.tokens();
        let is_tool = matches!(
            message.content_type,
            super::types::ContentType::ToolResult
        );
        let is_user = message.role == super::types::Role::User;
        match current.as_mut() {
            Some(range) => {
                range.end_ref = reference.clone();
                range.count += 1;
                range.tokens += tokens;
                range.tool_count += usize::from(is_tool);
                range.user_count += usize::from(is_user);
            }
            None => {
                current = Some(CompressibleRange {
                    start_ref: reference.clone(),
                    end_ref: reference.clone(),
                    count: 1,
                    tokens,
                    tool_count: usize::from(is_tool),
                    user_count: usize::from(is_user),
                });
            }
        }
    }
    if let Some(range) = current {
        ranges.push(range);
    }
    ranges
}

/// The runs of active blocks worth distilling, lowest tier first.
///
/// One range per tier, covering every block at that tier. Splitting a tier into
/// several ranges would ask the model to make several calls for one decision,
/// and the whole point of the nudge is that it is one call.
pub fn distillable_ranges(state: &CompressionState, config: &Config) -> Vec<DistillRange> {
    let mut by_tier: BTreeMap<u8, Vec<&CompressionBlock>> = BTreeMap::new();
    for block in state.active_blocks() {
        by_tier.entry(block.tier).or_default().push(block);
    }

    let mut ranges = Vec::new();
    for (tier, blocks) in by_tier {
        // One block is not a distillation, it is a rename.
        if blocks.len() < 2 {
            continue;
        }
        let tokens: u64 = blocks.iter().map(|block| block.summary_tokens()).sum();
        if tokens < config.min_distill_range {
            continue;
        }
        let Some(first) = blocks.first() else { continue };
        let Some(last) = blocks.last() else { continue };
        ranges.push(DistillRange {
            start_ref: first.block_id.clone(),
            end_ref: last.block_id.clone(),
            block_count: blocks.len(),
            tokens,
            tier,
        });
    }
    ranges
}

fn format_distill_ranges(ranges: &[DistillRange]) -> String {
    let mut lines = vec!["Distillable block runs (oldest first):".to_string()];
    for range in ranges {
        lines.push(format!(
            "  {}–{}  {} blocks | {} | tier {} → {}",
            range.start_ref,
            range.end_ref,
            range.block_count,
            format_k(range.tokens),
            range.tier,
            range.target_tier()
        ));
    }
    lines.join("
")
}

fn format_ranges(ranges: &[CompressibleRange]) -> String {
    if ranges.is_empty() {
        return "[No specific ranges detected — compress any consumed content.]".to_string();
    }
    let mut lines = vec!["Compressible ranges (oldest first):".to_string()];
    for range in ranges {
        let mut parts = vec![format!("{} msgs", range.count), format_k(range.tokens)];
        if range.tool_count > 0 {
            parts.push(format!("{} tool", range.tool_count));
        }
        if range.user_count > 0 {
            parts.push(format!("{} user", range.user_count));
        }
        lines.push(format!(
            "  {}–{}  {}",
            range.start_ref,
            range.end_ref,
            parts.join(" | ")
        ));
    }
    lines.join("\n")
}

fn format_block_map(state: &CompressionState) -> String {
    let spans: Vec<String> = state
        .active_blocks()
        .map(|block| format!("{}={}–{}", block.block_id, block.start_ref, block.end_ref))
        .collect();
    if spans.is_empty() {
        return String::new();
    }
    let shown = if spans.len() > 8 {
        format!("…+{} older · {}", spans.len() - 8, spans[spans.len() - 8..].join(" · "))
    } else {
        spans.join(" · ")
    };
    format!("Active blocks ({}): {shown}", spans.len())
}

fn format_breakdown(tokens: u64, limit: u64, growth: u64) -> String {
    let pct = if limit == 0 {
        0.0
    } else {
        tokens as f64 / limit as f64 * 100.0
    };
    let growth_line = if growth > 0 {
        format!("\n+{} since last nudge", format_k(growth))
    } else {
        String::new()
    };
    format!(
        "Context: {} of {} ({pct:.0}%){growth_line}",
        format_k(tokens),
        format_k(limit)
    )
}

/// The one-call reminder. Upstream keeps this as a single static line on
/// purpose: an expanded per-range skeleton grows with the range count, reads
/// like a mandatory to-do list, and erodes the model's own judgement about what
/// is worth folding.
const ONE_CALL_HINT: &str = "ONE call, ONE string — fold every range you keep into a single compress call: one JSON-escaped string holding one block per range ('m00150–m00220 topic' header line, then the summary). Ranges you still need can wait — they reappear in later nudges; never split the batch across separate calls.";

/// Anthropic-shaped lanes prime the model for the Claude-Code compaction
/// envelope, so it writes `<analysis>`/`<summary>` tags in its reply text
/// instead of calling the tool. A reply with no tool call ends the turn.
const ANTHROPIC_LANE_NOTE: &str = "Write the summary ONLY as the `compress` tool's `content` argument. Never emit `<analysis>` or `<summary>` tags in your reply text — they are not parsed, and a reply without a tool call ends the turn.";

/// Render the nudge for a decision.
///
/// `distill` is non-empty only when there is nothing left worth folding, and it
/// changes what the nudge asks for: the ranges become block refs and the rule
/// text becomes the tier's own. Sending the fold rules with block refs would
/// tell the model to summarise a summary, which is how a distillation loses the
/// decisions it was supposed to keep.
pub fn render(
    voice: Voice,
    tokens: u64,
    growth: u64,
    config: &Config,
    ranges: &[CompressibleRange],
    distill: &[DistillRange],
    state: &CompressionState,
    anthropic_lane: bool,
) -> String {
    let breakdown = format_breakdown(tokens, config.model_context_limit, growth);
    let ranges_text = if distill.is_empty() {
        format_ranges(ranges)
    } else {
        format_distill_ranges(distill)
    };
    let block_map = format_block_map(state);
    // The tier the nudge is asking for, when it is asking for a distillation.
    let tier_rules = distill
        .first()
        .map(|range| rules::distill_rules(range.target_tier()));
    let lane_note = if anthropic_lane {
        format!("\n\n{ANTHROPIC_LANE_NOTE}")
    } else {
        String::new()
    };

    let mut parts: Vec<String> = Vec::new();
    match voice {
        Voice::Emergency => {
            parts.push(format!(
                "Context limit reached — compress now. Prioritize consumed tool outputs.\n\n{}",
                rules::COMPRESS_PHILOSOPHY
            ));
            parts.push(breakdown);
            parts.push(match tier_rules {
                Some(text) => text.to_string(),
                None => rules::HOW_TO_COMPRESS_RULES.to_string(),
            });
            parts.push("Only use IDs from visible messages above. Compress older work first.".into());
            parts.push(ranges_text);
            if !block_map.is_empty() {
                parts.push(block_map);
            }
            parts.push(ONE_CALL_HINT.to_string());
        }
        Voice::Pressure | Voice::Gentle => {
            parts.push(format!(
                "This is an efficiency nudge to compress early and keep context lean — not an overflow warning. A separate, stronger alert will appear if the context is actually full.\n\n{}",
                rules::COMPRESS_PHILOSOPHY
            ));
            parts.push(breakdown);
            parts.push(match tier_rules {
                Some(text) => text.to_string(),
                None => rules::HOW_TO_COMPRESS_RULES.to_string(),
            });
            parts.push(ranges_text);
            if !block_map.is_empty() {
                parts.push(block_map);
            }
            parts.push(ONE_CALL_HINT.to_string());
        }
    }
    format!("{}{lane_note}", parts.join("\n\n"))
}

/// Why a nudge was or was not sent. Kept so the panel can explain the silence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NudgeDecision {
    Send {
        voice: Voice,
        pending_tokens: u64,
    },
    Hold {
        reason: &'static str,
    },
}

/// Decide whether to nudge, and in which voice.
///
/// The bands are upstream's: below `min_context_limit_pct` nothing is said at
/// all; between there and `max_context_limit_pct` a nudge needs real growth
/// since the last one, so the model is not nagged every turn; above
/// `max_context_limit_pct` growth is ignored because the window is the problem;
/// above `emergency_threshold_pct` the voice changes.
pub fn decide(
    tokens: u64,
    config: &Config,
    state: &CompressionState,
    pending_tokens: u64,
    floor: u64,
) -> NudgeDecision {
    let usage = config.usage(tokens);

    // The floor is the gate for the thing being asked for, not always the
    // compression gate. A distillation folds summaries, which are small by
    // construction, so measuring it against `min_compress_range` would hold
    // every distillation back forever and leave tiers 2 and 3 unreachable.
    if pending_tokens < floor {
        return NudgeDecision::Hold {
            reason: "nothing large enough left to fold",
        };
    }

    let growth = tokens.saturating_sub(state.nudge.last_shown_tokens);

    if usage >= config.emergency_threshold_pct {
        return NudgeDecision::Send {
            voice: Voice::Emergency,
            pending_tokens,
        };
    }
    if usage >= config.max_context_limit_pct {
        return NudgeDecision::Send {
            voice: Voice::Pressure,
            pending_tokens,
        };
    }
    if usage < config.min_context_limit_pct {
        return NudgeDecision::Hold {
            reason: "context is below the nudge floor",
        };
    }
    if growth < config.growth_floor {
        return NudgeDecision::Hold {
            reason: "not enough growth since the last nudge",
        };
    }
    NudgeDecision::Send {
        voice: Voice::Gentle,
        pending_tokens,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::compact::refs;
    use crate::capabilities::compact::types::{ContentType, MessageRefMap, Role};

    fn config() -> Config {
        Config {
            model_context_limit: 100_000,
            ..Config::default()
        }
    }

    fn state_at(tokens: u64) -> CompressionState {
        let mut state = CompressionState::default();
        state.nudge.last_shown_tokens = tokens;
        state
    }

    /// A distillation folds summaries, which are small by construction. If the
    /// gate were always `min_compress_range`, a distill candidate could never
    /// clear it and tiers 2 and 3 would be unreachable — the nudge would hold
    /// forever with "nothing large enough left to fold".
    #[test]
    fn a_distillation_is_measured_against_its_own_gate() {
        let config = config();
        assert!(config.min_distill_range < config.min_compress_range);

        // 1.4k of summaries: far below the compression gate, above the
        // distillation gate.
        let pending = 1_400;
        assert!(pending < config.min_compress_range);
        assert!(pending >= config.min_distill_range);

        let held = decide(60_000, &config, &state_at(0), pending, config.min_compress_range);
        assert!(matches!(held, NudgeDecision::Hold { .. }));

        let sent = decide(60_000, &config, &state_at(0), pending, config.min_distill_range);
        assert!(matches!(sent, NudgeDecision::Send { .. }));
    }

    #[test]
    fn nothing_is_said_below_the_floor() {
        let decision = decide(40_000, &config(), &state_at(0), 50_000, config().min_compress_range);
        assert_eq!(
            decision,
            NudgeDecision::Hold {
                reason: "context is below the nudge floor"
            }
        );
    }

    #[test]
    fn a_gentle_nudge_needs_real_growth_since_the_last_one() {
        let decision = decide(50_000, &config(), &state_at(20_000), 50_000, config().min_compress_range);
        assert_eq!(
            decision,
            NudgeDecision::Hold {
                reason: "not enough growth since the last nudge"
            }
        );
    }

    #[test]
    fn a_gentle_nudge_fires_once_growth_clears_the_floor() {
        let decision = decide(70_000, &config(), &state_at(10_000), 50_000, config().min_compress_range);
        assert_eq!(
            decision,
            NudgeDecision::Send {
                voice: Voice::Gentle,
                pending_tokens: 50_000
            }
        );
    }

    #[test]
    fn pressure_ignores_growth_because_the_window_is_the_problem() {
        let decision = decide(80_000, &config(), &state_at(79_000), 50_000, config().min_compress_range);
        assert_eq!(
            decision,
            NudgeDecision::Send {
                voice: Voice::Pressure,
                pending_tokens: 50_000
            }
        );
    }

    #[test]
    fn the_emergency_band_changes_the_voice() {
        let decision = decide(96_000, &config(), &state_at(95_000), 50_000, config().min_compress_range);
        assert_eq!(
            decision,
            NudgeDecision::Send {
                voice: Voice::Emergency,
                pending_tokens: 50_000
            }
        );
    }

    #[test]
    fn a_nudge_is_withheld_when_there_is_nothing_worth_folding() {
        let decision = decide(96_000, &config(), &state_at(0), 100, config().min_compress_range);
        assert_eq!(
            decision,
            NudgeDecision::Hold {
                reason: "nothing large enough left to fold"
            }
        );
    }

    #[test]
    fn contiguous_foldable_messages_become_one_range() {
        let mut messages = vec![
            CoreMessage::text(Role::User, "do the thing"),
            CoreMessage::text(Role::Assistant, "working on it"),
            CoreMessage::text(Role::User, "and another thing"),
        ];
        let mut map = MessageRefMap::default();
        refs::assign_refs(&mut messages, &mut map);
        let foldable: std::collections::BTreeSet<String> =
            messages.iter().map(|message| message.id.clone()).collect();
        let ranges = compressible_ranges(&messages, &map, &foldable);
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].start_ref, "m00001");
        assert_eq!(ranges[0].end_ref, "m00003");
        assert_eq!(ranges[0].count, 3);
    }

    #[test]
    fn a_protected_message_splits_the_range() {
        let mut messages = vec![
            CoreMessage::text(Role::User, "old"),
            CoreMessage::text(Role::User, "keep me"),
            CoreMessage::text(Role::User, "newer"),
        ];
        let mut map = MessageRefMap::default();
        refs::assign_refs(&mut messages, &mut map);
        let foldable: std::collections::BTreeSet<String> = messages
            .iter()
            .filter(|message| message.text != "keep me")
            .map(|message| message.id.clone())
            .collect();
        let ranges = compressible_ranges(&messages, &map, &foldable);
        assert_eq!(ranges.len(), 2);
        assert_eq!(ranges[0].end_ref, "m00001");
        assert_eq!(ranges[1].start_ref, "m00003");
    }

    #[test]
    fn a_range_counts_its_tool_results_and_user_messages() {
        let mut messages = vec![
            CoreMessage::text(Role::User, "run it"),
            CoreMessage {
                id: String::new(),
                role: Role::Tool,
                content_type: ContentType::ToolResult,
                text: "output".into(),
                tool_name: Some("exec".into()),
                tool_call_id: Some("c1".into()),
                blocks: None,
            },
        ];
        let mut map = MessageRefMap::default();
        refs::assign_refs(&mut messages, &mut map);
        let foldable: std::collections::BTreeSet<String> =
            messages.iter().map(|message| message.id.clone()).collect();
        let ranges = compressible_ranges(&messages, &map, &foldable);
        assert_eq!(ranges[0].tool_count, 1);
        assert_eq!(ranges[0].user_count, 1);
    }

    #[test]
    fn the_rendered_nudge_names_the_ranges_and_the_rules() {
        let ranges = vec![CompressibleRange {
            start_ref: "m00010".into(),
            end_ref: "m00042".into(),
            count: 33,
            tokens: 48_000,
            tool_count: 20,
            user_count: 2,
        }];
        let text = render(
            Voice::Gentle,
            80_000,
            30_000,
            &config(),
            &ranges,
            &[],
            &CompressionState::default(),
            false,
        );
        assert!(text.contains("m00010–m00042"));
        assert!(text.contains("48.0K"));
        assert!(text.contains("HOW TO COMPRESS"));
        assert!(text.contains("ONE call, ONE string"));
        assert!(text.contains("80.0K of 100.0K (80%)"));
        assert!(!text.contains("analysis"));
    }

    #[test]
    fn the_emergency_nudge_says_so_in_its_first_line() {
        let text = render(
            Voice::Emergency,
            96_000,
            0,
            &config(),
            &[],
            &[],
            &CompressionState::default(),
            false,
        );
        assert!(text.starts_with("Context limit reached — compress now."));
        assert!(text.contains("No specific ranges detected"));
    }

    #[test]
    fn an_anthropic_lane_gets_the_tool_call_warning() {
        let text = render(
            Voice::Gentle,
            80_000,
            0,
            &config(),
            &[],
            &[],
            &CompressionState::default(),
            true,
        );
        assert!(text.contains("Write the summary ONLY as the `compress` tool's `content` argument"));
    }

    fn block(id: &str, tier: u8, summary: &str) -> CompressionBlock {
        CompressionBlock {
            block_id: id.into(),
            tier,
            topic: None,
            summary: summary.into(),
            direct_message_ids: vec![],
            effective_message_ids: vec![],
            direct_block_ids: vec![],
            compressed_tokens: 0,
            created_at: 0,
            active: true,
            start_ref: "m00001".into(),
            end_ref: "m00020".into(),
        }
    }

    #[test]
    fn one_block_is_not_a_distillation_so_nothing_is_offered() {
        let mut state = CompressionState::default();
        state.blocks.push(block("b1", 1, &"a summary ".repeat(200)));
        assert!(distillable_ranges(&state, &config()).is_empty());
    }

    #[test]
    fn a_run_of_summaries_below_the_floor_is_not_worth_a_model_call() {
        let mut state = CompressionState::default();
        state.blocks.push(block("b1", 1, "short"));
        state.blocks.push(block("b2", 1, "short"));
        assert!(distillable_ranges(&state, &config()).is_empty());
    }

    #[test]
    fn a_run_of_summaries_above_the_floor_is_offered_as_one_range() {
        let mut state = CompressionState::default();
        for index in 1..=4 {
            state
                .blocks
                .push(block(&format!("b{index}"), 1, &"a summary ".repeat(200)));
        }
        let ranges = distillable_ranges(&state, &config());
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].start_ref, "b1");
        assert_eq!(ranges[0].end_ref, "b4");
        assert_eq!(ranges[0].block_count, 4);
        assert_eq!(ranges[0].tier, 1);
        assert_eq!(ranges[0].target_tier(), 2);
    }

    #[test]
    fn each_tier_gets_its_own_range_lowest_first() {
        let mut state = CompressionState::default();
        state.blocks.push(block("b1", 1, &"a summary ".repeat(200)));
        state.blocks.push(block("b2", 1, &"a summary ".repeat(200)));
        state.blocks.push(block("b3", 2, &"a summary ".repeat(200)));
        state.blocks.push(block("b4", 2, &"a summary ".repeat(200)));
        let ranges = distillable_ranges(&state, &config());
        assert_eq!(ranges.len(), 2);
        assert_eq!(ranges[0].tier, 1);
        assert_eq!(ranges[1].tier, 2);
        assert_eq!(ranges[1].target_tier(), 3);
    }

    #[test]
    fn a_consumed_block_is_not_offered_again() {
        let mut state = CompressionState::default();
        state.blocks.push(block("b1", 1, &"a summary ".repeat(200)));
        state.blocks.push(block("b2", 1, &"a summary ".repeat(200)));
        state.blocks[1].active = false;
        assert!(distillable_ranges(&state, &config()).is_empty());
    }

    #[test]
    fn the_distillation_nudge_carries_the_tier_two_rules_and_block_refs() {
        let mut state = CompressionState::default();
        for index in 1..=3 {
            state
                .blocks
                .push(block(&format!("b{index}"), 1, &"a summary ".repeat(200)));
        }
        let ranges = distillable_ranges(&state, &config());
        let text = render(Voice::Pressure, 80_000, 0, &config(), &[], &ranges, &state, false);

        // The tier's own rules, not the fold rules: telling the model to
        // summarise a summary is how a distillation loses the decisions it was
        // supposed to keep.
        assert!(text.contains("TIER 2 COMPRESSION — DISTILLATION"));
        assert!(!text.contains("HOW TO COMPRESS"));
        assert!(text.contains("b1–b3"));
        assert!(text.contains("3 blocks"));
        assert!(text.contains("tier 1 → 2"));
    }

    #[test]
    fn a_tier_three_distillation_carries_the_condensation_rules() {
        let mut state = CompressionState::default();
        for index in 1..=3 {
            state
                .blocks
                .push(block(&format!("b{index}"), 2, &"a summary ".repeat(200)));
        }
        let ranges = distillable_ranges(&state, &config());
        let text = render(Voice::Pressure, 80_000, 0, &config(), &[], &ranges, &state, false);
        assert!(text.contains("TIER 3 COMPRESSION — ULTRA-CONDENSATION"));
        assert!(text.contains("tier 2 → 3"));
    }

    #[test]
    fn a_fold_nudge_still_carries_the_fold_rules() {
        let ranges = vec![CompressibleRange {
            start_ref: "m00010".into(),
            end_ref: "m00042".into(),
            count: 33,
            tokens: 48_000,
            tool_count: 20,
            user_count: 2,
        }];
        let text = render(
            Voice::Gentle,
            80_000,
            0,
            &config(),
            &ranges,
            &[],
            &CompressionState::default(),
            false,
        );
        assert!(text.contains("HOW TO COMPRESS"));
        assert!(!text.contains("TIER 2 COMPRESSION"));
    }

    #[test]
    fn the_block_map_lists_active_blocks() {
        let mut state = CompressionState::default();
        state.blocks.push(crate::capabilities::compact::types::CompressionBlock {
            block_id: "b1".into(),
            tier: 1,
            topic: None,
            summary: "s".into(),
            direct_message_ids: vec![],
            effective_message_ids: vec![],
            direct_block_ids: vec![],
            compressed_tokens: 0,
            created_at: 0,
            active: true,
            start_ref: "m00001".into(),
            end_ref: "m00020".into(),
        });
        let text = render(Voice::Gentle, 80_000, 0, &config(), &[], &[], &state, false);
        assert!(text.contains("Active blocks (1): b1=m00001–m00020"));
    }
}
