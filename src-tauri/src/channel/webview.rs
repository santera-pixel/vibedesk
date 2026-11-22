//! The `webview` backend.
//!
//! Drives Chromium content — VibeDesk's own panels and widgets, and any page
//! loaded into them — through the DevTools protocol, in process. See `cdp.rs`
//! for why that route and not a debug port.
//!
//! Three things make this backend the reference implementation for the others.
//! Input is dispatched at the renderer, so the OS cursor does not move and the
//! window does not need the foreground — "background" is a property it has by
//! construction rather than something bolted on. Refs are held in the page, so a
//! ref survives a layout shift between the look and the act. And the issue
//! channel is free: the protocol already reports console errors, exceptions,
//! failed requests and dialogs, so the agent learns what broke without asking.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tauri::Manager;

use crate::channel::backend::{
    Action, ActionKind, ActionResult, Backend, BackendKind, Button, Capabilities, HostKind,
    InputKind, Look,
    Target, TargetRef,
};
use crate::channel::cdp;
use crate::channel::guardrail::Rect;
use crate::channel::issues::{Issue, Issues, Level};
use crate::error::AppError;

#[cfg(windows)]
use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2;

/// How many issues the buffer keeps before the oldest fall off.
const ISSUE_CAP: usize = 200;

/// The protocol events that mean something went wrong.
///
/// `Runtime.consoleAPICalled` and `Runtime.exceptionThrown` cover what the page
/// itself reports. `Log.entryAdded` covers what the browser reports about the
/// page — CSP violations, deprecations, failed subresources. `Network` covers
/// requests that never arrived and responses that arrived wrong. `Page` covers
/// a dialog, which blocks the page until something answers it and would
/// otherwise look like a hang.
const EVENTS: &[&str] = &[
    "Runtime.consoleAPICalled",
    "Runtime.exceptionThrown",
    "Log.entryAdded",
    "Network.loadingFailed",
    "Network.responseReceived",
    "Page.javascriptDialogOpening",
];

/// The domains those events live in, which have to be switched on first.
const DOMAINS: &[&str] = &["Runtime", "Log", "Network", "Page"];

/// What the backend remembers between calls.
struct State {
    /// The refs from the last look, so the guardrail can read a label.
    targets: Vec<Target>,
    /// The stage, in CSS pixels.
    stage: Rect,
    /// What broke.
    issues: Issues,
    /// The tokens that would remove the event listeners.
    tokens: Vec<i64>,
}

/// A channel onto one webview.
pub struct WebviewBackend {
    app: tauri::AppHandle,
    label: String,
    state: Arc<Mutex<State>>,
}

impl WebviewBackend {
    /// Open a channel: switch the domains on, subscribe to the events, and take
    /// a first look so the caller has refs to act on.
    pub fn open(app: tauri::AppHandle, label: impl Into<String>) -> Result<Self, AppError> {
        let label = label.into();
        let state = Arc::new(Mutex::new(State {
            targets: Vec::new(),
            stage: Rect {
                x: 0.0,
                y: 0.0,
                w: 0.0,
                h: 0.0,
            },
            issues: Issues::new(ISSUE_CAP),
            tokens: Vec::new(),
        }));

        let backend = Self {
            app,
            label,
            state,
        };

        let sink = backend.state.clone();
        let tokens = backend.state.clone();
        backend.on_main(move |core| {
            for domain in DOMAINS {
                cdp::call(core, &format!("{domain}.enable"), json!({}))?;
            }
            for event in EVENTS {
                let sink = sink.clone();
                let name = event.to_string();
                let token = cdp::subscribe(core, event, move |payload| {
                    if let Some((level, source, text)) = classify(&name, &payload) {
                        if let Ok(mut state) = sink.lock() {
                            state.issues.push(level, source, text, now());
                        }
                    }
                })?;
                if let Ok(mut state) = tokens.lock() {
                    state.tokens.push(token);
                }
            }
            Ok(())
        })?;

        Ok(backend)
    }

    /// Run `work` on the thread that owns the webview's message pump.
    ///
    /// The protocol call is asynchronous and its callback fires on that pump, so
    /// the call and the wait have to happen there. `with_webview` already
    /// dispatches to it, which is why this does not also call
    /// `run_on_main_thread` — doing both would post to the queue and then block
    /// waiting for the queue to drain.
    fn on_main<T, F>(&self, work: F) -> Result<T, AppError>
    where
        F: FnOnce(&ICoreWebView2) -> Result<T, AppError> + Send + 'static,
        T: Send + 'static,
    {
        let (sender, receiver) = std::sync::mpsc::channel();
        let window = self.app.get_webview_window(&self.label).ok_or_else(|| {
            AppError::message(format!("there is no webview called {}", self.label))
        })?;
        let webview: &tauri::Webview<_> = window.as_ref();

        webview
            .with_webview(move |platform| {
                let outcome = (|| {
                    let controller = platform.controller();
                    let core = unsafe { controller.CoreWebView2() }.map_err(|error| {
                        AppError::message(format!("the webview has no core: {error}"))
                    })?;
                    work(&core)
                })();
                let _ = sender.send(outcome);
            })
            .map_err(|error| AppError::message(format!("could not reach the webview: {error}")))?;

        receiver
            .recv()
            .map_err(|_| AppError::message("the webview dropped the request"))?
    }

}

impl Backend for WebviewBackend {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            backend: BackendKind::Webview,
            host: HostKind::Desktop,
            // The webview renders on the user's desktop, but CDP is out of band:
            // a click is a protocol message, not a cursor. This is the one
            // channel that is visible and still never takes the foreground.
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

    fn look(&mut self, capture: bool) -> Result<Look, AppError> {
        let state = self.state.clone();
        self.on_main(move |core| {
            let raw = evaluate(core, DIGEST, false)?;
            let parsed: Value = serde_json::from_str(
                raw.as_str()
                    .ok_or_else(|| AppError::message("the page did not describe itself"))?,
            )
            .map_err(|error| AppError::message(format!("the page described itself badly: {error}")))?;

            let stage = rect_of(parsed.get("stage")).unwrap_or(Rect {
                x: 0.0,
                y: 0.0,
                w: 0.0,
                h: 0.0,
            });
            let targets: Vec<Target> = parsed
                .get("targets")
                .and_then(Value::as_array)
                .map(|items| items.iter().filter_map(target_of).collect())
                .unwrap_or_default();

            let digest = describe(&parsed, &targets, &stage);
            let picture = if capture {
                Some(capture_stage(core, &stage)?)
            } else {
                None
            };

            if let Ok(mut state) = state.lock() {
                state.targets = targets.clone();
                state.stage = stage;
            }

            Ok(Look {
                digest,
                stage,
                targets,
                capture_type: picture.as_ref().map(|_| "image/jpeg".to_string()),
                capture: picture,
            })
        })
    }

    fn act(&mut self, action: &Action) -> Result<ActionResult, AppError> {
        let kind = action.kind();
        let started = std::time::Instant::now();
        let detail = self.dispatch(action)?;
        Ok(ActionResult::ok(
            kind,
            detail,
            started.elapsed().as_millis() as u64,
        ))
    }

    fn issues(&mut self) -> Result<Vec<Issue>, AppError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| AppError::message("the issue buffer is poisoned"))?;
        let (issues, _) = state.issues.drain_new(usize::MAX);
        Ok(issues)
    }

    fn resolve(&self, target: &TargetRef) -> Option<Target> {
        let TargetRef::Named(id) = target else {
            return None;
        };
        let state = self.state.lock().ok()?;
        state.targets.iter().find(|item| &item.id == id).cloned()
    }

    fn focused(&self) -> Option<Rect> {
        self.on_main(move |core| {
            let value = evaluate(core, FOCUSED, false)?;
            Ok(rect_of(Some(&value)))
        })
        .ok()
        .flatten()
    }

    fn stage(&self) -> Rect {
        self.state
            .lock()
            .map(|state| state.stage)
            .unwrap_or(Rect {
                x: 0.0,
                y: 0.0,
                w: 0.0,
                h: 0.0,
            })
    }

    fn close(&mut self) -> Result<(), AppError> {
        let state = self.state.clone();
        self.on_main(move |core| {
            let tokens = state
                .lock()
                .map(|mut state| std::mem::take(&mut state.tokens))
                .unwrap_or_default();
            for token in tokens {
                let _ = cdp::call(core, "Runtime.evaluate", json!({ "expression": "0" }));
                let _ = token;
            }
            let _ = evaluate(core, "(() => { delete window.__channel; return true; })()", false);
            Ok(())
        })
    }
}

impl WebviewBackend {
    /// Do one action, returning a short description of what happened.
    fn dispatch(&mut self, action: &Action) -> Result<String, AppError> {
        let state = self.state.clone();
        let action = action.clone();
        self.on_main(move |core| match action {
            Action::Click {
                target,
                button,
                count,
            } => {
                let (x, y) = point_of(core, &target)?;
                click(core, x, y, button, count)?;
                Ok(format!("clicked ({x:.0}, {y:.0}) {count}×"))
            }
            Action::Hold { key, frames, ms } => {
                hold(core, &key, frames, ms)?;
                Ok(format!("held {key}"))
            }
            Action::Drag { from, to } => {
                let (x1, y1) = point_of(core, &from)?;
                let (x2, y2) = point_of(core, &to)?;
                drag(core, x1, y1, x2, y2)?;
                Ok(format!("dragged ({x1:.0}, {y1:.0}) to ({x2:.0}, {y2:.0})"))
            }
            Action::Type { text, target } => {
                if let Some(target) = target {
                    let (x, y) = point_of(core, &target)?;
                    click(core, x, y, Button::Left, 1)?;
                }
                cdp::call(core, "Input.insertText", json!({ "text": text }))?;
                Ok(format!("typed {} characters", text.chars().count()))
            }
            Action::Scroll { target, dx, dy } => {
                let (x, y) = match target {
                    Some(target) => point_of(core, &target)?,
                    None => {
                        let stage = state
                            .lock()
                            .map(|state| state.stage)
                            .unwrap_or(Rect {
                                x: 0.0,
                                y: 0.0,
                                w: 0.0,
                                h: 0.0,
                            });
                        (stage.x + stage.w / 2.0, stage.y + stage.h / 2.0)
                    }
                };
                cdp::call(
                    core,
                    "Input.dispatchMouseEvent",
                    json!({
                        "type": "mouseWheel", "x": x, "y": y,
                        "deltaX": dx, "deltaY": dy,
                    }),
                )?;
                Ok(format!("scrolled ({dx:.0}, {dy:.0})"))
            }
            Action::Tick { frames } => {
                advance(core, frames)?;
                Ok(format!("advanced {frames} frames"))
            }
            Action::Wait { ms } => {
                std::thread::sleep(Duration::from_millis(ms));
                Ok(format!("waited {ms}ms"))
            }
            Action::Read { target } => {
                let text = read(core, &target)?;
                Ok(text)
            }
            // The capability list already refuses this before dispatch. The arm
            // exists so the match stays exhaustive without a wildcard, and it
            // says the reason rather than pretending to run a suite.
            Action::Run { .. } => Err(AppError::message(
                "a webview has no test suite of its own, so run is not supported — use the engine backend",
            )),
        })
    }
}

/// Resolve a target to a point, re-reading the page for a named ref.
fn point_of(core: &ICoreWebView2, target: &TargetRef) -> Result<(f64, f64), AppError> {
    match target {
        TargetRef::Point { x, y } => Ok((*x, *y)),
        TargetRef::Named(id) => {
            let script = format!(
                "(() => {{ const s = window.__channel; const el = s && s.refs.get({id:?}); \
                 if (!el || !el.isConnected) return null; \
                 const r = el.getBoundingClientRect(); \
                 return {{ x: r.x + r.width / 2, y: r.y + r.height / 2 }}; }})()"
            );
            let value = evaluate(core, &script, false)?;
            match (
                value.get("x").and_then(Value::as_f64),
                value.get("y").and_then(Value::as_f64),
            ) {
                (Some(x), Some(y)) => Ok((x, y)),
                _ => Err(AppError::message(format!(
                    "{id} is no longer on the page — call look again"
                ))),
            }
        }
    }
}

fn click(
    core: &ICoreWebView2,
    x: f64,
    y: f64,
    button: Button,
    count: u32,
) -> Result<(), AppError> {
    let (name, mask) = match button {
        Button::Left => ("left", 1),
        Button::Right => ("right", 2),
        Button::Middle => ("middle", 4),
    };
    for index in 0..count.max(1) {
        let click_count = index + 1;
        cdp::call(
            core,
            "Input.dispatchMouseEvent",
            json!({
                "type": "mousePressed", "x": x, "y": y,
                "button": name, "buttons": mask, "clickCount": click_count,
            }),
        )?;
        cdp::call(
            core,
            "Input.dispatchMouseEvent",
            json!({
                "type": "mouseReleased", "x": x, "y": y,
                "button": name, "buttons": 0, "clickCount": click_count,
            }),
        )?;
    }
    Ok(())
}

fn drag(core: &ICoreWebView2, x1: f64, y1: f64, x2: f64, y2: f64) -> Result<(), AppError> {
    cdp::call(
        core,
        "Input.dispatchMouseEvent",
        json!({ "type": "mousePressed", "x": x1, "y": y1, "button": "left", "buttons": 1, "clickCount": 1 }),
    )?;
    // A drag that teleports is a drag no drag handler sees. Twelve steps is
    // enough for a pointermove listener to fire and cheap enough to be free.
    for step in 1..=12 {
        let t = step as f64 / 12.0;
        cdp::call(
            core,
            "Input.dispatchMouseEvent",
            json!({
                "type": "mouseMoved",
                "x": x1 + (x2 - x1) * t,
                "y": y1 + (y2 - y1) * t,
                "button": "left", "buttons": 1,
            }),
        )?;
    }
    cdp::call(
        core,
        "Input.dispatchMouseEvent",
        json!({ "type": "mouseReleased", "x": x2, "y": y2, "button": "left", "buttons": 0, "clickCount": 1 }),
    )?;
    Ok(())
}

fn hold(
    core: &ICoreWebView2,
    key: &str,
    frames: Option<u32>,
    ms: Option<u64>,
) -> Result<(), AppError> {
    let (code, name, virtual_key) = key_of(key);
    cdp::call(
        core,
        "Input.dispatchKeyEvent",
        json!({
            "type": "keyDown", "key": name, "code": code,
            "windowsVirtualKeyCode": virtual_key, "autoRepeat": true,
        }),
    )?;

    match (frames, ms) {
        (Some(frames), _) => advance(core, frames)?,
        (None, Some(ms)) => std::thread::sleep(Duration::from_millis(ms)),
        (None, None) => advance(core, 1)?,
    }

    cdp::call(
        core,
        "Input.dispatchKeyEvent",
        json!({
            "type": "keyUp", "key": name, "code": code,
            "windowsVirtualKeyCode": virtual_key,
        }),
    )?;
    Ok(())
}

fn read(core: &ICoreWebView2, target: &TargetRef) -> Result<String, AppError> {
    let script = match target {
        TargetRef::Named(id) => format!(
            "(() => {{ const s = window.__channel; const el = s && s.refs.get({id:?}); \
             if (!el) return null; \
             return (el.value !== undefined && el.value !== null) ? String(el.value) : (el.innerText || el.textContent || ''); }})()"
        ),
        TargetRef::Point { .. } => {
            return Err(AppError::message(
                "read needs a ref, not a point — a point has no text",
            ))
        }
    };
    let value = evaluate(core, &script, false)?;
    Ok(match value {
        Value::String(text) => text,
        Value::Null => String::new(),
        other => other.to_string(),
    })
}

/// Advance the page by a number of animation frames.
///
/// Frames rather than milliseconds because a frame is what the page counts. A
/// game loop that runs at 60 Hz and one that runs at 144 Hz both advance by one
/// frame, and a test written in frames means the same thing on both.
fn advance(core: &ICoreWebView2, frames: u32) -> Result<(), AppError> {
    let script = format!(
        "new Promise(done => {{ let left = {frames}; \
         const step = () => {{ if (--left <= 0) done(true); else requestAnimationFrame(step); }}; \
         requestAnimationFrame(step); }})"
    );
    evaluate(core, &script, true)?;
    Ok(())
}

fn capture_stage(core: &ICoreWebView2, stage: &Rect) -> Result<String, AppError> {
    let answer = cdp::call(
        core,
        "Page.captureScreenshot",
        json!({
            "format": "jpeg",
            "quality": 60,
            "clip": {
                "x": stage.x, "y": stage.y,
                "width": stage.w.max(1.0), "height": stage.h.max(1.0),
                "scale": 1,
            },
        }),
    )?;
    answer
        .get("data")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| AppError::message("the page returned no picture"))
}

/// Evaluate a script and return its value.
fn evaluate(core: &ICoreWebView2, script: &str, await_promise: bool) -> Result<Value, AppError> {
    let answer = cdp::call(
        core,
        "Runtime.evaluate",
        json!({
            "expression": script,
            "returnByValue": true,
            "awaitPromise": await_promise,
        }),
    )?;

    if let Some(details) = answer.get("exceptionDetails") {
        let text = details
            .get("exception")
            .and_then(|exception| exception.get("description"))
            .or_else(|| details.get("text"))
            .and_then(Value::as_str)
            .unwrap_or("the page threw");
        return Err(AppError::message(text.to_string()));
    }

    Ok(answer
        .pointer("/result/value")
        .cloned()
        .unwrap_or(Value::Null))
}

fn rect_of(value: Option<&Value>) -> Option<Rect> {
    let value = value?;
    Some(Rect {
        x: value.get("x")?.as_f64()?,
        y: value.get("y")?.as_f64()?,
        w: value.get("w")?.as_f64()?,
        h: value.get("h")?.as_f64()?,
    })
}

fn target_of(value: &Value) -> Option<Target> {
    Some(Target {
        id: value.get("id")?.as_str()?.to_string(),
        role: value
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("element")
            .to_string(),
        label: value
            .get("label")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        rect: rect_of(value.get("rect"))?,
    })
}

/// The text the model reads instead of a picture.
fn describe(parsed: &Value, targets: &[Target], stage: &Rect) -> String {
    let title = parsed.get("title").and_then(Value::as_str).unwrap_or("");
    let url = parsed.get("url").and_then(Value::as_str).unwrap_or("");
    let mut text = format!(
        "{title} — {url} ({}×{})\n",
        stage.w.round(),
        stage.h.round()
    );
    if targets.is_empty() {
        text.push_str("nothing interactive on the page\n");
        return text;
    }
    for target in targets {
        let label = if target.label.is_empty() {
            "(no label)".to_string()
        } else {
            format!("\"{}\"", target.label)
        };
        text.push_str(&format!(
            "{}  {}  {}  at ({:.0}, {:.0})\n",
            target.id,
            target.role,
            label,
            target.rect.x + target.rect.w / 2.0,
            target.rect.y + target.rect.h / 2.0,
        ));
    }
    text
}

/// Turn a protocol event into an issue, when it is one.
fn classify(event: &str, payload: &Value) -> Option<(Level, String, String)> {
    match event {
        "Runtime.consoleAPICalled" => {
            let kind = payload.get("type").and_then(Value::as_str).unwrap_or("log");
            let level = match kind {
                "error" | "assert" => Level::Error,
                "warning" => Level::Warn,
                _ => Level::Info,
            };
            let text = payload
                .get("args")
                .and_then(Value::as_array)
                .map(|args| {
                    args.iter()
                        .map(|arg| {
                            arg.get("value")
                                .map(|value| match value {
                                    Value::String(text) => text.clone(),
                                    other => other.to_string(),
                                })
                                .or_else(|| {
                                    arg.get("description")
                                        .and_then(Value::as_str)
                                        .map(str::to_string)
                                })
                                .unwrap_or_default()
                        })
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            Some((level, "console".to_string(), text))
        }
        "Runtime.exceptionThrown" => {
            let details = payload.get("exceptionDetails")?;
            let text = details
                .get("exception")
                .and_then(|exception| exception.get("description"))
                .or_else(|| details.get("text"))
                .and_then(Value::as_str)
                .unwrap_or("an exception was thrown");
            Some((Level::Error, "exception".to_string(), text.to_string()))
        }
        "Log.entryAdded" => {
            let entry = payload.get("entry")?;
            let level = match entry.get("level").and_then(Value::as_str) {
                Some("error") => Level::Error,
                Some("warning") => Level::Warn,
                _ => Level::Info,
            };
            let text = entry
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let source = entry
                .get("source")
                .and_then(Value::as_str)
                .unwrap_or("log")
                .to_string();
            Some((level, source, text))
        }
        "Network.loadingFailed" => {
            let text = payload
                .get("errorText")
                .and_then(Value::as_str)
                .unwrap_or("a request failed");
            Some((Level::Error, "network".to_string(), text.to_string()))
        }
        "Network.responseReceived" => {
            let response = payload.get("response")?;
            let status = response.get("status").and_then(Value::as_f64)?;
            if status < 400.0 {
                return None;
            }
            let url = response
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or("a request");
            Some((
                Level::Warn,
                "network".to_string(),
                format!("{status:.0} from {url}"),
            ))
        }
        "Page.javascriptDialogOpening" => {
            let kind = payload
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("dialog");
            let message = payload
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("");
            Some((
                Level::Warn,
                "dialog".to_string(),
                format!("a {kind} is open and blocking the page: {message}"),
            ))
        }
        _ => None,
    }
}

/// The CDP triple for a key: `code`, `key`, and the Windows virtual key.
fn key_of(name: &str) -> (String, String, u32) {
    let named = match name {
        "ArrowLeft" => Some(("ArrowLeft", "ArrowLeft", 37)),
        "ArrowUp" => Some(("ArrowUp", "ArrowUp", 38)),
        "ArrowRight" => Some(("ArrowRight", "ArrowRight", 39)),
        "ArrowDown" => Some(("ArrowDown", "ArrowDown", 40)),
        "Enter" => Some(("Enter", "Enter", 13)),
        "Escape" => Some(("Escape", "Escape", 27)),
        "Tab" => Some(("Tab", "Tab", 9)),
        "Space" => Some(("Space", " ", 32)),
        "Backspace" => Some(("Backspace", "Backspace", 8)),
        "Shift" => Some(("ShiftLeft", "Shift", 16)),
        "Control" => Some(("ControlLeft", "Control", 17)),
        "Alt" => Some(("AltLeft", "Alt", 18)),
        _ => None,
    };
    if let Some((code, key, virtual_key)) = named {
        return (code.to_string(), key.to_string(), virtual_key);
    }

    let mut characters = name.chars();
    match (characters.next(), characters.next()) {
        (Some(letter), None) if letter.is_ascii_alphabetic() => {
            let upper = letter.to_ascii_uppercase();
            (format!("Key{upper}"), letter.to_string(), upper as u32)
        }
        (Some(digit), None) if digit.is_ascii_digit() => {
            (format!("Digit{digit}"), digit.to_string(), digit as u32)
        }
        _ => (name.to_string(), name.to_string(), 0),
    }
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// What the page is, and what can be clicked on it.
///
/// Refs live in the page rather than in Rust so that a ref survives a layout
/// shift: `act` asks the page where `e12` is now, not where it was when the
/// agent looked. The map is namespaced and cleared on every look, so a stale ref
/// fails loudly instead of landing on whatever moved into its place.
const DIGEST: &str = r#"(() => {
  const state = (window.__channel = window.__channel || { refs: new Map(), next: 1 });
  state.refs.clear();
  state.next = 1;

  const SELECTOR = [
    'a[href]', 'button', 'input', 'select', 'textarea', 'summary',
    '[role=button]', '[role=link]', '[role=checkbox]', '[role=radio]',
    '[role=tab]', '[role=menuitem]', '[role=switch]', '[role=slider]',
    '[contenteditable=""]', '[contenteditable=true]',
    '[tabindex]:not([tabindex="-1"])'
  ].join(',');

  const labelOf = (el) => {
    const aria = el.getAttribute('aria-label');
    if (aria && aria.trim()) return aria.trim();
    const by = el.getAttribute('aria-labelledby');
    if (by) {
      const text = by.split(/\s+/)
        .map(id => { const found = document.getElementById(id); return found ? found.textContent : ''; })
        .join(' ').trim();
      if (text) return text;
    }
    if (el.tagName === 'INPUT') {
      const type = (el.type || 'text').toLowerCase();
      if (type === 'submit' || type === 'button' || type === 'reset') return (el.value || type).trim();
      if (el.id) {
        const found = document.querySelector('label[for="' + CSS.escape(el.id) + '"]');
        if (found) return found.textContent.trim();
      }
      const wrapping = el.closest('label');
      if (wrapping) return wrapping.textContent.trim();
      return (el.placeholder || el.name || type).trim();
    }
    return (el.innerText || el.textContent || el.value || '').trim();
  };

  const roleOf = (el) => {
    const explicit = el.getAttribute('role');
    if (explicit) return explicit;
    const tag = el.tagName.toLowerCase();
    if (tag === 'a') return 'link';
    if (tag === 'button') return 'button';
    if (tag === 'select') return 'combobox';
    if (tag === 'textarea') return 'textbox';
    if (tag === 'input') {
      const type = (el.type || 'text').toLowerCase();
      if (type === 'checkbox' || type === 'radio') return type;
      if (type === 'submit' || type === 'button' || type === 'reset') return 'button';
      return 'textbox';
    }
    return tag;
  };

  const targets = [];
  for (const el of document.querySelectorAll(SELECTOR)) {
    const rect = el.getBoundingClientRect();
    if (rect.width < 1 || rect.height < 1) continue;
    const style = getComputedStyle(el);
    if (style.visibility === 'hidden' || style.display === 'none' || style.opacity === '0') continue;
    const id = 'e' + state.next++;
    state.refs.set(id, el);
    targets.push({
      id,
      role: roleOf(el),
      label: labelOf(el).replace(/\s+/g, ' ').slice(0, 120),
      rect: { x: rect.x, y: rect.y, w: rect.width, h: rect.height }
    });
  }

  return JSON.stringify({
    title: document.title,
    url: location.href,
    stage: { x: 0, y: 0, w: innerWidth, h: innerHeight },
    targets
  });
})()"#;

/// Where focus is, for the key-event scope check.
///
/// A page with nothing focused sends key events to the body, which covers the
/// whole page — so the body's rectangle is the honest answer, not `null`.
const FOCUSED: &str = r#"(() => {
  const el = document.activeElement || document.body;
  if (!el) return null;
  const rect = el.getBoundingClientRect();
  return { x: rect.x, y: rect.y, w: rect.width, h: rect.height };
})()"#;
