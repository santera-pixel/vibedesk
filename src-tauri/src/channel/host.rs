//! Where a channel runs.
//!
//! On Windows a desktop is a *thread* property, not a process one.
//! `SetThreadDesktop` binds the calling thread, and from then on every window it
//! creates, every `EnumWindows` it makes and every `SendInput` it sends belong to
//! that desktop. The binding cannot be undone, so the thread that takes it is
//! spent — which is why a hidden desktop needs a thread of its own rather than a
//! flag on an existing one.
//!
//! That thread is the whole mechanism. A window on a hidden desktop is not
//! visible, does not take the user's focus, and still receives real input,
//! because input is queued per desktop and the desktop does not have to be the
//! one on screen.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::AppError;

/// How long one job may take before the desktop thread is declared stuck.
///
/// A UI Automation walk on a busy window can take a second or two. Thirty is far
/// past that and still short enough that a wedged thread fails the tool call
/// instead of hanging the agent's turn.
const JOB_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the desktop thread gets to bind before the channel gives up.
const BIND_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the desktop thread waits for a job before pumping again.
///
/// `recv_timeout` returns the moment a job is queued, so this is only paid while
/// the channel is idle — and it is what keeps injected input moving.
const PUMP_IDLE: Duration = Duration::from_millis(1);

/// Deliver everything already queued on this thread's desktop.
///
/// Injected input is queued, not delivered: the window procedure only runs when
/// the owning thread pumps. This is that pump.
#[cfg(windows)]
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

/// Where a channel runs, which is what decides whether it takes the foreground.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostKind {
    /// The user's own desktop. Visible, and acting on it steals focus.
    Desktop,
    /// A desktop created for the test. Invisible, and it does not.
    HiddenDesktop,
}

impl HostKind {
    pub fn as_str(self) -> &'static str {
        match self {
            HostKind::Desktop => "desktop",
            HostKind::HiddenDesktop => "hidden-desktop",
        }
    }
}

/// A desktop this process created and owns.
///
/// The handle travels as an `isize` because `HDESK` is a raw pointer and so is
/// not `Send`. It is a process-wide identifier rather than an address into this
/// process's memory, so moving it between threads is sound; what is not sound is
/// using it from a thread that has not bound to it, and `Host` is the only thing
/// that hands it out.
#[cfg(windows)]
struct Desktop {
    handle: isize,
    name: String,
}

#[cfg(windows)]
impl Desktop {
    fn create(name: &str) -> Result<Self, AppError> {
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::GENERIC_ALL;
        use windows::Win32::System::StationsAndDesktops::{CreateDesktopW, DESKTOP_CONTROL_FLAGS};

        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        let handle = unsafe {
            CreateDesktopW(
                PCWSTR(wide.as_ptr()),
                PCWSTR::null(),
                None,
                DESKTOP_CONTROL_FLAGS(0),
                GENERIC_ALL.0,
                None,
            )
        }
        .map_err(|error| AppError::message(format!("could not create a hidden desktop: {error}")))?;

        Ok(Self {
            handle: handle.0 as isize,
            name: name.to_string(),
        })
    }

    fn close(&self) {
        use windows::Win32::System::StationsAndDesktops::{CloseDesktop, HDESK};
        unsafe {
            let _ = CloseDesktop(HDESK(self.handle as *mut std::ffi::c_void));
        }
    }
}

/// One unit of work for the desktop thread.
type Job = Box<dyn FnOnce() + Send + 'static>;

/// The thread that owns a hidden desktop.
struct Worker {
    jobs: Option<Sender<Job>>,
    thread: Option<JoinHandle<()>>,
}

impl Worker {
    #[cfg(windows)]
    fn start(desktop: isize) -> Result<Self, AppError> {
        use windows::Win32::System::StationsAndDesktops::{SetThreadDesktop, HDESK};
        use windows::Win32::UI::WindowsAndMessaging::{PeekMessageW, MSG, PM_NOREMOVE};

        let (ready, bound) = mpsc::channel::<Result<(), String>>();
        let (jobs, queue) = mpsc::channel::<Job>();

        let thread = std::thread::Builder::new()
            .name("channel-desktop".to_string())
            .spawn(move || {
                let result = unsafe { SetThreadDesktop(HDESK(desktop as *mut std::ffi::c_void)) }
                    .map_err(|error| error.to_string());

                if result.is_ok() {
                    // A thread gets an input queue the first time it asks for
                    // one. `SendInput` needs it to exist, and it is cheaper to
                    // make it here than to discover the gap on the first click.
                    let mut message = MSG::default();
                    unsafe {
                        let _ = PeekMessageW(&mut message, None, 0, 0, PM_NOREMOVE);
                    }
                }

                let failed = result.is_err();
                let _ = ready.send(result);
                if failed {
                    return;
                }

                loop {
                    // Injected input lands in this desktop's queue and is only
                    // delivered when the thread that owns the window pumps. A
                    // thread that never pumps is a thread whose windows never
                    // see a click, so the pump is part of the loop rather than
                    // something a caller has to remember.
                    pump();

                    match queue.recv_timeout(PUMP_IDLE) {
                        Ok(job) => job(),
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
            })
            .map_err(|error| {
                AppError::message(format!("could not start the desktop thread: {error}"))
            })?;

        match bound.recv_timeout(BIND_TIMEOUT) {
            Ok(Ok(())) => Ok(Self {
                jobs: Some(jobs),
                thread: Some(thread),
            }),
            Ok(Err(error)) => Err(AppError::message(format!(
                "could not bind the hidden desktop: {error}"
            ))),
            Err(_) => Err(AppError::message(
                "the desktop thread did not bind in time",
            )),
        }
    }

    fn stop(&mut self) {
        // Dropping the sender ends the `recv` loop, which is the only way the
        // thread is asked to finish.
        self.jobs.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Where a channel runs, and the thread that makes it true.
pub struct Host {
    kind: HostKind,
    #[cfg(windows)]
    desktop: Option<Desktop>,
    worker: Option<Worker>,
    /// Set when a job outlived `JOB_TIMEOUT`. The thread is still inside it, so
    /// every later job would queue behind a call that never returns; failing
    /// fast says what happened instead of hiding it behind a growing backlog.
    stuck: AtomicBool,
}

impl Host {
    /// The user's own desktop. Jobs run on the calling thread.
    pub fn desktop() -> Self {
        Self {
            kind: HostKind::Desktop,
            #[cfg(windows)]
            desktop: None,
            worker: None,
            stuck: AtomicBool::new(false),
        }
    }

    /// A desktop of our own, invisible and out of the user's way.
    #[cfg(windows)]
    pub fn hidden() -> Result<Self, AppError> {
        static NEXT: AtomicU64 = AtomicU64::new(1);

        let name = format!(
            "vibedesk-channel-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let desktop = Desktop::create(&name)?;
        let worker = Worker::start(desktop.handle)?;

        Ok(Self {
            kind: HostKind::HiddenDesktop,
            desktop: Some(desktop),
            worker: Some(worker),
            stuck: AtomicBool::new(false),
        })
    }

    #[cfg(not(windows))]
    pub fn hidden() -> Result<Self, AppError> {
        Err(AppError::message(
            "a hidden desktop needs Windows — it is a Win32 desktop object",
        ))
    }

    pub fn kind(&self) -> HostKind {
        self.kind
    }

    /// The desktop name to hand a child process, when there is one.
    ///
    /// `CreateProcess` with a null `lpDesktop` gives the child the desktop of its
    /// parent *process*, not of the calling thread, so a child started from the
    /// desktop thread would land back on the user's screen. The name is the only
    /// way to say where it belongs.
    pub fn desktop_name(&self) -> Option<&str> {
        #[cfg(windows)]
        {
            self.desktop.as_ref().map(|desktop| desktop.name.as_str())
        }
        #[cfg(not(windows))]
        {
            None
        }
    }

    /// Run one job where this host lives, and wait for its answer.
    ///
    /// On the user's desktop this is a plain call. On a hidden desktop it is a
    /// hop to the bound thread, because that is the only thread whose input and
    /// enumeration reach the desktop the channel is driving.
    pub fn run<T, F>(&self, job: F) -> Result<T, AppError>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        let Some(worker) = self.worker.as_ref() else {
            return Ok(job());
        };

        if self.stuck.load(Ordering::Relaxed) {
            return Err(AppError::message(
                "this channel's desktop thread is stuck in an earlier call — close the channel and open a new one",
            ));
        }

        let (answer, reply) = mpsc::channel::<T>();
        let send = worker
            .jobs
            .as_ref()
            .ok_or_else(|| AppError::message("this channel's desktop thread has stopped"))?;

        send.send(Box::new(move || {
            let _ = answer.send(job());
        }))
        .map_err(|_| AppError::message("this channel's desktop thread has stopped"))?;

        match reply.recv_timeout(JOB_TIMEOUT) {
            Ok(value) => Ok(value),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.stuck.store(true, Ordering::Relaxed);
                Err(AppError::message(format!(
                    "the desktop thread did not answer within {}s — the call it is inside will not return",
                    JOB_TIMEOUT.as_secs()
                )))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(AppError::message(
                "the desktop thread stopped before it answered",
            )),
        }
    }

    /// Start a program on this host's desktop.
    ///
    /// `std::process::Command` cannot name a desktop, so this goes through
    /// `CreateProcessW` directly. One path for both hosts: the only difference is
    /// whether `lpDesktop` is set.
    pub fn spawn(&self, command: &str, args: &[String]) -> Result<(), AppError> {
        #[cfg(windows)]
        {
            use windows::core::{PCWSTR, PWSTR};
            use windows::Win32::Foundation::CloseHandle;
            use windows::Win32::System::Threading::{
                CreateProcessW, CREATE_NEW_CONSOLE, PROCESS_INFORMATION, STARTUPINFOW,
            };

            let mut line: Vec<u16> = command_line(command, args)
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            let mut desktop: Vec<u16> = self
                .desktop_name()
                .map(|name| name.encode_utf16().chain(std::iter::once(0)).collect())
                .unwrap_or_default();

            let mut startup = STARTUPINFOW {
                cb: std::mem::size_of::<STARTUPINFOW>() as u32,
                ..Default::default()
            };
            if !desktop.is_empty() {
                startup.lpDesktop = PWSTR(desktop.as_mut_ptr());
            }

            let mut info = PROCESS_INFORMATION::default();
            unsafe {
                CreateProcessW(
                    PCWSTR::null(),
                    Some(PWSTR(line.as_mut_ptr())),
                    None,
                    None,
                    false,
                    CREATE_NEW_CONSOLE,
                    None,
                    PCWSTR::null(),
                    &startup,
                    &mut info,
                )
                .map_err(|error| {
                    AppError::message(format!("could not start {command}: {error}"))
                })?;

                // The channel does not wait on the child, so both handles are
                // closed here rather than leaked for the life of the process.
                let _ = CloseHandle(info.hThread);
                let _ = CloseHandle(info.hProcess);
            }
            Ok(())
        }
        #[cfg(not(windows))]
        {
            let _ = (command, args);
            Err(AppError::message(
                "starting a program on a channel host needs Windows",
            ))
        }
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        // The thread has to leave the desktop before the desktop is closed, so
        // the worker goes first. Field order would do this too, but the ordering
        // is the point and a reader should not have to know the rule.
        if let Some(mut worker) = self.worker.take() {
            worker.stop();
        }
        #[cfg(windows)]
        if let Some(desktop) = self.desktop.take() {
            desktop.close();
        }
    }
}

/// Build a Windows command line from a program and its arguments.
///
/// The quoting rules are the ones `CommandLineToArgvW` parses: a run of
/// backslashes is literal unless it precedes a quote, where each pair becomes one
/// backslash and the odd one escapes the quote.
fn command_line(command: &str, args: &[String]) -> String {
    let mut line = String::from(command);

    for arg in args {
        line.push(' ');
        if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
            line.push_str(arg);
            continue;
        }

        line.push('"');
        let mut backslashes = 0usize;
        for character in arg.chars() {
            match character {
                '\\' => backslashes += 1,
                '"' => {
                    line.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                    line.push('"');
                    backslashes = 0;
                }
                other => {
                    line.extend(std::iter::repeat_n('\\', backslashes));
                    line.push(other);
                    backslashes = 0;
                }
            }
        }
        line.extend(std::iter::repeat_n('\\', backslashes * 2));
        line.push('"');
    }

    line
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn a_plain_argument_is_left_alone() {
        assert_eq!(command_line("app.exe", &args(&["--fast"])), "app.exe --fast");
    }

    #[test]
    fn an_argument_with_a_space_is_quoted() {
        assert_eq!(
            command_line("app.exe", &args(&["two words"])),
            "app.exe \"two words\""
        );
    }

    #[test]
    fn an_empty_argument_stays_visible_as_a_quoted_pair() {
        assert_eq!(command_line("app.exe", &args(&[""])), "app.exe \"\"");
    }

    #[test]
    fn a_quote_inside_an_argument_is_escaped() {
        assert_eq!(
            command_line("app.exe", &args(&["say \"hi\""])),
            "app.exe \"say \\\"hi\\\"\""
        );
    }

    #[test]
    fn a_trailing_backslash_is_doubled_so_it_does_not_escape_the_closing_quote() {
        assert_eq!(
            command_line("app.exe", &args(&["C:\\a b\\"])),
            "app.exe \"C:\\a b\\\\\""
        );
    }

    #[test]
    fn a_backslash_before_a_quote_is_doubled_and_the_quote_escaped() {
        assert_eq!(
            command_line("app.exe", &args(&["a\\\"b"])),
            "app.exe \"a\\\\\\\"b\""
        );
    }

    #[test]
    fn the_desktop_host_runs_a_job_on_the_calling_thread() {
        let host = Host::desktop();
        let thread = std::thread::current().id();
        let ran_on = host.run(|| std::thread::current().id()).unwrap();
        assert_eq!(ran_on, thread);
        assert_eq!(host.kind(), HostKind::Desktop);
        assert!(host.desktop_name().is_none());
    }

    #[test]
    fn the_desktop_host_hands_back_what_the_job_returned() {
        let host = Host::desktop();
        assert_eq!(host.run(|| 6 * 7).unwrap(), 42);
    }

    #[cfg(windows)]
    #[test]
    fn a_hidden_desktop_runs_a_job_on_a_different_thread() {
        let host = Host::hidden().unwrap();
        let caller = std::thread::current().id();
        let ran_on = host.run(|| std::thread::current().id()).unwrap();

        assert_ne!(ran_on, caller, "the job has to run on the bound thread");
        assert_eq!(host.kind(), HostKind::HiddenDesktop);
        assert!(host.desktop_name().is_some());
    }

    #[cfg(windows)]
    #[test]
    fn a_hidden_desktop_keeps_its_answer_and_its_order() {
        let host = Host::hidden().unwrap();
        let first = host.run(|| 1).unwrap();
        let second = host.run(|| 2).unwrap();
        assert_eq!((first, second), (1, 2));
    }

    #[cfg(windows)]
    #[test]
    fn two_hidden_desktops_are_separate() {
        let one = Host::hidden().unwrap();
        let two = Host::hidden().unwrap();
        assert_ne!(one.desktop_name(), two.desktop_name());
    }

    #[cfg(windows)]
    #[test]
    fn a_hidden_desktop_can_enumerate_its_own_windows() {
        use windows::Win32::Foundation::{HWND, LPARAM};
        use windows::Win32::UI::WindowsAndMessaging::EnumWindows;

        // The user's desktop has windows on it; a fresh hidden desktop has none.
        // That difference is the proof the thread is somewhere else, and it is
        // the property the whole background story rests on.
        let count = |host: &Host| {
            host.run(|| {
                let mut seen = 0usize;
                unsafe extern "system" fn count(window: HWND, parameter: LPARAM) -> windows::core::BOOL {
                    let _ = window;
                    let total = unsafe { &mut *(parameter.0 as *mut usize) };
                    *total += 1;
                    windows::core::BOOL(1)
                }
                unsafe {
                    let _ = EnumWindows(Some(count), LPARAM(&mut seen as *mut usize as isize));
                }
                seen
            })
            .unwrap()
        };

        let hidden = Host::hidden().unwrap();
        assert_eq!(count(&hidden), 0, "a new desktop starts empty");
    }
}
