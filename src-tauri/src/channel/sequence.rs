//! Running a sequence.
//!
//! The unit of work is the sequence, not the action. One tool call that carries
//! forty steps costs one agent turn; forty tool calls cost forty, and a turn is
//! three orders of magnitude more expensive than the CDP call inside it.
//!
//! Two rules make a sequence safe to send blind. It stops at the first failure,
//! because the steps after a failed step were written assuming it worked and
//! running them turns one wrong click into a cascade. And the guardrail judges
//! the sequence as a whole before any of it runs, because intent lives in the
//! sequence — Delete then Confirm is a different act from Delete alone, and no
//! single coordinate can tell them apart.

use std::time::Instant;

use crate::channel::backend::{Action, ActionResult, Backend, TargetRef};
use crate::channel::guardrail::{self, Rect, Rule, Verdict};
use crate::channel::issues::{Issue, Level};

/// How many issues a single result carries before it says "and N more".
const ISSUE_CAP: usize = 10;

/// What a sequence did.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub results: Vec<ActionResult>,
    /// The index of the step that failed, if one did.
    pub stopped_at: Option<usize>,
    /// Set when the guardrail refused the sequence before it ran.
    pub blocked: Option<Verdict>,
    /// What broke while it ran.
    pub issues: Vec<Issue>,
    pub more_issues: usize,
}

impl Outcome {
    pub fn ok(&self) -> bool {
        self.blocked.is_none() && self.stopped_at.is_none()
    }
}

/// Run every step, stopping at the first one that fails or is refused.
pub fn run(
    backend: &mut dyn Backend,
    actions: &[Action],
    scope: &Rect,
    allow_destructive: bool,
) -> Outcome {
    if !allow_destructive {
        if let Some(verdict) = destructive_verdict(backend, actions) {
            return Outcome {
                results: Vec::new(),
                stopped_at: None,
                blocked: Some(verdict),
                issues: Vec::new(),
                more_issues: 0,
            };
        }
    }

    let capabilities = backend.capabilities();
    let mut results = Vec::with_capacity(actions.len());
    let mut stopped_at = None;

    for (index, action) in actions.iter().enumerate() {
        let kind = action.kind();

        if !capabilities.supports(kind) {
            results.push(ActionResult::failed(
                kind,
                format!(
                    "the {:?} backend does not support {kind:?}",
                    capabilities.backend
                ),
                0,
            ));
            stopped_at = Some(index);
            break;
        }

        if let Verdict::Block { rule, detail } = check(backend, action, scope) {
            results.push(ActionResult::failed(
                kind,
                format!("refused by the {rule:?} rule: {detail}"),
                0,
            ));
            stopped_at = Some(index);
            break;
        }

        let started = Instant::now();
        match backend.act(action) {
            Ok(result) => {
                let ok = result.ok;
                results.push(result);
                if !ok {
                    stopped_at = Some(index);
                    break;
                }
            }
            Err(error) => {
                results.push(ActionResult::failed(
                    kind,
                    error.to_string(),
                    started.elapsed().as_millis() as u64,
                ));
                stopped_at = Some(index);
                break;
            }
        }
    }

    let (issues, more_issues) = drain(backend);
    Outcome {
        results,
        stopped_at,
        blocked: None,
        issues,
        more_issues,
    }
}

/// Judge one step before it is dispatched.
fn check(backend: &dyn Backend, action: &Action, scope: &Rect) -> Verdict {
    match action {
        Action::Click { target, .. } | Action::Read { target } => point_check(backend, target, scope),
        Action::Drag { from, to } => {
            let first = point_check(backend, from, scope);
            if !first.is_allowed() {
                return first;
            }
            point_check(backend, to, scope)
        }
        Action::Type { target, .. } => match target {
            Some(target) => rect_check(backend, target, scope),
            None => guardrail::check_focus(scope, backend.focused().as_ref()),
        },
        Action::Hold { .. } => guardrail::check_focus(scope, backend.focused().as_ref()),
        Action::Scroll { target, .. } => match target {
            Some(target) => point_check(backend, target, scope),
            // A scroll with no target goes to the stage centre, which is inside
            // the stage by definition.
            None => Verdict::Allow,
        },
        Action::Tick { .. } | Action::Wait { .. } | Action::Run { .. } => Verdict::Allow,
    }
}

fn point_check(backend: &dyn Backend, target: &TargetRef, scope: &Rect) -> Verdict {
    match point_of(backend, target) {
        Some((x, y)) => guardrail::check_point(scope, x, y),
        None => Verdict::Block {
            rule: Rule::Unevaluable,
            detail: unresolved(target),
        },
    }
}

fn rect_check(backend: &dyn Backend, target: &TargetRef, scope: &Rect) -> Verdict {
    match rect_of(backend, target) {
        Some(rect) => guardrail::check_focus(scope, Some(&rect)),
        None => Verdict::Block {
            rule: Rule::Unevaluable,
            detail: unresolved(target),
        },
    }
}

/// What to say when a target does not resolve.
///
/// A target is a ref id or a point. A label is neither, and an agent that passes
/// one is told to look again — which it does, gets the same refs, passes the
/// same label, and loops. So the refusal names the form it wanted rather than
/// only the thing it could not find.
fn unresolved(target: &TargetRef) -> String {
    format!(
        "{} did not resolve — a target is a ref id from the last look (e1, e2, …) or a point,          not a label. Look, then use the ref it gives you.",
        target.describe()
    )
}

fn point_of(backend: &dyn Backend, target: &TargetRef) -> Option<(f64, f64)> {
    match target {
        TargetRef::Point { x, y } => Some((*x, *y)),
        TargetRef::Named(_) => backend.resolve(target).map(|found| found.centre()),
    }
}

fn rect_of(backend: &dyn Backend, target: &TargetRef) -> Option<Rect> {
    match target {
        TargetRef::Point { x, y } => Some(Rect {
            x: *x,
            y: *y,
            w: 0.0,
            h: 0.0,
        }),
        TargetRef::Named(_) => backend.resolve(target).map(|found| found.rect),
    }
}

/// Refuse a sequence that deletes and then confirms.
fn destructive_verdict(backend: &dyn Backend, actions: &[Action]) -> Option<Verdict> {
    let labels: Vec<Option<String>> = actions
        .iter()
        .map(|action| {
            action
                .target()
                .and_then(|target| backend.resolve(target))
                .map(|found| found.label)
        })
        .collect();

    let (first, second) = guardrail::destructive_pair(&labels)?;
    Some(Verdict::Block {
        rule: Rule::Destructive,
        detail: format!(
            "step {} clicks \"{}\" and step {} confirms with \"{}\" — pass allow_destructive to run it",
            first + 1,
            labels[first].as_deref().unwrap_or(""),
            second + 1,
            labels[second].as_deref().unwrap_or(""),
        ),
    })
}

fn drain(backend: &mut dyn Backend) -> (Vec<Issue>, usize) {
    match backend.issues() {
        Ok(mut issues) => {
            let total = issues.len();
            if total > ISSUE_CAP {
                issues.truncate(ISSUE_CAP);
                return (issues, total - ISSUE_CAP);
            }
            (issues, 0)
        }
        Err(error) => (
            vec![Issue::note(
                Level::Warn,
                "channeler",
                format!("could not read issues: {error}"),
            )],
            0,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::backend::{
        ActionKind, BackendKind, Capabilities, HostKind, InputKind, Look, Target,
    };
    use std::collections::HashMap;

    struct Fake {
        targets: HashMap<String, Target>,
        focused: Option<Rect>,
        fail_on: Option<ActionKind>,
        pending: Vec<Issue>,
        acted: Vec<ActionKind>,
    }

    impl Fake {
        fn new() -> Self {
            let mut targets = HashMap::new();
            targets.insert(
                "e1".into(),
                Target {
                    id: "e1".into(),
                    role: "button".into(),
                    label: "Delete account".into(),
                    rect: Rect {
                        x: 10.0,
                        y: 10.0,
                        w: 100.0,
                        h: 30.0,
                    },
                },
            );
            targets.insert(
                "e2".into(),
                Target {
                    id: "e2".into(),
                    role: "button".into(),
                    label: "Confirm".into(),
                    rect: Rect {
                        x: 10.0,
                        y: 60.0,
                        w: 100.0,
                        h: 30.0,
                    },
                },
            );
            targets.insert(
                "e3".into(),
                Target {
                    id: "e3".into(),
                    role: "button".into(),
                    label: "Play".into(),
                    rect: Rect {
                        x: 10.0,
                        y: 110.0,
                        w: 100.0,
                        h: 30.0,
                    },
                },
            );
            Self {
                targets,
                focused: Some(Rect {
                    x: 0.0,
                    y: 0.0,
                    w: 800.0,
                    h: 600.0,
                }),
                fail_on: None,
                pending: Vec::new(),
                acted: Vec::new(),
            }
        }
    }

    impl Backend for Fake {
        fn capabilities(&self) -> Capabilities {
            Capabilities {
                backend: BackendKind::Webview,
                host: HostKind::Desktop,
                foreground: false,
                input: InputKind::Synthetic,
                actions: vec![
                    ActionKind::Click,
                    ActionKind::Hold,
                    ActionKind::Drag,
                    ActionKind::Type,
                    ActionKind::Scroll,
                    ActionKind::Tick,
                    ActionKind::Wait,
                    ActionKind::Read,
                ],
                capture: true,
                issues: true,
            }
        }

        fn look(&mut self, _capture: bool) -> Result<Look, crate::error::AppError> {
            Ok(Look {
                digest: String::new(),
                stage: Rect {
                    x: 0.0,
                    y: 0.0,
                    w: 800.0,
                    h: 600.0,
                },
                targets: self.targets.values().cloned().collect(),
                capture: None,
                capture_type: None,
            })
        }

        fn act(&mut self, action: &Action) -> Result<ActionResult, crate::error::AppError> {
            let kind = action.kind();
            self.acted.push(kind);
            if self.fail_on == Some(kind) {
                return Ok(ActionResult::failed(kind, "the element moved", 1));
            }
            Ok(ActionResult::ok(kind, "done", 1))
        }

        fn issues(&mut self) -> Result<Vec<Issue>, crate::error::AppError> {
            Ok(std::mem::take(&mut self.pending))
        }

        fn resolve(&self, target: &TargetRef) -> Option<Target> {
            match target {
                TargetRef::Named(id) => self.targets.get(id).cloned(),
                TargetRef::Point { .. } => None,
            }
        }

        fn focused(&self) -> Option<Rect> {
            self.focused
        }

        fn stage(&self) -> Rect {
            stage()
        }

        fn close(&mut self) -> Result<(), crate::error::AppError> {
            Ok(())
        }
    }

    fn stage() -> Rect {
        Rect {
            x: 0.0,
            y: 0.0,
            w: 800.0,
            h: 600.0,
        }
    }

    fn click(id: &str) -> Action {
        Action::Click {
            target: TargetRef::Named(id.into()),
            button: Default::default(),
            count: 1,
        }
    }

    #[test]
    fn every_step_runs_in_order() {
        let mut backend = Fake::new();
        let actions = vec![click("e3"), Action::Tick { frames: 30 }, click("e3")];

        let outcome = run(&mut backend, &actions, &stage(), false);

        assert!(outcome.ok());
        assert_eq!(
            backend.acted,
            vec![ActionKind::Click, ActionKind::Tick, ActionKind::Click]
        );
        assert_eq!(outcome.results.len(), 3);
    }

    #[test]
    fn a_failing_step_stops_the_sequence() {
        let mut backend = Fake::new();
        backend.fail_on = Some(ActionKind::Click);
        let actions = vec![click("e3"), click("e3"), Action::Tick { frames: 1 }];

        let outcome = run(&mut backend, &actions, &stage(), false);

        assert!(!outcome.ok());
        assert_eq!(outcome.stopped_at, Some(0));
        assert_eq!(backend.acted.len(), 1, "the steps after the failure did not run");
    }

    #[test]
    fn a_stale_ref_is_refused_before_dispatch() {
        let mut backend = Fake::new();
        let actions = vec![click("e99")];

        let outcome = run(&mut backend, &actions, &stage(), false);

        assert!(!outcome.ok());
        assert_eq!(outcome.stopped_at, Some(0));
        assert!(backend.acted.is_empty(), "nothing was dispatched");
        assert!(outcome.results[0].detail.contains("did not resolve"));
    }

    #[test]
    fn a_point_outside_the_stage_is_refused() {
        let mut backend = Fake::new();
        let actions = vec![Action::Click {
            target: TargetRef::Point { x: 900.0, y: 300.0 },
            button: Default::default(),
            count: 1,
        }];

        let outcome = run(&mut backend, &actions, &stage(), false);

        assert!(!outcome.ok());
        assert!(backend.acted.is_empty());
        assert!(outcome.results[0].detail.contains("Scope"));
    }

    #[test]
    fn delete_then_confirm_is_refused_unless_allowed() {
        let mut backend = Fake::new();
        let actions = vec![click("e1"), click("e2")];

        let refused = run(&mut backend, &actions, &stage(), false);
        assert!(!refused.ok());
        assert!(refused.blocked.is_some());
        assert!(backend.acted.is_empty(), "the whole sequence was refused");

        let allowed = run(&mut backend, &actions, &stage(), true);
        assert!(allowed.ok());
        assert_eq!(backend.acted.len(), 2);
    }

    #[test]
    fn delete_alone_is_not_refused() {
        let mut backend = Fake::new();
        let outcome = run(&mut backend, &vec![click("e1")], &stage(), false);
        assert!(outcome.ok());
    }

    #[test]
    fn issues_are_drained_into_the_outcome() {
        let mut backend = Fake::new();
        backend.pending = vec![Issue::note(Level::Error, "console", "boom")];

        let outcome = run(&mut backend, &vec![click("e3")], &stage(), false);

        assert_eq!(outcome.issues.len(), 1);
        assert_eq!(outcome.issues[0].text, "boom");
        assert!(backend.pending.is_empty(), "the buffer was drained");
    }

    #[test]
    fn an_unsupported_action_is_refused_with_the_backend_named() {
        struct Limited(Fake);

        impl Backend for Limited {
            fn capabilities(&self) -> Capabilities {
                let mut capabilities = self.0.capabilities();
                capabilities.actions = vec![ActionKind::Click];
                capabilities
            }
            fn look(&mut self, capture: bool) -> Result<Look, crate::error::AppError> {
                self.0.look(capture)
            }
            fn act(&mut self, action: &Action) -> Result<ActionResult, crate::error::AppError> {
                self.0.act(action)
            }
            fn issues(&mut self) -> Result<Vec<Issue>, crate::error::AppError> {
                self.0.issues()
            }
            fn resolve(&self, target: &TargetRef) -> Option<Target> {
                self.0.resolve(target)
            }
            fn focused(&self) -> Option<Rect> {
                self.0.focused()
            }
            fn stage(&self) -> Rect {
                self.0.stage()
            }
            fn close(&mut self) -> Result<(), crate::error::AppError> {
                self.0.close()
            }
        }

        let mut backend = Limited(Fake::new());
        let outcome = run(&mut backend, &vec![Action::Tick { frames: 1 }], &stage(), false);

        assert!(!outcome.ok());
        assert!(outcome.results[0].detail.contains("does not support"));
    }
}
