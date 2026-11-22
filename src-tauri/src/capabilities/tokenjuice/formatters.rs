//! Per-command formatters that go beyond what a rule's declarative fields can
//! express.
//!
//! Ported from tokenjuice's `src/core/reduce-formatters.ts`. Only the `gh`
//! formatter is here, because it is the one the rule set depends on for
//! correctness rather than polish: `gh --json` prints one JSON object per line,
//! and a rule that only filters and truncates leaves the agent reading raw JSON
//! keys. The formatter turns each record into the line a human would have
//! written.
//!
//! The `git diff`, `git status` and `rg` formatters upstream are not ported.
//! Their rules already filter and truncate correctly; the formatters only
//! rewrite the surviving lines into a denser shape. That is a real gap, and it
//! is recorded in `docs/PLAN-capabilities.md` rather than papered over.

use serde_json::Value;

/// Collapse runs of whitespace to one space and trim.
pub fn compact_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn get_string(record: &Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(value) = record.get(*key).and_then(Value::as_str) {
            if !value.trim().is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

fn label_names(value: Option<&Value>) -> Vec<String> {
    let Some(Value::Array(items)) = value else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|entry| match entry {
            Value::String(name) if !name.is_empty() => Some(name.clone()),
            Value::Object(map) => map
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
                .map(str::to_string),
            _ => None,
        })
        .collect()
}

fn comment_count(value: Option<&Value>) -> Option<u64> {
    match value {
        Some(Value::Number(number)) => number.as_u64(),
        Some(Value::Array(items)) => Some(items.len() as u64),
        Some(Value::Object(map)) => map.get("totalCount").and_then(Value::as_u64),
        _ => None,
    }
}

fn format_duration(seconds: f64) -> String {
    if !seconds.is_finite() || seconds < 0.0 {
        return String::new();
    }
    if seconds < 60.0 {
        return format!("{}s", seconds.round() as u64);
    }
    let hours = (seconds / 3600.0).floor() as u64;
    let minutes = ((seconds % 3600.0) / 60.0).floor() as u64;
    let remaining = (seconds % 60.0).round() as u64;
    if hours > 0 {
        format!("{hours}h{minutes:02}m")
    } else {
        format!("{minutes}m{remaining:02}s")
    }
}

fn parse_iso(value: Option<&Value>) -> Option<i64> {
    let text = value?.as_str()?;
    chrono::DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|stamp| stamp.timestamp())
}

fn duration_of(record: &Value) -> Option<String> {
    for key in ["durationSec", "durationSeconds"] {
        if let Some(seconds) = record.get(key).and_then(Value::as_f64) {
            if seconds >= 0.0 {
                return Some(format_duration(seconds));
            }
        }
    }
    for (start, end) in [
        ("startedAt", "completedAt"),
        ("createdAt", "updatedAt"),
    ] {
        if let (Some(from), Some(to)) = (parse_iso(record.get(start)), parse_iso(record.get(end))) {
            if to >= from {
                return Some(format_duration((to - from) as f64));
            }
        }
    }
    None
}

/// One `gh --json` record as a single line.
///
/// Returns `None` when the record has no title-like field, because a line with
/// only an id is not worth the tokens.
fn format_record(record: &Value) -> Option<String> {
    let numeric_id = record
        .get("number")
        .and_then(Value::as_u64)
        .or_else(|| record.get("databaseId").and_then(Value::as_u64));

    let title = get_string(
        record,
        &["title", "displayTitle", "name", "workflowName"],
    )?;

    let labels = label_names(record.get("labels"));
    let comments = comment_count(record.get("comments"));
    let branch = get_string(record, &["headBranch", "headRefName"]);
    let status = get_string(record, &["state", "conclusion", "status"]);
    let updated_at = record
        .get("updatedAt")
        .and_then(Value::as_str)
        .map(|stamp| stamp.chars().take(10).collect::<String>());
    let duration = duration_of(record);

    let mut parts: Vec<String> = Vec::new();
    if let Some(id) = numeric_id {
        parts.push(format!("#{id}"));
    }
    parts.push(compact_whitespace(&title));
    if let Some(status) = status {
        parts.push(format!("[{}]", compact_whitespace(&status)));
    }
    if let Some(branch) = branch {
        parts.push(format!("({})", compact_whitespace(&branch)));
    }
    if let Some(duration) = duration {
        parts.push(duration);
    }
    if let Some(count) = comments.filter(|count| *count > 0) {
        parts.push(format!("{count}c"));
    }
    if !labels.is_empty() {
        // Three labels is the point where the line stops being scannable.
        let shown: Vec<String> = labels.iter().take(3).cloned().collect();
        parts.push(format!("{{{}}}", shown.join(", ")));
    }
    if let Some(updated_at) = updated_at {
        parts.push(updated_at);
    }

    Some(parts.join(" "))
}

/// Rewrite a `gh` output block, whether it is one JSON document, one JSON
/// object per line, or neither.
pub fn rewrite_gh_lines(lines: &[String]) -> Option<Vec<String>> {
    let non_empty: Vec<&String> = lines.iter().filter(|line| !line.trim().is_empty()).collect();
    if non_empty.is_empty() {
        return Some(Vec::new());
    }

    // A single JSON document: an array of records, or one record.
    let joined = non_empty
        .iter()
        .map(|line| line.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let trimmed = joined.trim();
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
            let rewritten = match &value {
                Value::Array(items) => items.iter().filter_map(format_record).collect::<Vec<_>>(),
                Value::Object(_) => format_record(&value).into_iter().collect::<Vec<_>>(),
                _ => Vec::new(),
            };
            if !rewritten.is_empty() {
                return Some(rewritten);
            }
        }
    }

    // One JSON object per line, which is what `gh --json` actually prints.
    let parsed: Vec<Option<Value>> = non_empty
        .iter()
        .map(|line| {
            let trimmed = line.trim();
            if !trimmed.starts_with('{') || !trimmed.ends_with('}') {
                return None;
            }
            serde_json::from_str::<Value>(trimmed).ok()
        })
        .collect();
    if parsed.iter().all(Option::is_some) {
        let rewritten: Vec<String> = parsed
            .iter()
            .flatten()
            .filter_map(format_record)
            .collect();
        if !rewritten.is_empty() {
            return Some(rewritten);
        }
    }

    // Not JSON: leave the lines for the rule's own filters.
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_string).collect()
    }

    #[test]
    fn a_json_record_becomes_one_readable_line() {
        let input = lines(
            r#"{"number":67473,"title":"[Bug]: Auto-compaction never fires","url":"https://x/67473","comments":0,"updatedAt":"2026-04-16T01:52:50Z","labels":["bug","bug:behavior"]}"#,
        );
        let out = rewrite_gh_lines(&input).expect("a JSON record is rewritten");
        assert_eq!(out.len(), 1);
        assert!(out[0].starts_with("#67473 "), "{}", out[0]);
        assert!(out[0].contains("[Bug]: Auto-compaction never fires"));
        assert!(out[0].contains("{bug, bug:behavior}"));
        assert!(out[0].contains("2026-04-16"));
        // The raw JSON keys are gone, which is the whole point.
        assert!(!out[0].contains("\"number\""));
    }

    #[test]
    fn a_json_array_becomes_one_line_per_record() {
        let input = lines(
            r#"[{"number":1,"title":"first"},{"number":2,"title":"second"}]"#,
        );
        let out = rewrite_gh_lines(&input).unwrap();
        assert_eq!(out, vec!["#1 first", "#2 second"]);
    }

    #[test]
    fn a_comment_count_is_shown_only_when_it_is_not_zero() {
        let none = rewrite_gh_lines(&lines(r#"{"number":1,"title":"t","comments":0}"#)).unwrap();
        assert_eq!(none, vec!["#1 t"]);
        let some = rewrite_gh_lines(&lines(r#"{"number":1,"title":"t","comments":3}"#)).unwrap();
        assert_eq!(some, vec!["#1 t 3c"]);
        // An array of comments counts by length.
        let array =
            rewrite_gh_lines(&lines(r#"{"number":1,"title":"t","comments":[{},{}]}"#)).unwrap();
        assert_eq!(array, vec!["#1 t 2c"]);
    }

    #[test]
    fn more_than_three_labels_are_dropped() {
        let out = rewrite_gh_lines(&lines(
            r#"{"number":1,"title":"t","labels":["a","b","c","d","e"]}"#,
        ))
        .unwrap();
        assert_eq!(out, vec!["#1 t {a, b, c}"]);
    }

    #[test]
    fn a_record_without_a_title_is_skipped() {
        assert!(rewrite_gh_lines(&lines(r#"{"number":1}"#)).is_none());
    }

    #[test]
    fn non_json_output_is_left_for_the_rule() {
        assert!(rewrite_gh_lines(&lines("some plain text\nmore text")).is_none());
    }

    #[test]
    fn durations_are_formatted_the_way_a_human_writes_them() {
        assert_eq!(format_duration(45.0), "45s");
        assert_eq!(format_duration(90.0), "1m30s");
        assert_eq!(format_duration(3700.0), "1h01m");
        assert_eq!(format_duration(-1.0), "");
    }

    #[test]
    fn a_duration_is_derived_from_timestamps_when_there_is_no_field() {
        let out = rewrite_gh_lines(&lines(
            r#"{"number":1,"title":"t","startedAt":"2026-04-16T01:00:00Z","completedAt":"2026-04-16T01:02:30Z"}"#,
        ))
        .unwrap();
        assert!(out[0].contains("2m30s"), "{}", out[0]);
    }
}
