//! Stable message refs.
//!
//! Ported from billion-context's `kernel/src/refs.ts`. A ref is the handle the
//! model cites when it asks to fold a range, so it has to survive across turns:
//! the same message must keep the same `mNNNNN` from one request to the next.
//!
//! Upstream gets that stability from a server-side ledger sequence number. The
//! proxy has no ledger, so this port derives the raw id from the message's own
//! bytes — a stable FNV-1a hash of role, content type, tool identity and text,
//! plus an occurrence counter for byte-identical messages. Content addressing
//! has one property worth naming: a message that is edited in place gets a new
//! ref, which is correct, because it is a different message.

use std::collections::BTreeMap;

use super::types::{CoreMessage, MessageRefMap};

/// The ref a message at `index` (1-based) would get.
pub fn index_to_ref(index: usize) -> String {
    format!("m{index:05}")
}

/// Parse `m00123` back to `123`.
pub fn ref_to_index(reference: &str) -> Option<usize> {
    let digits = reference.strip_prefix('m')?;
    if digits.is_empty() || !digits.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// Parse `b12` back to `12`.
///
/// Blocks are citable for the same reason messages are: a distillation names a
/// run of summaries, and it has to name them the way the nudge showed them.
pub fn block_ref_to_index(reference: &str) -> Option<u64> {
    let digits = reference.strip_prefix('b')?;
    if digits.is_empty() || !digits.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// Whether a reference names a block rather than a message.
pub fn is_block_ref(reference: &str) -> bool {
    block_ref_to_index(reference).is_some()
}

/// A total order over both ref kinds, so a span can be sorted.
///
/// Messages and blocks are separate namespaces and never compare against each
/// other in practice — a range that mixes them is rejected before this is
/// asked — but a single key keeps the sorting code in one place.
pub fn ref_sort_key(reference: &str) -> Option<(char, u64)> {
    if let Some(index) = ref_to_index(reference) {
        return Some(('m', index as u64));
    }
    block_ref_to_index(reference).map(|index| ('b', index))
}

/// FNV-1a, 64-bit. Chosen because it is stable across Rust versions and
/// platforms: a persisted ref map must still resolve after an upgrade.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// The content-addressed base id of a message, before occurrence disambiguation.
fn base_id(message: &CoreMessage) -> String {
    let mut material = String::with_capacity(message.text.len() + 32);
    material.push_str(message.role.as_str());
    material.push('\u{1f}');
    material.push_str(match message.content_type {
        super::types::ContentType::Text => "text",
        super::types::ContentType::ToolCall => "tool-call",
        super::types::ContentType::ToolResult => "tool-result",
        super::types::ContentType::Reasoning => "reasoning",
    });
    material.push('\u{1f}');
    material.push_str(message.tool_name.as_deref().unwrap_or(""));
    material.push('\u{1f}');
    material.push_str(message.tool_call_id.as_deref().unwrap_or(""));
    material.push('\u{1f}');
    material.push_str(&message.text);
    format!("{:016x}", fnv1a(material.as_bytes()))
}

/// The highest ref index already handed out.
pub fn highest_used_index(map: &MessageRefMap) -> usize {
    map.by_ref
        .keys()
        .filter_map(|reference| ref_to_index(reference))
        .max()
        .unwrap_or(0)
}

/// Give every message its raw id and its ref, minting refs for new messages.
///
/// Existing messages keep their ref. New ones are appended in order, so the
/// numbering is monotonic and a ref never changes meaning.
pub fn assign_refs(messages: &mut [CoreMessage], map: &mut MessageRefMap) {
    let mut occurrences: BTreeMap<String, usize> = BTreeMap::new();
    let mut next = highest_used_index(map) + 1;

    for message in messages.iter_mut() {
        let base = base_id(message);
        let occurrence = occurrences.entry(base.clone()).or_insert(0);
        let id = format!("{base}#{occurrence}");
        *occurrence += 1;
        message.id = id.clone();

        if !map.by_raw.contains_key(&id) {
            let reference = index_to_ref(next);
            next += 1;
            map.by_raw.insert(id.clone(), reference.clone());
            map.by_ref.insert(reference, id);
        }
    }
}

/// The ref for a raw id, if it has one.
pub fn ref_for_raw<'a>(map: &'a MessageRefMap, raw: &str) -> Option<&'a str> {
    map.by_raw.get(raw).map(String::as_str)
}

/// The raw id for a ref, if it has one.
pub fn raw_for_ref<'a>(map: &'a MessageRefMap, reference: &str) -> Option<&'a str> {
    map.by_ref.get(reference).map(String::as_str)
}

/// Order two refs so `start` is never after `end`.
pub fn ordered_ref_pair(a: &str, b: &str) -> (String, String) {
    match (ref_sort_key(a), ref_sort_key(b)) {
        (Some(left), Some(right)) if left > right => (b.to_string(), a.to_string()),
        _ => (a.to_string(), b.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::compact::types::Role;

    fn message(text: &str) -> CoreMessage {
        CoreMessage::text(Role::User, text)
    }

    #[test]
    fn a_ref_is_five_digits_so_it_sorts_lexically() {
        assert_eq!(index_to_ref(1), "m00001");
        assert_eq!(index_to_ref(150), "m00150");
        assert_eq!(index_to_ref(99_999), "m99999");
    }

    #[test]
    fn a_ref_round_trips_through_its_index() {
        assert_eq!(ref_to_index("m00150"), Some(150));
        assert_eq!(ref_to_index("m1"), Some(1));
        assert_eq!(ref_to_index("b1"), None);
        assert_eq!(ref_to_index("m"), None);
        assert_eq!(ref_to_index("mabc"), None);
    }

    #[test]
    fn refs_are_assigned_in_order() {
        let mut messages = vec![message("one"), message("two"), message("three")];
        let mut map = MessageRefMap::default();
        assign_refs(&mut messages, &mut map);
        assert_eq!(map.by_raw[&messages[0].id], "m00001");
        assert_eq!(map.by_raw[&messages[1].id], "m00002");
        assert_eq!(map.by_raw[&messages[2].id], "m00003");
    }

    #[test]
    fn a_message_keeps_its_ref_when_the_conversation_grows() {
        let mut map = MessageRefMap::default();
        let mut first = vec![message("one"), message("two")];
        assign_refs(&mut first, &mut map);
        let original = map.by_raw[&first[0].id].clone();

        let mut second = vec![message("one"), message("two"), message("three")];
        assign_refs(&mut second, &mut map);
        assert_eq!(map.by_raw[&second[0].id], original);
        assert_eq!(map.by_raw[&second[2].id], "m00003");
    }

    #[test]
    fn byte_identical_messages_get_distinct_refs() {
        let mut messages = vec![message("ok"), message("ok"), message("ok")];
        let mut map = MessageRefMap::default();
        assign_refs(&mut messages, &mut map);
        let refs: Vec<&str> = messages
            .iter()
            .map(|message| map.by_raw[&message.id].as_str())
            .collect();
        assert_eq!(refs, vec!["m00001", "m00002", "m00003"]);
    }

    #[test]
    fn an_edited_message_gets_a_new_ref_because_it_is_a_different_message() {
        let mut map = MessageRefMap::default();
        let mut before = vec![message("draft")];
        assign_refs(&mut before, &mut map);
        let mut after = vec![message("final")];
        assign_refs(&mut after, &mut map);
        assert_ne!(map.by_raw[&before[0].id], map.by_raw[&after[0].id]);
    }

    #[test]
    fn a_ref_resolves_back_to_its_raw_id() {
        let mut messages = vec![message("one")];
        let mut map = MessageRefMap::default();
        assign_refs(&mut messages, &mut map);
        assert_eq!(raw_for_ref(&map, "m00001"), Some(messages[0].id.as_str()));
        assert_eq!(ref_for_raw(&map, &messages[0].id), Some("m00001"));
    }

    #[test]
    fn a_block_ref_is_told_apart_from_a_message_ref() {
        assert!(is_block_ref("b12"));
        assert!(!is_block_ref("m00012"));
        assert!(!is_block_ref("b"));
        assert!(!is_block_ref("bx"));
        assert_eq!(block_ref_to_index("b12"), Some(12));
    }

    #[test]
    fn block_refs_sort_by_their_number_not_their_text() {
        assert_eq!(
            ordered_ref_pair("b10", "b9"),
            ("b9".to_string(), "b10".to_string())
        );
    }

    #[test]
    fn a_reversed_pair_is_put_back_in_order() {
        assert_eq!(
            ordered_ref_pair("m00042", "m00010"),
            ("m00010".to_string(), "m00042".to_string())
        );
        assert_eq!(
            ordered_ref_pair("m00010", "m00042"),
            ("m00010".to_string(), "m00042".to_string())
        );
    }

    #[test]
    fn the_highest_index_is_the_last_ref_handed_out() {
        let mut messages = vec![message("a"), message("b"), message("c")];
        let mut map = MessageRefMap::default();
        assign_refs(&mut messages, &mut map);
        assert_eq!(highest_used_index(&map), 3);
    }
}
