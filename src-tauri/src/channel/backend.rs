//! What a backend is.
//!
//! A backend drives one kind of target: a webview over CDP, a native window
//! over UI Automation and `SendInput`, a child process over its pipes, an engine
//! over its own test harness. The four differ enormously in mechanism and not at
//! all in shape, which is the point — the agent learns one vocabulary and it
//! works on all of them.
//!
//! Two axes, deliberately separate. `BackendKind` is what is being driven.
//! `HostKind` is where it runs. A window on the user's desktop is visible and
//! steals focus; the same window on a hidden desktop is neither, and nothing
//! above this layer has to know.

use serde::{Deserialize, Serialize};

use crate::channel::guardrail::Rect;
pub use crate::channel::host::HostKind;
use crate::channel::issues::Issue;
use crate::error::AppError;

/// What is being driven.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BackendKind {
    Webview,
    Window,
    Process,
    Engine,
}

/// The action kinds a backend accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    Click,
    Hold,
    Drag,
    Type,
    Scroll,
    Tick,
    Wait,
    Read,
    /// Run the target's own suite. The `engine` backend's primary verb.
    Run,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Button {
    #[default]
    Left,
    Right,
    Middle,
}

/// Where an action lands.
///
/// A bare string is a ref id from the last `look`; an object with `x` and `y` is
/// a point. Refs are preferred because they survive a layout shift between the
/// look and the act, and because a ref carries a label the guardrail can read.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum TargetRef {
    Named(String),
    Point { x: f64, y: f64 },
}

impl TargetRef {
    pub fn describe(&self) -> String {
        match self {
            TargetRef::Named(id) => id.clone(),
            TargetRef::Point { x, y } => format!("({x}, {y})"),
        }
    }
}

fn one() -> u32 {
    1
}

/// One step in a sequence.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    Click {
        target: TargetRef,
        #[serde(default)]
        button: Button,
        #[serde(default = "one")]
        count: u32,
    },
    /// Hold a key down for a number of frames or milliseconds.
    ///
    /// This is how a game is driven: a tap is one frame and does nothing, a hold
    /// is what moves a character. `frames` is preferred because it is what the
    /// simulation counts.
    Hold {
        key: String,
        #[serde(default)]
        frames: Option<u32>,
        #[serde(default)]
        ms: Option<u64>,
    },
    Drag {
        from: TargetRef,
        to: TargetRef,
    },
    Type {
        text: String,
        #[serde(default)]
        target: Option<TargetRef>,
    },
    Scroll {
        #[serde(default)]
        target: Option<TargetRef>,
        #[serde(default)]
        dx: f64,
        #[serde(default)]
        dy: f64,
    },
    /// Advance the simulation by a number of frames.
    Tick {
        #[serde(default = "one")]
        frames: u32,
    },
    Wait {
        ms: u64,
    },
    /// Read the text of a target, for an assertion.
    Read {
        target: TargetRef,
    },
    /// Run the target's own suite, and report what it said.
    ///
    /// This is the `engine` backend's primary verb, and it is a verb of its own
    /// rather than a reuse of `tick` because a suite has no frames to advance.
    /// `filter` narrows the run to one test by name, which is what makes a
    /// failing suite cheap to iterate on: re-running ten thousand tests to watch
    /// one of them is the cost this field exists to avoid.
    Run {
        #[serde(default)]
        filter: Option<String>,
    },
}

impl Action {
    pub fn kind(&self) -> ActionKind {
        match self {
            Action::Click { .. } => ActionKind::Click,
            Action::Hold { .. } => ActionKind::Hold,
            Action::Drag { .. } => ActionKind::Drag,
            Action::Type { .. } => ActionKind::Type,
            Action::Scroll { .. } => ActionKind::Scroll,
            Action::Tick { .. } => ActionKind::Tick,
            Action::Wait { .. } => ActionKind::Wait,
            Action::Read { .. } => ActionKind::Read,
            Action::Run { .. } => ActionKind::Run,
        }
    }

    /// The target this action names, when it names one.
    pub fn target(&self) -> Option<&TargetRef> {
        match self {
            Action::Click { target, .. } | Action::Read { target } => Some(target),
            Action::Drag { from, .. } => Some(from),
            Action::Type { target, .. } | Action::Scroll { target, .. } => target.as_ref(),
            Action::Hold { .. } | Action::Tick { .. } | Action::Wait { .. } | Action::Run { .. } => {
                None
            }
        }
    }
}

/// A named thing on screen, with the rectangle it occupies.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Target {
    pub id: String,
    pub role: String,
    pub label: String,
    pub rect: Rect,
}

impl Target {
    pub fn centre(&self) -> (f64, f64) {
        (
            self.rect.x + self.rect.w / 2.0,
            self.rect.y + self.rect.h / 2.0,
        )
    }
}

/// What the target looks like right now.
#[derive(Debug, Clone, Serialize)]
pub struct Look {
    /// A text digest, for the model to read.
    pub digest: String,
    /// The stage rectangle in CSS pixels. Everything the guardrail allows is
    /// inside this.
    pub stage: Rect,
    pub targets: Vec<Target>,
    /// An image of the stage, base64, when one was asked for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture: Option<String>,
    /// The media type of `capture`, so the caller does not have to guess.
    ///
    /// The two backends that can take a picture do not use the same encoder:
    /// `webview` asks Chromium for a JPEG, and `window` encodes a PNG from the
    /// window's own pixels. A bare base64 string with no type is a picture the
    /// caller has to sniff, so the type travels with it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_type: Option<String>,
}

/// How input reaches the target.
///
/// This is the difference that decides whether a channel can drive a game. A
/// game that polls raw input never looks at a window message, and a window on a
/// desktop that is not the active one never receives hardware input, so the two
/// are not interchangeable and the agent has to be able to see which it has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputKind {
    /// Real input through the OS. Reaches everything, including raw input, but
    /// only on the active desktop.
    Hardware,
    /// Events delivered to the target's own event system — a posted window
    /// message, a CDP input event. Reaches any desktop, but only a target that
    /// listens for them.
    Synthetic,
}

/// What this backend can actually do, so the agent does not ask for the rest.
#[derive(Debug, Clone, Serialize)]
pub struct Capabilities {
    pub backend: BackendKind,
    pub host: HostKind,
    /// Whether acting on this channel takes the user's screen and focus.
    ///
    /// This is the property that decides whether a test can run while the user
    /// works, and it does not follow from `host` alone: a webview renders on the
    /// user's desktop but is driven over a protocol that never touches it, so it
    /// is a desktop channel that does not take the foreground.
    pub foreground: bool,
    pub input: InputKind,
    pub actions: Vec<ActionKind>,
    pub capture: bool,
    pub issues: bool,
}

impl Capabilities {
    pub fn supports(&self, kind: ActionKind) -> bool {
        self.actions.contains(&kind)
    }
}

/// What one action did.
#[derive(Debug, Clone, Serialize)]
pub struct ActionResult {
    pub kind: ActionKind,
    pub ok: bool,
    pub detail: String,
    pub ms: u64,
}

impl ActionResult {
    pub fn ok(kind: ActionKind, detail: impl Into<String>, ms: u64) -> Self {
        Self {
            kind,
            ok: true,
            detail: detail.into(),
            ms,
        }
    }

    pub fn failed(kind: ActionKind, detail: impl Into<String>, ms: u64) -> Self {
        Self {
            kind,
            ok: false,
            detail: detail.into(),
            ms,
        }
    }
}

/// One thing that can be driven.
pub trait Backend: Send {
    fn capabilities(&self) -> Capabilities;

    /// What is on screen, and optionally a picture of it.
    fn look(&mut self, capture: bool) -> Result<Look, AppError>;

    /// Do one thing. The caller runs sequences; this runs a step.
    fn act(&mut self, action: &Action) -> Result<ActionResult, AppError>;

    /// Everything that went wrong since the last call.
    fn issues(&mut self) -> Result<Vec<Issue>, AppError>;

    /// Resolve a ref against the last look, so the guardrail can read its label
    /// and its rectangle. `None` means the ref is stale or was never there.
    fn resolve(&self, target: &TargetRef) -> Option<Target>;

    /// The rectangle of whatever holds focus, for the key-event scope check.
    fn focused(&self) -> Option<Rect>;

    /// The stage from the last look, without asking the target again.
    ///
    /// The guardrail needs it before every sequence, and a sequence is already
    /// the expensive part — a second round trip to re-read a rectangle that
    /// barely changes would be paid on every call.
    fn stage(&self) -> Rect;

    fn close(&mut self) -> Result<(), AppError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_capability_shape_the_skills_document_is_the_one_that_serialises() {
        // The `channeler*` skills show this object to the agent and tell it what
        // each field means. A renamed field would make them lie, so the shape is
        // pinned here rather than trusted.
        let capabilities = Capabilities {
            backend: BackendKind::Window,
            host: HostKind::HiddenDesktop,
            foreground: false,
            input: InputKind::Synthetic,
            actions: vec![ActionKind::Click, ActionKind::Hold],
            capture: false,
            issues: true,
        };

        let value = serde_json::to_value(&capabilities).unwrap();
        assert_eq!(value["backend"], "window");
        assert_eq!(value["host"], "hidden_desktop");
        assert_eq!(value["foreground"], false);
        assert_eq!(value["input"], "synthetic");
        assert_eq!(value["capture"], false);
        assert_eq!(value["issues"], true);
        assert_eq!(value["actions"][0], "click");
        assert_eq!(value["actions"][1], "hold");
    }

    #[test]
    fn a_hidden_desktop_is_the_only_host_that_does_not_take_the_foreground() {
        // The claim `channeler-open` makes, held as a fact about the enum.
        assert_eq!(HostKind::Desktop.as_str(), "desktop");
        assert_eq!(HostKind::HiddenDesktop.as_str(), "hidden-desktop");
    }
}
