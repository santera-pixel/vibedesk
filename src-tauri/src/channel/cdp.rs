//! The DevTools protocol, in process.
//!
//! `ICoreWebView2::CallDevToolsProtocolMethod` is Microsoft's documented Win32
//! API for driving a WebView2 from its host. It is a runtime API, not a debug
//! feature: it works in a release build, it needs no port, and it needs no
//! listener. That is why the channeler uses it rather than the usual
//! `--remote-debugging-port` route, which would mean a second process, a socket
//! the user can see, and a debug flag in the shipped binary.
//!
//! The call is asynchronous and its callback fires on the webview's message
//! pump, so every call has to run on the thread that owns the pump and the
//! answer has to come back through a channel. This module hides that: `call`
//! looks synchronous to everything above it.
//!
//! The pump is bounded. `webview2_com::wait_with_pump` loops on `GetMessageA`
//! with no deadline, so a callback that never fires hangs the caller forever.
//! This pumps with a deadline instead, because a test tool that can hang is
//! worse than one that reports a timeout.

use std::time::Duration;

/// How long one call may take before it is abandoned.
const CALL_TIMEOUT: Duration = Duration::from_secs(10);

/// How long the pump sleeps when no message is waiting.
const IDLE: Duration = Duration::from_millis(1);

#[cfg(windows)]
pub use windows_impl::{call, subscribe};

#[cfg(windows)]
mod windows_impl {
    use std::sync::mpsc;
    use std::time::Instant;

    use serde_json::Value;
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        ICoreWebView2, ICoreWebView2DevToolsProtocolEventReceivedEventArgs,
    };
    use webview2_com::{
        take_pwstr, CallDevToolsProtocolMethodCompletedHandler,
        DevToolsProtocolEventReceivedEventHandler,
    };
    use windows::core::{HSTRING, PWSTR};
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageA, PeekMessageA, TranslateMessage, MSG, PM_REMOVE,
    };

    use super::{CALL_TIMEOUT, IDLE};
    use crate::error::AppError;

    /// One protocol call, answered synchronously.
    ///
    /// Must run on the thread that owns the webview's message pump — in this app
    /// that is the main thread, reached through `Webview::with_webview`.
    pub fn call(webview: &ICoreWebView2, method: &str, params: Value) -> Result<Value, AppError> {
        let (sender, receiver) = mpsc::channel();
        let handler = CallDevToolsProtocolMethodCompletedHandler::create(Box::new(
            move |result, json| {
                let _ = sender.send((result, json));
                Ok(())
            },
        ));

        let method_wide = HSTRING::from(method);
        let params_wide = HSTRING::from(params.to_string());
        unsafe {
            webview
                .CallDevToolsProtocolMethod(&method_wide, &params_wide, &handler)
                .map_err(|error| AppError::message(format!("{method} was refused: {error}")))?;
        }

        let (result, json) = pump(&receiver, method)?;
        result.map_err(|error| AppError::message(format!("{method} failed: {error}")))?;

        serde_json::from_str(&json).map_err(|error| {
            AppError::message(format!("{method} answered with invalid json: {error}"))
        })
    }

    /// Listen for one protocol event, handing each payload to `sink`.
    ///
    /// Returns the token that would remove the listener. The channeler keeps it
    /// so closing a channel stops the events rather than leaving a handler
    /// pushing into a buffer nobody reads.
    pub fn subscribe(
        webview: &ICoreWebView2,
        event: &str,
        mut sink: impl FnMut(Value) + 'static,
    ) -> Result<i64, AppError> {
        let receiver = unsafe {
            webview
                .GetDevToolsProtocolEventReceiver(&HSTRING::from(event))
                .map_err(|error| {
                    AppError::message(format!("{event} is not available: {error}"))
                })?
        };

        let handler = DevToolsProtocolEventReceivedEventHandler::create(Box::new(
            move |_sender: Option<ICoreWebView2>,
                  args: Option<ICoreWebView2DevToolsProtocolEventReceivedEventArgs>| {
                if let Some(args) = args {
                    if let Ok(payload) = payload_of(&args) {
                        if let Ok(value) = serde_json::from_str::<Value>(&payload) {
                            sink(value);
                        }
                    }
                }
                Ok(())
            },
        ));

        let mut token = 0i64;
        unsafe {
            receiver
                .add_DevToolsProtocolEventReceived(&handler, &mut token)
                .map_err(|error| {
                    AppError::message(format!("could not listen for {event}: {error}"))
                })?;
        }
        Ok(token)
    }

    fn payload_of(
        args: &ICoreWebView2DevToolsProtocolEventReceivedEventArgs,
    ) -> Result<String, AppError> {
        let mut raw = PWSTR::null();
        unsafe {
            args.ParameterObjectAsJson(&mut raw).map_err(|error| {
                AppError::message(format!("could not read the event payload: {error}"))
            })?;
        }
        Ok(take_pwstr(raw))
    }

    /// Pump messages until the answer arrives or the deadline passes.
    fn pump<T>(receiver: &mpsc::Receiver<T>, method: &str) -> Result<T, AppError> {
        let deadline = Instant::now() + CALL_TIMEOUT;
        let mut message = MSG::default();

        loop {
            if let Ok(value) = receiver.try_recv() {
                return Ok(value);
            }
            if Instant::now() >= deadline {
                return Err(AppError::message(format!(
                    "{method} did not answer within {}s",
                    CALL_TIMEOUT.as_secs()
                )));
            }
            unsafe {
                if PeekMessageA(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&message);
                    DispatchMessageA(&message);
                } else {
                    std::thread::sleep(IDLE);
                }
            }
        }
    }
}

