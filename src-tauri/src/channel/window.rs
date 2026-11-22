//! The `window` backend.
//!
//! Drives a native window: any Win32, WinForms, WPF, Qt, Electron or engine
//! window, by title, by class, or by launching the executable that makes one.
//! This is the backend that makes the channeler universal rather than
//! web-shaped — it needs nothing from the target, no SDK, no plugin, no MCP
//! server, and no cooperation beyond being a window.
//!
//! Three mechanisms, each the one Windows actually provides for the job:
//!
//! * **Observe** through UI Automation, which is the accessibility tree every
//!   window has whether or not its author thought about testing. It gives names,
//!   roles and rectangles, which is what a ref needs.
//! * **Input** through `SendInput`, which is the same path a real mouse and
//!   keyboard take. A message posted to a window is ignored by anything reading
//!   raw input, which is most games; `SendInput` is not.
//! * **Issues** through the `OutputDebugString` buffer, which is where a Win32
//!   program's diagnostics go and where `DebugView` reads them from.
//!
//! `SendInput` goes to whatever holds the foreground, so this backend takes the
//! foreground while it acts. That is the honest cost of driving a native window
//! on the user's own desktop, and it is why the host axis exists: the same
//! backend on a hidden desktop takes the foreground of a desktop nobody is
//! looking at.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::channel::backend::{
    Action, ActionKind, ActionResult, Backend, BackendKind, Button, Capabilities, InputKind, Look,
    Target, TargetRef,
};
use crate::channel::guardrail::Rect;
use crate::channel::host::{Host, HostKind};
use crate::channel::issues::{Issue, Issues, Level};
use crate::error::AppError;

/// How many elements the tree walk keeps.
const TARGET_CAP: usize = 300;

/// How many issues the buffer keeps.
const ISSUE_CAP: usize = 200;

/// How long to wait for a program to make its window.
const STARTUP_TRIES: usize = 40;

/// What to look for.
#[derive(Debug, Clone, Default)]
pub struct WindowSpec {
    /// A substring of the window title.
    pub title: Option<String>,
    /// An exact window class name.
    pub class: Option<String>,
    /// An executable to start, when no window is open yet.
    pub command: Option<String>,
    pub args: Vec<String>,
    /// Where to run it. A hidden desktop keeps the test off the user's screen.
    pub host: Option<HostKind>,
}

struct State {
    targets: Vec<Target>,
    stage: Rect,
    issues: Issues,
}

/// A channel onto one window.
pub struct WindowBackend {
    #[cfg(windows)]
    hwnd: windows::Win32::Foundation::HWND,
    title: String,
    host: Host,
    state: Arc<Mutex<State>>,
    #[cfg(windows)]
    debug: Option<debug::Reader>,
}

// A window handle is a process-wide identifier, not a pointer into this
// process's memory, so it is valid on any thread. The registry serialises every
// call through one lock, so nothing here is touched concurrently.
unsafe impl Send for WindowBackend {}

impl WindowBackend {
    /// Find a window, or start the program that makes one.
    pub fn open(spec: WindowSpec) -> Result<Self, AppError> {
        #[cfg(windows)]
        {
            let host = match spec.host.unwrap_or(HostKind::Desktop) {
                HostKind::Desktop => Host::desktop(),
                HostKind::HiddenDesktop => Host::hidden()?,
            };

            let handle = locate(&host, &spec)?;
            let title = host.run(move || find::title_of(hwnd_of(handle)))?;
            let stage = host.run(move || find::client_rect(hwnd_of(handle)))?;
            let debug = debug::Reader::start().ok();

            Ok(Self {
                hwnd: hwnd_of(handle),
                title,
                host,
                state: Arc::new(Mutex::new(State {
                    targets: Vec::new(),
                    stage,
                    issues: Issues::new(ISSUE_CAP),
                })),
                debug,
            })
        }
        #[cfg(not(windows))]
        {
            let _ = spec;
            Err(AppError::message(
                "the window backend needs Windows — it drives Win32 windows",
            ))
        }
    }

    /// Run a job on the desktop this channel is bound to.
    ///
    /// The handle travels as an `isize` because `HWND` is a raw pointer and so is
    /// not `Send`. It names a window in another process, so moving the number is
    /// sound; what matters is that the job runs on the thread that owns the
    /// desktop, and that is what `Host::run` guarantees.
    #[cfg(windows)]
    fn on_desktop<T, F>(&self, job: F) -> Result<T, AppError>
    where
        T: Send + 'static,
        F: FnOnce(isize) -> Result<T, AppError> + Send + 'static,
    {
        let handle = self.hwnd.0 as isize;
        self.host.run(move || job(handle))?
    }
}

/// Find the window the spec names, starting the program if it is not open.
#[cfg(windows)]
fn locate(host: &Host, spec: &WindowSpec) -> Result<isize, AppError> {
    let search = |spec: &WindowSpec| {
        let spec = spec.clone();
        host.run(move || find::search(&spec))
    };

    if let Some(found) = search(spec)? {
        return Ok(found);
    }

    let Some(command) = spec.command.as_deref() else {
        return Err(AppError::message(format!(
            "no window matches {} — open it, or pass a command to start",
            find::describe_spec(spec)
        )));
    };

    host.spawn(command, &spec.args)?;

    // A window takes time to appear. Two seconds is long enough for a native app
    // and short enough that a program which never makes one fails here rather
    // than hanging the tool call.
    for _ in 0..STARTUP_TRIES {
        std::thread::sleep(Duration::from_millis(50));
        if let Some(found) = search(spec)? {
            return Ok(found);
        }
    }

    Err(AppError::message(format!(
        "{command} started but made no window matching {}",
        find::describe_spec(spec)
    )))
}

/// Bring the window forward, or say why the click would have gone elsewhere.
///
/// Hardware input goes to whatever is on top, so a click on a window that did not
/// come forward lands on the user's work instead. Failing here is the difference
/// between a test that reports a problem and a test that quietly types into
/// somebody's editor.
#[cfg(windows)]
fn raise(window: windows::Win32::Foundation::HWND) -> Result<(), AppError> {
    if input::focus(window) {
        return Ok(());
    }
    Err(AppError::message(
        "the window would not come to the front, so the input would have landed on whatever is — close what is in front of it, or open the channel on a hidden desktop",
    ))
}

#[cfg(windows)]
fn hwnd_of(handle: isize) -> windows::Win32::Foundation::HWND {
    windows::Win32::Foundation::HWND(handle as *mut std::ffi::c_void)
}

impl Backend for WindowBackend {
    fn capabilities(&self) -> Capabilities {
        let host = self.host.kind();
        Capabilities {
            backend: BackendKind::Window,
            host,
            // `SendInput` goes to the desktop the calling thread is bound to, so
            // a window on the user's desktop has to be brought forward first. On
            // a hidden desktop it is already the only thing there.
            foreground: host == HostKind::Desktop,
            input: if host == HostKind::Desktop {
                InputKind::Hardware
            } else {
                InputKind::Synthetic
            },
            actions: vec![
                ActionKind::Click,
                ActionKind::Hold,
                ActionKind::Drag,
                ActionKind::Type,
                ActionKind::Scroll,
                ActionKind::Wait,
                ActionKind::Read,
            ],
            // A picture of the client area, taken by asking the window to render
            // itself. That is the one route that works on a desktop that is not
            // the active one, because it is a message to the window rather than
            // a read of the screen.
            capture: true,
            issues: true,
        }
    }

    fn look(&mut self, capture: bool) -> Result<Look, AppError> {
        #[cfg(windows)]
        {
            let (targets, stage) = self.on_desktop(|handle| tree::walk(hwnd_of(handle)))?;
            let digest = describe(&self.title, &stage, &targets);

            if let Ok(mut state) = self.state.lock() {
                state.targets = targets.clone();
                state.stage = stage;
            }

            // The picture is taken on the channel's own desktop, for the same
            // reason the tree is: a window on a hidden desktop is not the one
            // the user's desktop would render.
            let picture = if capture {
                self.on_desktop(|handle| crate::channel::capture::window_png(hwnd_of(handle)))?
            } else {
                None
            };

            Ok(Look {
                digest,
                stage,
                targets,
                capture_type: picture.as_ref().map(|_| "image/png".to_string()),
                capture: picture.map(|(base64, _, _)| base64),
            })
        }
        #[cfg(not(windows))]
        {
            let _ = capture;
            Err(AppError::message("the window backend needs Windows"))
        }
    }

    fn act(&mut self, action: &Action) -> Result<ActionResult, AppError> {
        let kind = action.kind();
        let started = std::time::Instant::now();

        #[cfg(windows)]
        {
            let detail = self.dispatch(action)?;
            Ok(ActionResult::ok(
                kind,
                detail,
                started.elapsed().as_millis() as u64,
            ))
        }
        #[cfg(not(windows))]
        {
            let _ = action;
            Err(AppError::message("the window backend needs Windows"))
        }
    }

    fn issues(&mut self) -> Result<Vec<Issue>, AppError> {
        #[cfg(windows)]
        if let Some(reader) = self.debug.as_mut() {
            if let Ok(mut state) = self.state.lock() {
                for line in reader.drain() {
                    state.issues.push(Level::Info, "debug", line, now());
                }
            }
        }

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
        #[cfg(windows)]
        {
            return self
                .on_desktop(|handle| Ok(find::focused_rect(hwnd_of(handle))))
                .ok()
                .flatten();
        }
        #[cfg(not(windows))]
        {
            None
        }
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
        #[cfg(windows)]
        if let Some(mut reader) = self.debug.take() {
            reader.stop();
        }
        Ok(())
    }
}

#[cfg(windows)]
impl WindowBackend {
    fn dispatch(&mut self, action: &Action) -> Result<String, AppError> {
        // A hidden desktop is not the active input desktop, so hardware input
        // never arrives there. Messages do, whatever desktop the window is on.
        let synthetic = self.host.kind() == HostKind::HiddenDesktop;

        match action {
            Action::Click {
                target,
                button,
                count,
            } => {
                let (x, y) = self.point_of(target)?;
                let (button, count) = (*button, *count);
                self.on_desktop(move |handle| {
                    let window = hwnd_of(handle);
                    if synthetic {
                        return input::post_click(window, x, y, button, count);
                    }
                    raise(window)?;
                    input::click(x, y, button, count)
                })?;
                Ok(format!("clicked ({x:.0}, {y:.0}) {count}×"))
            }
            Action::Hold { key, frames, ms } => {
                let (held, frames, ms) = (key.clone(), *frames, *ms);
                self.on_desktop(move |handle| {
                    let window = hwnd_of(handle);
                    if synthetic {
                        return input::post_hold(window, &held, frames, ms);
                    }
                    raise(window)?;
                    input::hold(&held, frames, ms)
                })?;
                Ok(format!("held {key}"))
            }
            Action::Drag { from, to } => {
                let (x1, y1) = self.point_of(from)?;
                let (x2, y2) = self.point_of(to)?;
                self.on_desktop(move |handle| {
                    let window = hwnd_of(handle);
                    if synthetic {
                        return input::post_drag(window, x1, y1, x2, y2);
                    }
                    raise(window)?;
                    input::drag(x1, y1, x2, y2)
                })?;
                Ok(format!("dragged ({x1:.0}, {y1:.0}) to ({x2:.0}, {y2:.0})"))
            }
            Action::Type { text, target } => {
                let point = match target {
                    Some(target) => Some(self.point_of(target)?),
                    None => None,
                };
                let typed = text.clone();
                self.on_desktop(move |handle| {
                    let window = hwnd_of(handle);
                    if synthetic {
                        if let Some((x, y)) = point {
                            input::post_click(window, x, y, Button::Left, 1)?;
                        }
                        return input::post_text(window, &typed);
                    }
                    raise(window)?;
                    if let Some((x, y)) = point {
                        input::click(x, y, Button::Left, 1)?;
                    }
                    input::type_text(&typed)
                })?;
                Ok(format!("typed {} characters", text.chars().count()))
            }
            Action::Scroll { target, dx, dy } => {
                let (x, y) = match target {
                    Some(target) => self.point_of(target)?,
                    None => {
                        let stage = self.stage();
                        (stage.x + stage.w / 2.0, stage.y + stage.h / 2.0)
                    }
                };
                let (dx, dy) = (*dx, *dy);
                self.on_desktop(move |handle| {
                    let window = hwnd_of(handle);
                    if synthetic {
                        return input::post_scroll(window, x, y, dy);
                    }
                    raise(window)?;
                    input::scroll(x, y, dx, dy)
                })?;
                Ok(format!("scrolled ({dx:.0}, {dy:.0})"))
            }
            Action::Wait { ms } => {
                std::thread::sleep(Duration::from_millis(*ms));
                Ok(format!("waited {ms}ms"))
            }
            Action::Read { target } => {
                let text = self.read(target)?;
                Ok(text)
            }
            // The capability list already refuses this before dispatch. The arm
            // exists so the match stays exhaustive without a wildcard, and it
            // says the reason rather than pretending to advance a clock.
            Action::Tick { .. } => Err(AppError::message(
                "a window has no frame clock this backend can observe, so tick is not supported — use wait",
            )),
            // Same shape, same reason: the capability list refuses it first.
            Action::Run { .. } => Err(AppError::message(
                "a window has no test suite of its own, so run is not supported — use the engine backend",
            )),
        }
    }

    fn point_of(&self, target: &TargetRef) -> Result<(f64, f64), AppError> {
        match target {
            TargetRef::Point { x, y } => Ok((*x, *y)),
            TargetRef::Named(id) => {
                let state = self
                    .state
                    .lock()
                    .map_err(|_| AppError::message("the target list is poisoned"))?;
                state
                    .targets
                    .iter()
                    .find(|item| &item.id == id)
                    .map(Target::centre)
                    .ok_or_else(|| {
                        AppError::message(format!("{id} is no longer in the window — call look again"))
                    })
            }
        }
    }

    fn read(&self, target: &TargetRef) -> Result<String, AppError> {
        let TargetRef::Named(id) = target else {
            return Err(AppError::message(
                "read needs a ref, not a point — a point has no text",
            ));
        };
        let state = self
            .state
            .lock()
            .map_err(|_| AppError::message("the target list is poisoned"))?;
        state
            .targets
            .iter()
            .find(|item| &item.id == id)
            .map(|item| item.label.clone())
            .ok_or_else(|| AppError::message(format!("{id} is no longer in the window")))
    }
}

fn describe(title: &str, stage: &Rect, targets: &[Target]) -> String {
    let mut text = format!(
        "{title} — {}×{} at ({:.0}, {:.0})\n",
        stage.w.round(),
        stage.h.round(),
        stage.x,
        stage.y
    );
    if targets.is_empty() {
        text.push_str("nothing named in the window\n");
        return text;
    }
    for target in targets {
        let label = if target.label.is_empty() {
            "(no name)".to_string()
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

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[cfg(windows)]
mod find {
    use windows::core::BOOL;
    use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
    use windows::Win32::Graphics::Gdi::ClientToScreen;
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetClassNameW, GetClientRect, GetWindowTextLengthW, GetWindowTextW,
        IsWindowVisible, SetForegroundWindow, ShowWindow, SW_RESTORE,
    };

    use super::WindowSpec;
    use crate::channel::guardrail::Rect;

    /// What the enumeration is looking for, and what it has found so far.
    ///
    /// The callback is `extern "system"` and cannot capture, so the state travels
    /// through `LPARAM` as a pointer to this.
    struct Search<'a> {
        spec: &'a WindowSpec,
        found: Option<HWND>,
    }

    /// Find the window the spec names, without starting anything.
    ///
    /// Starting a program is the host's job, because only the host knows which
    /// desktop the child belongs on.
    pub fn search(spec: &WindowSpec) -> Option<isize> {
        let mut search = Search { spec, found: None };
        unsafe {
            let _ = EnumWindows(Some(visit), LPARAM(&mut search as *mut Search as isize));
        }
        search.found.map(|window| window.0 as isize)
    }

    pub fn describe_spec(spec: &WindowSpec) -> String {
        match (&spec.title, &spec.class) {
            (Some(title), _) => format!("\"{title}\""),
            (None, Some(class)) => format!("class {class}"),
            (None, None) => "anything".to_string(),
        }
    }

    unsafe extern "system" fn visit(window: HWND, parameter: LPARAM) -> BOOL {
        let search = unsafe { &mut *(parameter.0 as *mut Search) };
        if search.found.is_some() {
            return BOOL(0);
        }
        if !unsafe { IsWindowVisible(window) }.as_bool() {
            return BOOL(1);
        }

        let title = title_of(window);
        if title.is_empty() {
            return BOOL(1);
        }
        if let Some(wanted) = search.spec.title.as_deref() {
            if !title.to_lowercase().contains(&wanted.to_lowercase()) {
                return BOOL(1);
            }
        }
        if let Some(wanted) = search.spec.class.as_deref() {
            if class_of(window) != wanted {
                return BOOL(1);
            }
        }

        search.found = Some(window);
        BOOL(0)
    }

    pub fn title_of(window: HWND) -> String {
        let length = unsafe { GetWindowTextLengthW(window) };
        if length <= 0 {
            return String::new();
        }
        let mut buffer = vec![0u16; length as usize + 1];
        let written = unsafe { GetWindowTextW(window, &mut buffer) };
        String::from_utf16_lossy(&buffer[..written.max(0) as usize])
    }

    fn class_of(window: HWND) -> String {
        let mut buffer = [0u16; 256];
        let written = unsafe { GetClassNameW(window, &mut buffer) };
        String::from_utf16_lossy(&buffer[..written.max(0) as usize])
    }

    /// The window's client area, in screen coordinates.
    ///
    /// `GetClientRect` answers in client space, which always starts at zero, so
    /// the origin has to come from `ClientToScreen` — the difference between the
    /// window rectangle and the client rectangle is the border and the title bar,
    /// not the position.
    pub fn client_rect(window: HWND) -> Rect {
        let mut client = RECT::default();
        if unsafe { GetClientRect(window, &mut client) }.is_err() {
            return Rect {
                x: 0.0,
                y: 0.0,
                w: 0.0,
                h: 0.0,
            };
        }
        let mut origin = POINT { x: 0, y: 0 };
        let _ = unsafe { ClientToScreen(window, &mut origin) };
        Rect {
            x: f64::from(origin.x),
            y: f64::from(origin.y),
            w: f64::from(client.right - client.left),
            h: f64::from(client.bottom - client.top),
        }
    }

    /// Bring the window forward, because `SendInput` goes to the foreground.
    ///
    /// `SetForegroundWindow` is refused for a process that is not already the
    /// foreground process — the call succeeds and nothing moves. Attaching to the
    /// foreground thread's input queue lifts that restriction for the duration,
    /// which is the documented way to do this rather than a trick.
    ///
    /// Returns whether the window actually came forward. A caller that ignores
    /// this is a caller that clicks whatever happens to be on top.
    pub fn focus(window: HWND) -> bool {
        use windows::Win32::System::Threading::AttachThreadInput;
        use windows::Win32::UI::Input::KeyboardAndMouse::SetActiveWindow;
        use windows::Win32::UI::WindowsAndMessaging::{
            BringWindowToTop, GetForegroundWindow, GetWindowThreadProcessId,
        };

        unsafe {
            let _ = ShowWindow(window, SW_RESTORE);
            if GetForegroundWindow() == window {
                return true;
            }

            let target = GetWindowThreadProcessId(window, None);
            let foreground = GetWindowThreadProcessId(GetForegroundWindow(), None);
            let attached = target != foreground && AttachThreadInput(foreground, target, true).as_bool();

            let _ = BringWindowToTop(window);
            let _ = SetForegroundWindow(window);
            let _ = SetActiveWindow(window);

            if attached {
                let _ = AttachThreadInput(foreground, target, false);
            }

            GetForegroundWindow() == window
        }
    }

    /// The rectangle of the focused element, for the key-event scope check.
    ///
    /// A window with nothing focused sends keys to the window itself, so the
    /// client area is the honest answer rather than `None`.
    pub fn focused_rect(window: HWND) -> Option<Rect> {
        Some(client_rect(window))
    }
}

#[cfg(windows)]
mod tree {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Accessibility::{
        CUIAutomation, IUIAutomation, IUIAutomationElement, TreeScope_Subtree,
        UIA_ButtonControlTypeId, UIA_CheckBoxControlTypeId, UIA_ComboBoxControlTypeId,
        UIA_EditControlTypeId, UIA_HyperlinkControlTypeId, UIA_ListItemControlTypeId,
        UIA_MenuItemControlTypeId, UIA_RadioButtonControlTypeId, UIA_SliderControlTypeId,
        UIA_TabItemControlTypeId, UIA_TextControlTypeId, UIA_TreeItemControlTypeId,
    };

    use super::{Target, TARGET_CAP};
    use crate::channel::guardrail::Rect;
    use crate::error::AppError;

    /// Walk the accessibility tree and return what can be acted on.
    pub fn walk(window: HWND) -> Result<(Vec<Target>, Rect), AppError> {
        unsafe {
            // A failure here means COM was already initialised on this thread
            // with a different model, which is fine — the automation object does
            // not care which one.
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);

            let automation: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)
                .map_err(|error| {
                    AppError::message(format!("could not start UI Automation: {error}"))
                })?;

            let root = automation.ElementFromHandle(window).map_err(|error| {
                AppError::message(format!("could not read the window's tree: {error}"))
            })?;

            let condition = automation
                .CreateTrueCondition()
                .map_err(|error| AppError::message(format!("could not build a query: {error}")))?;

            let found = root
                .FindAll(TreeScope_Subtree, &condition)
                .map_err(|error| AppError::message(format!("could not walk the tree: {error}")))?;

            let count = found.Length().unwrap_or(0).max(0) as usize;
            let mut targets = Vec::new();

            for index in 0..count.min(TARGET_CAP) {
                let Ok(element) = found.GetElement(index as i32) else {
                    continue;
                };
                let Some(target) = target_of(&element, targets.len() + 1) else {
                    continue;
                };
                targets.push(target);
            }

            let stage = super::find::client_rect(window);
            Ok((targets, stage))
        }
    }

    fn target_of(element: &IUIAutomationElement, index: usize) -> Option<Target> {
        let name = unsafe { element.CurrentName() }
            .map(|value| value.to_string())
            .unwrap_or_default();
        if name.trim().is_empty() {
            return None;
        }

        let bounds = unsafe { element.CurrentBoundingRectangle() }.ok()?;
        let width = f64::from(bounds.right - bounds.left);
        let height = f64::from(bounds.bottom - bounds.top);
        if width < 1.0 || height < 1.0 {
            return None;
        }

        let control = unsafe { element.CurrentControlType() }.ok()?;

        Some(Target {
            id: format!("e{index}"),
            role: role_of(control).to_string(),
            label: name.trim().to_string(),
            rect: Rect {
                x: f64::from(bounds.left),
                y: f64::from(bounds.top),
                w: width,
                h: height,
            },
        })
    }

    // The constant names come from the Windows SDK and are not upper case, so
    // matching on them is the only way to read this table.
    #[allow(non_upper_case_globals)]
    fn role_of(control: windows::Win32::UI::Accessibility::UIA_CONTROLTYPE_ID) -> &'static str {
        match control {
            UIA_ButtonControlTypeId => "button",
            UIA_CheckBoxControlTypeId => "checkbox",
            UIA_ComboBoxControlTypeId => "combobox",
            UIA_EditControlTypeId => "textbox",
            UIA_HyperlinkControlTypeId => "link",
            UIA_ListItemControlTypeId => "listitem",
            UIA_MenuItemControlTypeId => "menuitem",
            UIA_RadioButtonControlTypeId => "radio",
            UIA_SliderControlTypeId => "slider",
            UIA_TabItemControlTypeId => "tab",
            UIA_TextControlTypeId => "text",
            UIA_TreeItemControlTypeId => "treeitem",
            _ => "element",
        }
    }

}

#[cfg(windows)]
mod input {
    use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
    use windows::Win32::Graphics::Gdi::ScreenToClient;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        MapVirtualKeyW, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT,
        KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MAPVK_VK_TO_VSC, MOUSEINPUT,
        MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN,
        MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
        MOUSEEVENTF_WHEEL, MOUSE_EVENT_FLAGS, VIRTUAL_KEY,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetGUIThreadInfo, GetSystemMetrics, GetWindowThreadProcessId, PostMessageW, GUITHREADINFO,
        SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, WM_CHAR,
        WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP,
        WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_RBUTTONUP,
    };

    use super::Button;
    use crate::error::AppError;

    pub use super::find::focus;

    /// Where a screen point lands in the normalised space `SendInput` wants.
    ///
    /// `MOUSEEVENTF_ABSOLUTE` is not pixels: it is 0..65535 across the whole
    /// virtual screen, which on a multi-monitor setup starts at a negative
    /// origin. Getting this wrong clicks the wrong monitor, so the origin is
    /// read rather than assumed.
    fn normalise(x: f64, y: f64) -> (i32, i32) {
        unsafe {
            let left = GetSystemMetrics(SM_XVIRTUALSCREEN) as f64;
            let top = GetSystemMetrics(SM_YVIRTUALSCREEN) as f64;
            let width = GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1) as f64;
            let height = GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1) as f64;
            (
                (((x - left) * 65535.0) / width).round() as i32,
                (((y - top) * 65535.0) / height).round() as i32,
            )
        }
    }

    fn mouse(flags: MOUSE_EVENT_FLAGS, dx: i32, dy: i32, data: i32) -> INPUT {
        INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx,
                    dy,
                    mouseData: data as u32,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    fn key(virtual_key: u16, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(virtual_key),
                    wScan: scan,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    fn send(events: &[INPUT]) -> Result<(), AppError> {
        let sent = unsafe { SendInput(events, std::mem::size_of::<INPUT>() as i32) };
        if sent as usize != events.len() {
            return Err(AppError::message(
                "the system refused the input — another process may be holding the desktop",
            ));
        }
        Ok(())
    }

    pub fn click(x: f64, y: f64, button: Button, count: u32) -> Result<(), AppError> {
        let (dx, dy) = normalise(x, y);
        let (down, up) = match button {
            Button::Left => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP),
            Button::Right => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP),
            Button::Middle => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP),
        };
        send(&[mouse(MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE, dx, dy, 0)])?;
        for _ in 0..count.max(1) {
            send(&[
                mouse(down | MOUSEEVENTF_ABSOLUTE, dx, dy, 0),
                mouse(up | MOUSEEVENTF_ABSOLUTE, dx, dy, 0),
            ])?;
        }
        Ok(())
    }

    /// A screen point in the window's own client coordinates.
    ///
    /// Posted messages carry client coordinates, unlike `SendInput`, which works
    /// in screen space. The rectangles the tree walk reports are screen space, so
    /// the conversion has to happen here rather than at the call site.
    fn client_point(window: HWND, x: f64, y: f64) -> (i32, i32) {
        let mut point = POINT {
            x: x.round() as i32,
            y: y.round() as i32,
        };
        let _ = unsafe { ScreenToClient(window, &mut point) };
        (point.x, point.y)
    }

    fn point_param(x: i32, y: i32) -> LPARAM {
        LPARAM((((y as u32) << 16) | ((x as u32) & 0xffff)) as isize)
    }

    /// The window that should receive a key, which is not always the one asked.
    ///
    /// Keys go to whatever holds focus inside the target's thread. `GetFocus`
    /// only answers for the calling thread's windows, so this asks the thread
    /// itself through `GetGUIThreadInfo`, which works across processes.
    fn key_target(window: HWND) -> HWND {
        let thread = unsafe { GetWindowThreadProcessId(window, None) };
        let mut info = GUITHREADINFO {
            cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        if unsafe { GetGUIThreadInfo(thread, &mut info) }.is_ok() && !info.hwndFocus.is_invalid() {
            return info.hwndFocus;
        }
        window
    }

    /// Click by posting window messages instead of injecting hardware input.
    ///
    /// `SendInput` only reaches the active input desktop, so a window on a hidden
    /// desktop never sees it — measured, not assumed. Messages go to the window's
    /// own queue and arrive whatever desktop it is on. The trade is real: an app
    /// that reads raw input, a game polling DirectInput for instance, never looks
    /// at these, so this path cannot drive one.
    pub fn post_click(
        window: HWND,
        x: f64,
        y: f64,
        button: Button,
        count: u32,
    ) -> Result<(), AppError> {
        let (cx, cy) = client_point(window, x, y);
        let (down, up) = match button {
            Button::Left => (WM_LBUTTONDOWN, WM_LBUTTONUP),
            Button::Right => (WM_RBUTTONDOWN, WM_RBUTTONUP),
            Button::Middle => (WM_MBUTTONDOWN, WM_MBUTTONUP),
        };

        for _ in 0..count.max(1) {
            unsafe {
                PostMessageW(Some(window), down, WPARAM(1), point_param(cx, cy))
                    .map_err(|error| AppError::message(format!("could not post a click: {error}")))?;
                PostMessageW(Some(window), up, WPARAM(0), point_param(cx, cy))
                    .map_err(|error| AppError::message(format!("could not post a click: {error}")))?;
            }
        }
        Ok(())
    }

    /// Hold a key by posting the down and up messages around the wait.
    pub fn post_hold(
        window: HWND,
        name: &str,
        frames: Option<u32>,
        ms: Option<u64>,
    ) -> Result<(), AppError> {
        let virtual_key = virtual_key_of(name);
        let scan = unsafe { MapVirtualKeyW(u32::from(virtual_key), MAPVK_VK_TO_VSC) };
        let target = key_target(window);
        let held = ms.unwrap_or_else(|| u64::from(frames.unwrap_or(1)) * 16);

        unsafe {
            PostMessageW(
                Some(target),
                WM_KEYDOWN,
                WPARAM(virtual_key as usize),
                LPARAM(1 | ((scan as isize) << 16)),
            )
            .map_err(|error| AppError::message(format!("could not post a key: {error}")))?;
        }
        std::thread::sleep(std::time::Duration::from_millis(held));
        unsafe {
            PostMessageW(
                Some(target),
                WM_KEYUP,
                WPARAM(virtual_key as usize),
                LPARAM(1 | ((scan as isize) << 16) | (1 << 30) | (1 << 31)),
            )
            .map_err(|error| AppError::message(format!("could not post a key: {error}")))?;
        }
        Ok(())
    }

    /// Drag by posting the button down, a run of moves, and the button up.
    pub fn post_drag(window: HWND, x1: f64, y1: f64, x2: f64, y2: f64) -> Result<(), AppError> {
        let (ax, ay) = client_point(window, x1, y1);
        let (bx, by) = client_point(window, x2, y2);

        unsafe {
            PostMessageW(Some(window), WM_LBUTTONDOWN, WPARAM(1), point_param(ax, ay))
                .map_err(|error| AppError::message(format!("could not post a drag: {error}")))?;
        }
        // A drag that teleports is a drag no drag handler sees.
        for step in 1..=12 {
            let t = f64::from(step) / 12.0;
            let (mx, my) = client_point(window, x1 + (x2 - x1) * t, y1 + (y2 - y1) * t);
            unsafe {
                PostMessageW(Some(window), WM_MOUSEMOVE, WPARAM(1), point_param(mx, my))
                    .map_err(|error| AppError::message(format!("could not post a drag: {error}")))?;
            }
        }
        unsafe {
            PostMessageW(Some(window), WM_LBUTTONUP, WPARAM(0), point_param(bx, by))
                .map_err(|error| AppError::message(format!("could not post a drag: {error}")))?;
        }
        Ok(())
    }

    /// Scroll by posting a wheel message.
    ///
    /// `WM_MOUSEWHEEL` is the one mouse message that carries screen coordinates
    /// rather than client ones, so this does not convert.
    pub fn post_scroll(window: HWND, x: f64, y: f64, dy: f64) -> Result<(), AppError> {
        let delta = (dy * 120.0).round() as i32;
        let screen = point_param(x.round() as i32, y.round() as i32);
        unsafe {
            PostMessageW(
                Some(window),
                WM_MOUSEWHEEL,
                WPARAM(((delta as u32 & 0xffff) << 16) as usize),
                screen,
            )
            .map_err(|error| AppError::message(format!("could not post a scroll: {error}")))?;
        }
        Ok(())
    }

    /// Type by posting one `WM_CHAR` per UTF-16 unit.
    pub fn post_text(window: HWND, text: &str) -> Result<(), AppError> {
        let target = key_target(window);
        for unit in text.encode_utf16() {
            unsafe {
                PostMessageW(Some(target), WM_CHAR, WPARAM(unit as usize), LPARAM(1))
                    .map_err(|error| {
                        AppError::message(format!("could not post a character: {error}"))
                    })?;
            }
        }
        Ok(())
    }

    pub fn drag(x1: f64, y1: f64, x2: f64, y2: f64) -> Result<(), AppError> {        let (ax, ay) = normalise(x1, y1);
        send(&[mouse(MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE, ax, ay, 0)])?;
        send(&[mouse(MOUSEEVENTF_LEFTDOWN | MOUSEEVENTF_ABSOLUTE, ax, ay, 0)])?;
        // A drag that teleports is a drag no drag handler sees.
        for step in 1..=12 {
            let t = f64::from(step) / 12.0;
            let (dx, dy) = normalise(x1 + (x2 - x1) * t, y1 + (y2 - y1) * t);
            send(&[mouse(MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE, dx, dy, 0)])?;
        }
        let (bx, by) = normalise(x2, y2);
        send(&[mouse(MOUSEEVENTF_LEFTUP | MOUSEEVENTF_ABSOLUTE, bx, by, 0)])?;
        Ok(())
    }

    pub fn scroll(x: f64, y: f64, _dx: f64, dy: f64) -> Result<(), AppError> {
        let (ax, ay) = normalise(x, y);
        send(&[mouse(MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE, ax, ay, 0)])?;
        // One wheel notch is 120 units, which is what every Windows app expects.
        let notches = (-dy / 120.0).round() as i32;
        if notches != 0 {
            send(&[mouse(MOUSEEVENTF_WHEEL, 0, 0, notches * 120)])?;
        }
        Ok(())
    }

    pub fn type_text(text: &str) -> Result<(), AppError> {
        for character in text.chars() {
            let mut buffer = [0u16; 2];
            for unit in character.encode_utf16(&mut buffer) {
                send(&[
                    key(0, *unit, KEYEVENTF_UNICODE),
                    key(0, *unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP),
                ])?;
            }
        }
        Ok(())
    }

    pub fn hold(key_name: &str, frames: Option<u32>, ms: Option<u64>) -> Result<(), AppError> {
        let virtual_key = virtual_key_of(key_name);
        send(&[key(virtual_key, 0, KEYBD_EVENT_FLAGS(0))])?;
        match (frames, ms) {
            (Some(frames), _) => std::thread::sleep(std::time::Duration::from_millis(
                u64::from(frames) * 16,
            )),
            (None, Some(ms)) => std::thread::sleep(std::time::Duration::from_millis(ms)),
            (None, None) => std::thread::sleep(std::time::Duration::from_millis(16)),
        }
        send(&[key(virtual_key, 0, KEYEVENTF_KEYUP)])?;
        Ok(())
    }

    /// The virtual key code for a name.
    ///
    /// The names are the same ones the webview backend takes, so a sequence
    /// written against one backend reads the same against the other.
    fn virtual_key_of(name: &str) -> u16 {
        match name {
            "ArrowLeft" => 0x25,
            "ArrowUp" => 0x26,
            "ArrowRight" => 0x27,
            "ArrowDown" => 0x28,
            "Enter" => 0x0D,
            "Escape" => 0x1B,
            "Tab" => 0x09,
            "Space" => 0x20,
            "Backspace" => 0x08,
            "Shift" => 0x10,
            "Control" => 0x11,
            "Alt" => 0x12,
            other => {
                let mut characters = other.chars();
                match (characters.next(), characters.next()) {
                    (Some(letter), None) if letter.is_ascii_alphabetic() => {
                        letter.to_ascii_uppercase() as u16
                    }
                    (Some(digit), None) if digit.is_ascii_digit() => digit as u16,
                    _ => 0,
                }
            }
        }
    }
}

#[cfg(windows)]
mod debug {
    //! The `OutputDebugString` buffer.
    //!
    //! A Win32 program's diagnostics go here, and `DebugView` is the usual way to
    //! read them. The protocol is two named events and a named shared section:
    //! the reader signals `DBWIN_BUFFER_READY`, waits for `DBWIN_DATA_READY`,
    //! reads a process id and a string, and signals ready again. Only one reader
    //! may hold it at a time, which is why a failure to open it is not fatal —
    //! the user may already have `DebugView` running.

    use std::sync::mpsc::{channel, Receiver, Sender};
    use std::sync::{Arc, Mutex};

    use windows::core::s;
    use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
    use windows::Win32::System::Memory::{
        CreateFileMappingA, MapViewOfFile, UnmapViewOfFile, FILE_MAP_READ, PAGE_READWRITE,
    };
    use windows::Win32::System::Threading::{CreateEventA, SetEvent, WaitForSingleObject};

    const BUFFER_SIZE: usize = 4096;

    pub struct Reader {
        stop: Arc<Mutex<bool>>,
        lines: Receiver<String>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl Reader {
        pub fn start() -> Result<Self, String> {
            let (sender, lines) = channel();
            let stop = Arc::new(Mutex::new(false));
            let flag = stop.clone();

            let thread = std::thread::spawn(move || run(sender, flag));
            Ok(Self {
                stop,
                lines,
                thread: Some(thread),
            })
        }

        pub fn drain(&mut self) -> Vec<String> {
            let mut collected = Vec::new();
            while let Ok(line) = self.lines.try_recv() {
                collected.push(line);
            }
            collected
        }

        pub fn stop(&mut self) {
            if let Ok(mut flag) = self.stop.lock() {
                *flag = true;
            }
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    fn run(sender: Sender<String>, stop: Arc<Mutex<bool>>) {
        unsafe {
            let Ok(ready) = CreateEventA(None, false, false, s!("DBWIN_BUFFER_READY")) else {
                return;
            };
            let Ok(data) = CreateEventA(None, false, false, s!("DBWIN_DATA_READY")) else {
                let _ = CloseHandle(ready);
                return;
            };
            let Ok(mapping) = CreateFileMappingA(
                HANDLE::default(),
                None,
                PAGE_READWRITE,
                0,
                BUFFER_SIZE as u32,
                s!("DBWIN_BUFFER"),
            ) else {
                let _ = CloseHandle(ready);
                let _ = CloseHandle(data);
                return;
            };
            let view = MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, BUFFER_SIZE);
            if view.Value.is_null() {
                let _ = CloseHandle(ready);
                let _ = CloseHandle(data);
                let _ = CloseHandle(mapping);
                return;
            }

            let bytes = view.Value as *const u8;
            loop {
                if stop.lock().map(|flag| *flag).unwrap_or(true) {
                    break;
                }
                let _ = SetEvent(ready);
                // A short wait rather than an infinite one, so the stop flag is
                // noticed even when nothing is being written.
                if WaitForSingleObject(data, 100) != WAIT_OBJECT_0 {
                    continue;
                }

                // The first four bytes are the writing process id; the rest is a
                // null-terminated ANSI string.
                let text = std::slice::from_raw_parts(bytes.add(4), BUFFER_SIZE - 4);
                let end = text.iter().position(|byte| *byte == 0).unwrap_or(text.len());
                let line = String::from_utf8_lossy(&text[..end]).trim().to_string();
                if !line.is_empty() && sender.send(line).is_err() {
                    break;
                }
            }

            let _ = UnmapViewOfFile(view);
            let _ = CloseHandle(mapping);
            let _ = CloseHandle(data);
            let _ = CloseHandle(ready);
        }
    }
}

#[cfg(all(windows, test))]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, RegisterClassW, ShowWindow, SW_SHOW, WM_LBUTTONDOWN,
        WNDCLASSW, WS_OVERLAPPEDWINDOW,
    };

    use super::*;

    static CLICKS: AtomicUsize = AtomicUsize::new(0);

    /// Hardware input goes to whatever is on top at a screen coordinate, so two
    /// tests clicking at once fight over the same pixels. The desktop tests take
    /// this; the hidden-desktop ones do not need it.
    static DESKTOP: std::sync::Mutex<()> = std::sync::Mutex::new(());

    extern "system" fn count_clicks(
        window: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if message == WM_LBUTTONDOWN {
            CLICKS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { DefWindowProcW(window, message, wparam, lparam) }
    }

    /// Make a window on whatever desktop the calling thread is bound to.
    fn make_window(title: &str) -> isize {
        unsafe {
            let class = WNDCLASSW {
                lpfnWndProc: Some(count_clicks),
                lpszClassName: w!("VibeDeskChannelProbe"),
                ..Default::default()
            };
            let _ = RegisterClassW(&class);

            let wide: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
            let window = CreateWindowExW(
                Default::default(),
                w!("VibeDeskChannelProbe"),
                PCWSTR(wide.as_ptr()),
                WS_OVERLAPPEDWINDOW,
                100,
                100,
                400,
                300,
                None,
                None,
                None,
                None,
            )
            .expect("the probe window has to be created");

            let _ = ShowWindow(window, SW_SHOW);
            window.0 as isize
        }
    }

    /// A window that fills its client area with one colour.
    ///
    /// A capture test needs a window whose pixels are known, so that "the
    /// picture is not blank" can be checked against a value rather than against
    /// a guess. The colour is the window class's background brush, which is what
    /// Windows paints the client area with and what `PrintWindow` renders.
    fn paint_window(title: &str, colour: [u8; 3]) -> isize {
        use windows::Win32::Foundation::COLORREF;
        use windows::Win32::Graphics::Gdi::{CreateSolidBrush, UpdateWindow};

        extern "system" fn plain(
            window: HWND,
            message: u32,
            wparam: WPARAM,
            lparam: LPARAM,
        ) -> LRESULT {
            unsafe { DefWindowProcW(window, message, wparam, lparam) }
        }

        // The class carries the brush, and a class name can only be registered
        // once per process. A fixed name would make the second test's window
        // paint the first test's colour, so each call gets its own class.
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let class_name: Vec<u16> = format!(
            "VibeDeskChannelPaint{} ",
            NEXT.fetch_add(1, Ordering::Relaxed)
        )
        .encode_utf16()
        .collect();

        unsafe {
            let brush = CreateSolidBrush(COLORREF(
                (colour[0] as u32) | ((colour[1] as u32) << 8) | ((colour[2] as u32) << 16),
            ));
            let class = WNDCLASSW {
                lpfnWndProc: Some(plain),
                hbrBackground: brush,
                lpszClassName: PCWSTR(class_name.as_ptr()),
                ..Default::default()
            };
            let _ = RegisterClassW(&class);

            let wide: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
            let window = CreateWindowExW(
                Default::default(),
                PCWSTR(class_name.as_ptr()),
                PCWSTR(wide.as_ptr()),
                WS_OVERLAPPEDWINDOW,
                100,
                100,
                320,
                240,
                None,
                None,
                None,
                None,
            )
            .expect("the paint window has to be created");

            let _ = ShowWindow(window, SW_SHOW);
            // Force the first paint, so the redirection surface exists before
            // anything asks for a picture of it.
            let _ = UpdateWindow(window);
            window.0 as isize
        }
    }

    /// A window with real child buttons, so the guardrail reads real labels.
    fn make_button_window(title: &str, labels: &[&str]) -> isize {
        use windows::Win32::UI::WindowsAndMessaging::{WS_CHILD, WS_TABSTOP, WS_VISIBLE};

        unsafe {
            let class = WNDCLASSW {
                lpfnWndProc: Some(count_clicks),
                lpszClassName: w!("VibeDeskChannelButtons"),
                ..Default::default()
            };
            let _ = RegisterClassW(&class);

            let wide: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
            let window = CreateWindowExW(
                Default::default(),
                w!("VibeDeskChannelButtons"),
                PCWSTR(wide.as_ptr()),
                WS_OVERLAPPEDWINDOW,
                100,
                100,
                400,
                300,
                None,
                None,
                None,
                None,
            )
            .expect("the button window has to be created");

            for (index, label) in labels.iter().enumerate() {
                let text: Vec<u16> = label.encode_utf16().chain(std::iter::once(0)).collect();
                let _ = CreateWindowExW(
                    Default::default(),
                    w!("BUTTON"),
                    PCWSTR(text.as_ptr()),
                    WS_CHILD | WS_VISIBLE | WS_TABSTOP,
                    20,
                    20 + (index as i32) * 40,
                    140,
                    30,
                    Some(window),
                    None,
                    None,
                    None,
                );
            }

            let _ = ShowWindow(window, SW_SHOW);
            window.0 as isize
        }
    }

    fn probe_spec(title: &str) -> WindowSpec {
        WindowSpec {
            title: Some(title.to_string()),
            ..Default::default()
        }
    }

    /// Deliver whatever is queued on this thread's desktop.
    ///
    /// A window procedure only runs when the thread that owns the window pumps.
    /// The desktop host runs jobs on the calling thread, so a test that owns a
    /// window has to pump the way the app's main loop does.
    fn pump() {
        use windows::Win32::UI::WindowsAndMessaging::{
            DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE,
        };
        let mut message = MSG::default();
        while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
            unsafe {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }

    /// Wait for the window procedure to run, which happens on the owning
    /// thread's next pump rather than inside the call that queued the message.
    fn clicks_after(before: usize) -> usize {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while CLICKS.load(Ordering::Relaxed) == before && std::time::Instant::now() < deadline {
            pump();
            std::thread::sleep(Duration::from_millis(5));
        }
        CLICKS.load(Ordering::Relaxed)
    }

    #[test]
    fn a_window_on_a_hidden_desktop_is_captured_with_its_own_colour() {
        // The claim that makes `PrintWindow` the right mechanism rather than a
        // screen read: a window on a desktop nobody is looking at still renders
        // itself, because the request is a message to the window and not a read
        // of whatever is in front.
        const PAINT: [u8; 3] = [0xC0, 0x40, 0x20];

        let host = Host::hidden().unwrap();
        let handle = host.run(|| paint_window("hidden capture probe", PAINT)).unwrap();

        let (base64, width, height) = host
            .run(move || crate::channel::capture::window_png(hwnd_of(handle)))
            .unwrap()
            .expect("a painted window has to be capturable")
            .expect("a painted window has a client area");

        assert!(width > 0 && height > 0, "got {width}x{height}");
        let png = decode_base64(&base64);
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "it has to be a PNG");

        let rgba = host
            .run(move || crate::channel::capture::render_for_test(hwnd_of(handle), width, height))
            .unwrap()
            .unwrap();
        let middle = ((height / 2) as usize * width as usize + (width / 2) as usize) * 4;
        let pixel = &rgba[middle..middle + 3];
        for (got, want) in pixel.iter().zip(PAINT.iter()) {
            assert!(
                (*got as i32 - *want as i32).abs() <= 8,
                "the hidden window paints {PAINT:?} but the capture has {pixel:?}"
            );
        }
    }

    #[test]
    fn a_window_on_a_hidden_desktop_is_found_and_clicked() {
        let host = Host::hidden().unwrap();
        let handle = host.run(|| make_window("hidden click probe")).unwrap();

        let spec = probe_spec("hidden click probe");
        let found = host.run(move || find::search(&spec)).unwrap();
        assert_eq!(
            found,
            Some(handle),
            "enumeration on the bound thread has to see the window it made"
        );

        let rect = host.run(move || find::client_rect(hwnd_of(handle))).unwrap();
        let (x, y) = (rect.x + rect.w / 2.0, rect.y + rect.h / 2.0);

        let before = CLICKS.load(Ordering::Relaxed);
        host.run(move || input::post_click(hwnd_of(handle), x, y, Button::Left, 1))
            .unwrap()
            .unwrap();
        assert!(
            clicks_after(before) > before,
            "a posted click has to reach the window on the hidden desktop"
        );
    }

    #[test]
    fn a_window_on_a_hidden_desktop_is_not_visible_to_the_users_desktop() {
        let host = Host::hidden().unwrap();
        host.run(|| make_window("hidden invisible probe")).unwrap();

        // The same search, run from the user's desktop, must not find it. This is
        // the whole claim: the test is out of the way, not merely unfocused.
        let spec = probe_spec("hidden invisible probe");
        assert_eq!(find::search(&spec), None);
    }

    #[test]
    fn hardware_input_does_not_reach_a_hidden_desktop() {
        // Measured, not assumed: `SendInput` only reaches the active input
        // desktop, so the cursor does not even move on a hidden one. This test
        // exists so nobody "simplifies" the message path back into `SendInput`.
        let host = Host::hidden().unwrap();
        let handle = host.run(|| make_window("hidden hardware probe")).unwrap();
        let rect = host.run(move || find::client_rect(hwnd_of(handle))).unwrap();
        let (x, y) = (rect.x + rect.w / 2.0, rect.y + rect.h / 2.0);

        let before = CLICKS.load(Ordering::Relaxed);
        // It either refuses outright or reports success and delivers nothing.
        // Both are the same answer, so the assertion is on the delivery.
        let accepted = host
            .run(move || input::click(x, y, Button::Left, 1))
            .unwrap()
            .is_ok();
        let reached = accepted && clicks_after(before) > before;

        assert!(
            !reached,
            "hardware input reaching a hidden desktop would mean the message path is unnecessary"
        );
    }

    #[test]
    fn a_window_on_the_desktop_comes_to_the_front_when_asked() {
        let _desktop = DESKTOP.lock().unwrap_or_else(|error| error.into_inner());
        let host = Host::desktop();
        let handle = host.run(|| make_window("desktop focus probe")).unwrap();
        assert!(
            host.run(move || input::focus(hwnd_of(handle))).unwrap(),
            "focus has to report whether the window actually came forward"
        );
    }

    /// One JSON-RPC call, the way the agent makes it.
    fn call(name: &str, arguments: serde_json::Value) -> serde_json::Value {
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": name, "arguments": arguments },
        });
        crate::channel::mcp::dispatch(&request).expect("the server has to answer")
    }

    fn text_of(reply: &serde_json::Value) -> String {
        reply["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    #[test]
    fn the_tool_surface_drives_a_window_end_to_end() {
        let _desktop = DESKTOP.lock().unwrap_or_else(|error| error.into_inner());
        // The whole path the agent takes: open, look, act, expect, close, over
        // the same JSON-RPC the MCP server speaks.
        let host = Host::desktop();
        let handle = host.run(|| make_window("tool surface probe")).unwrap();
        let rect = host.run(move || find::client_rect(hwnd_of(handle))).unwrap();
        let (x, y) = (rect.x + rect.w / 2.0, rect.y + rect.h / 2.0);

        let opened = call(
            "channel_open",
            serde_json::json!({ "backend": "window", "title": "tool surface probe" }),
        );
        let body: serde_json::Value =
            serde_json::from_str(&text_of(&opened)).expect("open has to answer with json");
        let id = body["id"].as_str().expect("open has to name the channel");
        assert_eq!(body["capabilities"]["input"], "hardware");
        assert_eq!(body["capabilities"]["foreground"], true);

        let before = CLICKS.load(Ordering::Relaxed);
        let acted = call(
            "channel_act",
            serde_json::json!({
                "id": id,
                "actions": [{ "kind": "click", "target": { "x": x, "y": y } }],
            }),
        );
        let outcome: serde_json::Value =
            serde_json::from_str(&text_of(&acted)).expect("act has to answer with json");
        assert_eq!(outcome["ok"], true, "the click has to be accepted");
        assert!(
            clicks_after(before) > before,
            "the click has to reach the window through the tool surface"
        );

        let checked = call("channel_expect", serde_json::json!({ "id": id }));
        assert_eq!(
            checked["result"]["isError"].as_bool().unwrap_or(false),
            false,
            "a clean run has to pass the assertion"
        );

        let closed = call("channel_close", serde_json::json!({ "id": id }));
        assert!(
            !closed["result"]["isError"].as_bool().unwrap_or(false),
            "close has to succeed"
        );
    }

    fn ref_for(body: &serde_json::Value, label: &str) -> String {
        body["look"]["targets"]
            .as_array()
            .expect("a look has targets")
            .iter()
            .find(|target| target["label"].as_str() == Some(label))
            .and_then(|target| target["id"].as_str())
            .unwrap_or_else(|| panic!("no target labelled {label} in {body}"))
            .to_string()
    }

    fn body_of(reply: &serde_json::Value) -> serde_json::Value {
        let text = text_of(reply);
        serde_json::from_str(&text)
            .unwrap_or_else(|error| panic!("the tool has to answer with json: {error} — {text}"))
    }

    // The tests below check the claims the `channeler*` skills make. A skill that
    // says something false is worse than no skill, so each claim is asserted
    // against the real tool surface rather than trusted.

    #[test]
    fn a_destructive_pair_is_blocked_and_the_flag_lets_it_through() {
        let _desktop = DESKTOP.lock().unwrap_or_else(|error| error.into_inner());
        let host = Host::desktop();
        host.run(|| make_button_window("guardrail probe", &["Delete", "Confirm"]))
            .unwrap();

        let opened = call(
            "channel_open",
            serde_json::json!({ "backend": "window", "title": "guardrail probe" }),
        );
        let body = body_of(&opened);
        let id = body["id"].as_str().unwrap().to_string();
        let delete = ref_for(&body, "Delete");
        let confirm = ref_for(&body, "Confirm");

        let pair = serde_json::json!([
            { "kind": "click", "target": delete },
            { "kind": "click", "target": confirm },
        ]);

        let blocked = body_of(&call(
            "channel_act",
            serde_json::json!({ "id": id, "actions": pair }),
        ));
        assert_eq!(blocked["ok"], false, "the pair has to be refused");
        assert_eq!(blocked["blocked"]["verdict"], "block");
        assert_eq!(blocked["blocked"]["rule"], "destructive");

        let detail = blocked["blocked"]["detail"].as_str().unwrap_or_default();
        for expected in ["step 1", "step 2", "Delete", "Confirm"] {
            assert!(
                detail.contains(expected),
                "the refusal has to name {expected}: {detail}"
            );
        }

        let allowed = body_of(&call(
            "channel_act",
            serde_json::json!({ "id": id, "actions": pair, "allow_destructive": true }),
        ));
        assert_eq!(
            allowed["ok"], true,
            "allow_destructive has to let the pair through: {allowed}"
        );

        call("channel_close", serde_json::json!({ "id": id }));
    }

    #[test]
    fn a_point_outside_the_stage_is_refused() {
        let _desktop = DESKTOP.lock().unwrap_or_else(|error| error.into_inner());
        let host = Host::desktop();
        host.run(|| make_window("scope probe")).unwrap();

        let opened = call(
            "channel_open",
            serde_json::json!({ "backend": "window", "title": "scope probe" }),
        );
        let body = body_of(&opened);
        let id = body["id"].as_str().unwrap().to_string();

        let refused = body_of(&call(
            "channel_act",
            serde_json::json!({
                "id": id,
                "actions": [{ "kind": "click", "target": { "x": -5000.0, "y": -5000.0 } }],
            }),
        ));
        assert_eq!(refused["ok"], false);
        assert_eq!(refused["stopped_at"], 0);
        // blocked is the destructive pre-check only. A scope refusal is a
        // failed step, which is where the agent has to look for it.
        assert_eq!(refused["blocked"], serde_json::Value::Null);
        let detail = refused["results"][0]["detail"].as_str().unwrap_or_default();
        assert!(
            detail.contains("outside the stage"),
            "the refusal has to say the point is outside: {detail}"
        );

        call("channel_close", serde_json::json!({ "id": id }));
    }

    #[test]
    fn a_sequence_stops_at_the_first_failure_and_names_it() {
        // `process` has no click, so a click is the first failure.
        let opened = call(
            "channel_open",
            serde_json::json!({
                "backend": "process",
                "command": "cmd",
                "args": ["/c", "exit", "0"],
            }),
        );
        let body = body_of(&opened);
        let id = body["id"].as_str().unwrap().to_string();

        let actions = body["capabilities"]["actions"].as_array().unwrap();
        assert!(
            !actions.iter().any(|kind| kind == "click"),
            "a process channel must not claim to click: {actions:?}"
        );

        let outcome = body_of(&call(
            "channel_act",
            serde_json::json!({
                "id": id,
                "actions": [
                    { "kind": "click", "target": { "x": 1.0, "y": 1.0 } },
                    { "kind": "wait", "ms": 1 },
                ],
            }),
        ));
        assert_eq!(outcome["ok"], false);
        assert_eq!(
            outcome["stopped_at"], 0,
            "the first step is the one that failed"
        );
        assert_eq!(
            outcome["results"].as_array().unwrap().len(),
            1,
            "the step after a failure must not run"
        );

        call("channel_close", serde_json::json!({ "id": id }));
    }

    #[test]
    fn a_failing_assertion_is_a_successful_tool_call() {
        // The trap the skills warn about: `ok: false` with `isError: false`.
        let opened = call(
            "channel_open",
            serde_json::json!({
                "backend": "process",
                "command": "cmd",
                "args": ["/c", "exit", "3"],
            }),
        );
        let body = body_of(&opened);
        let id = body["id"].as_str().unwrap().to_string();

        // Give the child time to exit so the non-zero code is recorded. This
        // cannot go through channel_act: a sequence drains the issues its own
        // steps produced, so the exit code would be consumed before expect.
        std::thread::sleep(Duration::from_millis(400));

        let checked = call("channel_expect", serde_json::json!({ "id": id }));
        assert_eq!(
            checked["result"]["isError"].as_bool().unwrap_or(false),
            false,
            "a failed assertion is still a successful call"
        );

        let verdict = body_of(&checked);
        assert_eq!(verdict["ok"], false, "a non-zero exit has to fail the run");
        assert!(
            !verdict["issues"].as_array().unwrap().is_empty(),
            "the failure has to name an issue: {verdict}"
        );

        call("channel_close", serde_json::json!({ "id": id }));
    }

    #[test]
    fn a_window_channel_captures_the_colour_the_window_paints() {
        // The claim the `channeler-open` skill makes: a `window` channel can
        // take a picture. Checked against a window whose pixels are known, so
        // "it returned something" is not mistaken for "it returned the window".
        const PAINT: [u8; 3] = [0x20, 0x80, 0xC0];

        let _desktop = DESKTOP.lock().unwrap_or_else(|error| error.into_inner());
        let host = Host::desktop();
        let handle = host.run(|| paint_window("capture probe", PAINT)).unwrap();

        let opened = call(
            "channel_open",
            serde_json::json!({ "backend": "window", "title": "capture probe" }),
        );
        let body = body_of(&opened);
        let id = body["id"].as_str().unwrap().to_string();
        assert_eq!(body["capabilities"]["capture"], true);

        let looked = call(
            "channel_look",
            serde_json::json!({ "id": id, "capture": true }),
        );
        let look = body_of(&looked);
        assert_eq!(look["capture_type"], "image/png");

        let png = decode_base64(
            look["capture"]
                .as_str()
                .expect("a capture was asked for and has to be there"),
        );
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "it has to be a PNG");
        let width = u32::from_be_bytes(png[16..20].try_into().unwrap());
        let height = u32::from_be_bytes(png[20..24].try_into().unwrap());
        assert!(width > 0 && height > 0, "got {width}x{height}");

        // The pixels come from the same render path the encoder used. Decoding
        // the PNG here would need a decoder this crate does not carry, and what
        // matters is that the window's own colour came back rather than a black
        // rectangle.
        let hwnd = hwnd_of(handle);
        let rgba = crate::channel::capture::render_for_test(hwnd, width, height).unwrap();
        let middle = ((height / 2) as usize * width as usize + (width / 2) as usize) * 4;
        let pixel = &rgba[middle..middle + 3];
        for (got, want) in pixel.iter().zip(PAINT.iter()) {
            assert!(
                (*got as i32 - *want as i32).abs() <= 8,
                "the window paints {PAINT:?} but the capture has {pixel:?}"
            );
        }

        call("channel_close", serde_json::json!({ "id": id }));
    }

    fn decode_base64(text: &str) -> Vec<u8> {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = Vec::new();
        let mut acc = 0u32;
        let mut bits = 0u32;
        for byte in text.bytes() {
            if byte == b'=' {
                break;
            }
            let Some(value) = ALPHABET.iter().position(|c| *c == byte) else {
                continue;
            };
            acc = (acc << 6) | value as u32;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((acc >> bits) as u8);
            }
        }
        out
    }

    #[test]
    fn a_window_channel_refuses_tick_rather_than_pretending_to_advance_frames() {
        let _desktop = DESKTOP.lock().unwrap_or_else(|error| error.into_inner());
        let host = Host::desktop();
        host.run(|| make_window("tick probe")).unwrap();

        let opened = call(
            "channel_open",
            serde_json::json!({ "backend": "window", "title": "tick probe" }),
        );
        let body = body_of(&opened);
        let id = body["id"].as_str().unwrap().to_string();

        let outcome = body_of(&call(
            "channel_act",
            serde_json::json!({ "id": id, "actions": [{ "kind": "tick", "frames": 2 }] }),
        ));
        let detail = outcome["results"][0]["detail"].as_str().unwrap_or_default();
        assert!(
            detail.contains("does not support Tick"),
            "a window has no frame clock, so tick has to be refused: {detail}"
        );

        call("channel_close", serde_json::json!({ "id": id }));
    }

    #[test]
    fn the_window_backend_reports_the_input_a_hidden_desktop_actually_has() {
        let host = Host::hidden().unwrap();
        let handle = host.run(|| make_window("hidden capability probe")).unwrap();

        let backend = WindowBackend {
            hwnd: hwnd_of(handle),
            title: "hidden capability probe".to_string(),
            host,
            state: Arc::new(Mutex::new(State {
                targets: Vec::new(),
                stage: Rect {
                    x: 0.0,
                    y: 0.0,
                    w: 0.0,
                    h: 0.0,
                },
                issues: Issues::new(ISSUE_CAP),
            })),
            debug: None,
        };

        let capabilities = backend.capabilities();
        assert_eq!(capabilities.host, HostKind::HiddenDesktop);
        assert!(!capabilities.foreground);
        assert_eq!(capabilities.input, InputKind::Synthetic);
    }
}
