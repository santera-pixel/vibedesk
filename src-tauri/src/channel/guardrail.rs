//! The guardrail.
//!
//! This is not a security boundary. The agent already runs with the user's
//! permissions and can run shell commands, so it can already do anything. What
//! this stops is the accident: a test that clicks Approve corrupts its own run,
//! and a test that clicks Delete destroys state the next test needs.
//!
//! Two checks, both deterministic, both before dispatch. Scope is spatial and
//! cheap. Destructive patterns look at the sequence rather than the action,
//! because intent lives in the sequence — one click on Delete is fine, Delete
//! followed by Confirm is not, and no single coordinate can tell them apart.
//!
//! It fails closed. An action it cannot evaluate is blocked, because a blocked
//! test is recoverable and a corrupted app is not.

use serde::Serialize;

/// A rectangle in CSS pixels.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x <= self.x + self.w && y >= self.y && y <= self.y + self.h
    }

    /// Whether `other` sits entirely inside this rectangle.
    pub fn contains_rect(&self, other: &Rect) -> bool {
        other.x >= self.x
            && other.y >= self.y
            && other.x + other.w <= self.x + self.w
            && other.y + other.h <= self.y + self.h
    }
}

/// Which rule refused an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Rule {
    Scope,
    Destructive,
    Unevaluable,
}

/// The outcome of a check.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "verdict", rename_all = "lowercase")]
pub enum Verdict {
    Allow,
    Block { rule: Rule, detail: String },
}

impl Verdict {
    pub fn is_allowed(&self) -> bool {
        matches!(self, Verdict::Allow)
    }
}

/// Labels that change state the next test depends on.
const DESTRUCTIVE: &[&str] = &[
    "delete",
    "remove",
    "discard",
    "uninstall",
    "approve",
    "install",
    "pay",
    "purchase",
    "reset",
    "clear",
    "erase",
    "destroy",
    "revoke",
];

/// Labels that answer a destructive prompt.
const CONFIRM: &[&str] = &[
    "confirm",
    "yes",
    "ok",
    "okay",
    "continue",
    "proceed",
    "delete",
    "remove",
    "discard",
    "reset",
    "clear",
];

/// Split a label into lowercase words.
///
/// Whole-word matching rather than substring, so "Undelete" does not read as
/// "delete" and "Cancel" does not read as "clear".
fn words(label: &str) -> Vec<String> {
    label
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(|word| word.to_lowercase())
        .collect()
}

fn matches_any(label: &str, set: &[&str]) -> bool {
    words(label).iter().any(|word| set.contains(&word.as_str()))
}

/// The first destructive label and the confirm that follows it, if any.
///
/// Returns positions into `labels`, which is one entry per action in the
/// sequence. A `None` label is an action whose target had no accessible name —
/// it cannot be judged, so it neither triggers nor satisfies the pattern.
pub fn destructive_pair(labels: &[Option<String>]) -> Option<(usize, usize)> {
    let first = labels.iter().position(|label| {
        label
            .as_deref()
            .map(|text| matches_any(text, DESTRUCTIVE))
            .unwrap_or(false)
    })?;

    let second = labels
        .iter()
        .enumerate()
        .skip(first + 1)
        .find(|(_, label)| {
            label
                .as_deref()
                .map(|text| matches_any(text, CONFIRM))
                .unwrap_or(false)
        })
        .map(|(index, _)| index)?;

    Some((first, second))
}

/// Whether a point lands inside the stage.
pub fn check_point(scope: &Rect, x: f64, y: f64) -> Verdict {
    if scope.contains(x, y) {
        return Verdict::Allow;
    }
    Verdict::Block {
        rule: Rule::Scope,
        detail: format!("({x}, {y}) is outside the stage"),
    }
}

/// Whether a key event has somewhere inside the stage to land.
///
/// A key event goes to whatever holds focus, and focus can be anywhere on the
/// page. A backend that finds nothing focused reports the document body, which
/// is the whole page — so a stage that is the whole page passes and a stage that
/// is a sub-region does not. `None` therefore means the backend could not tell,
/// and that blocks rather than assuming the stage has focus.
pub fn check_focus(scope: &Rect, focused: Option<&Rect>) -> Verdict {
    match focused {
        Some(rect) if scope.contains_rect(rect) => Verdict::Allow,
        Some(rect) => Verdict::Block {
            rule: Rule::Scope,
            detail: format!(
                "the focused element at ({}, {}) straddles the stage edge",
                rect.x, rect.y
            ),
        },
        None => Verdict::Block {
            rule: Rule::Unevaluable,
            detail: "nothing is focused, so a key event has no target inside the stage".into(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stage() -> Rect {
        Rect {
            x: 0.0,
            y: 0.0,
            w: 800.0,
            h: 600.0,
        }
    }

    fn labels(items: &[&str]) -> Vec<Option<String>> {
        items.iter().map(|item| Some(item.to_string())).collect()
    }

    #[test]
    fn a_point_inside_the_stage_is_allowed() {
        assert!(check_point(&stage(), 400.0, 300.0).is_allowed());
    }

    #[test]
    fn a_point_on_the_edge_is_inside() {
        assert!(check_point(&stage(), 0.0, 0.0).is_allowed());
        assert!(check_point(&stage(), 800.0, 600.0).is_allowed());
    }

    #[test]
    fn a_point_outside_the_stage_is_blocked() {
        let verdict = check_point(&stage(), 900.0, 300.0);
        assert!(!verdict.is_allowed());
        match verdict {
            Verdict::Block { rule, .. } => assert_eq!(rule, Rule::Scope),
            Verdict::Allow => panic!("expected a block"),
        }
    }

    #[test]
    fn a_rect_fully_inside_is_allowed() {
        let inner = Rect {
            x: 10.0,
            y: 10.0,
            w: 100.0,
            h: 40.0,
        };
        assert!(check_focus(&stage(), Some(&inner)).is_allowed());
    }

    #[test]
    fn a_rect_straddling_the_edge_is_blocked() {
        let straddling = Rect {
            x: 750.0,
            y: 10.0,
            w: 100.0,
            h: 40.0,
        };
        assert!(!check_focus(&stage(), Some(&straddling)).is_allowed());
    }

    #[test]
    fn a_backend_that_cannot_report_focus_blocks() {
        let verdict = check_focus(&stage(), None);
        assert!(!verdict.is_allowed());
        match verdict {
            Verdict::Block { rule, .. } => assert_eq!(rule, Rule::Unevaluable),
            Verdict::Allow => panic!("expected a block"),
        }
    }

    #[test]
    fn delete_then_confirm_is_a_destructive_pair() {
        let sequence = labels(&["Settings", "Delete account", "Confirm"]);
        assert_eq!(destructive_pair(&sequence), Some((1, 2)));
    }

    #[test]
    fn a_destructive_label_alone_is_not_a_pair() {
        let sequence = labels(&["Settings", "Delete account"]);
        assert_eq!(destructive_pair(&sequence), None);
    }

    #[test]
    fn a_confirm_before_the_destructive_label_is_not_a_pair() {
        let sequence = labels(&["OK", "Delete account"]);
        assert_eq!(destructive_pair(&sequence), None);
    }

    #[test]
    fn undelete_does_not_read_as_delete() {
        let sequence = labels(&["Undelete", "Confirm"]);
        assert_eq!(destructive_pair(&sequence), None);
    }

    #[test]
    fn an_unlabelled_action_neither_triggers_nor_satisfies() {
        let sequence = vec![Some("Delete account".to_string()), None, Some("Confirm".to_string())];
        assert_eq!(destructive_pair(&sequence), Some((0, 2)));
    }
}
