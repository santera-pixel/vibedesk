//! A live probe for the `webview` backend.
//!
//! The `webview` backend is the one the unit tests cannot reach: it needs a real
//! WebView2, which needs a real Tauri app, which needs an event loop on the main
//! thread. So it is proven here instead — a real app, a real webview, a real
//! page, driven over the same JSON-RPC the MCP server speaks.
//!
//! ```text
//! cargo run --bin channel-probe
//! ```
//!
//! It prints one line per claim and exits non-zero if any of them is false, so
//! it can be run by hand or from a script. It opens a window for a few seconds
//! and closes it.

use std::time::Duration;

use serde_json::{json, Value};

/// The page the probe drives. A button with a known label, a canvas with a known
/// colour, and one deliberate console error so the issue channel has something
/// real to report.
const PAGE: &str = r#"<!doctype html>
<html><head><meta charset="utf-8"><title>channel probe</title>
<style>body{margin:0;background:#123456;font:16px sans-serif;color:#fff}</style>
</head><body>
<button id="go" onclick="this.textContent='Clicked';document.getElementById('out').textContent='clicked'">Press me</button>
<div id="out">idle</div>
<canvas id="c" width="200" height="100"></canvas>
<script>
  const ctx = document.getElementById('c').getContext('2d');
  ctx.fillStyle = '#ff8800';
  ctx.fillRect(0, 0, 200, 100);
  console.error('probe: a deliberate console error');
</script>
</body></html>"#;

fn main() {
    let page = std::env::temp_dir().join("vibedesk-channel-probe.html");
    if let Err(error) = std::fs::write(&page, PAGE) {
        eprintln!("could not write the probe page: {error}");
        std::process::exit(2);
    }
    let url = format!("file:///{}", page.display().to_string().replace('\\', "/"));

    tauri::Builder::default()
        .setup(move |app| {
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                let code = run(&handle, &url);
                std::process::exit(code);
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("the probe app has to run");
}

/// Drive the channeler and print a verdict per claim.
fn run(app: &tauri::AppHandle, url: &str) -> i32 {
    let mut failures = 0;
    let mut check = |name: &str, ok: bool, detail: String| {
        if ok {
            println!("  ok    {name}");
        } else {
            println!("  FAIL  {name} — {detail}");
            failures += 1;
        }
    };

    // The server has to be listening before `channel_open` can find the app, and
    // starting it is also what registers the app handle the backend reads.
    if let Err(error) = tauri::async_runtime::block_on(vibedesk_lib::channel::mcp::ensure_started(
        app.clone(),
    )) {
        eprintln!("the channeler did not start: {error}");
        return 2;
    }

    // The window exists before `setup` runs, but its webview is not ready to
    // take a navigation until the event loop has turned at least once.
    std::thread::sleep(Duration::from_millis(1_500));

    let opened = call(
        "channel_open",
        json!({ "backend": "webview", "label": "main" }),
    );
    let Some(body) = body_of(&opened) else {
        eprintln!("channel_open did not answer with json: {}", text_of(&opened));
        return 2;
    };
    let id = body["id"].as_str().unwrap_or_default().to_string();
    if id.is_empty() {
        eprintln!("channel_open did not name a channel: {body}");
        return 2;
    }

    check(
        "a webview channel is background and synthetic",
        body["capabilities"]["foreground"] == json!(false)
            && body["capabilities"]["input"] == json!("synthetic"),
        format!("{}", body["capabilities"]),
    );
    check(
        "a webview channel can capture",
        body["capabilities"]["capture"] == json!(true),
        format!("{}", body["capabilities"]),
    );

    // Navigate to the probe page and let it paint.
    if let Err(error) = navigate(app, url) {
        eprintln!("could not navigate the webview: {error}");
        return 2;
    }
    std::thread::sleep(Duration::from_millis(1_200));

    let looked = call("channel_look", json!({ "id": id }));
    let look = body_of(&looked).unwrap_or(Value::Null);
    let labels: Vec<String> = look["targets"]
        .as_array()
        .map(|targets| {
            targets
                .iter()
                .filter_map(|target| target["label"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    check(
        "the page's button is in the tree",
        labels.iter().any(|label| label == "Press me"),
        format!("{labels:?}"),
    );

    // The deliberate console error has to fail the assertion, which is the whole
    // point of the issue channel. It is checked before any act, because an act
    // drains the issues its own steps produced — the trap `channeler-verify`
    // documents.
    let checked = body_of(&call("channel_expect", json!({ "id": id }))).unwrap_or(Value::Null);
    check(
        "a console error fails the assertion",
        checked["ok"] == json!(false),
        format!("{checked}"),
    );

    // A label is not a target: the contract is a ref id or a point. The refusal
    // has to say so rather than sending the caller round the look loop.
    let by_label = body_of(&call(
        "channel_act",
        json!({ "id": id, "actions": [{ "kind": "click", "target": "Press me" }] }),
    ))
    .unwrap_or(Value::Null);
    check(
        "a label is refused with the form it wanted",
        by_label["ok"] == json!(false)
            && by_label["results"][0]["detail"]
                .as_str()
                .unwrap_or_default()
                .contains("not a label"),
        format!("{by_label}"),
    );

    let Some(button) = look["targets"]
        .as_array()
        .and_then(|targets| {
            targets
                .iter()
                .find(|target| target["label"] == json!("Press me"))
        })
        .and_then(|target| target["id"].as_str())
        .map(str::to_string)
    else {
        eprintln!("the button has no ref: {look}");
        return 2;
    };

    let clicked = call(
        "channel_act",
        json!({ "id": id, "actions": [{ "kind": "click", "target": button }] }),
    );
    let outcome = body_of(&clicked).unwrap_or(Value::Null);
    check(
        "a click by ref is accepted",
        outcome["ok"] == json!(true),
        format!("{outcome}"),
    );

    std::thread::sleep(Duration::from_millis(400));
    let after = body_of(&call("channel_look", json!({ "id": id }))).unwrap_or(Value::Null);
    let digest = after["digest"].as_str().unwrap_or_default().to_string();
    // The button relabels itself, so the effect of the click is visible in the
    // tree the agent reads rather than only in the DOM.
    check(
        "the click reached the page",
        digest.contains("Clicked"),
        digest.chars().take(400).collect(),
    );

    // A picture, and the type that says what it is.
    let pictured = body_of(&call(
        "channel_look",
        json!({ "id": id, "capture": true }),
    ))
    .unwrap_or(Value::Null);
    let capture = pictured["capture"].as_str().unwrap_or_default();
    check(
        "a capture comes back as a JPEG",
        pictured["capture_type"] == json!("image/jpeg") && capture.starts_with("/9j/"),
        format!(
            "type {:?}, first bytes {:?}",
            pictured["capture_type"],
            capture.chars().take(8).collect::<String>()
        ),
    );

    let closed = call("channel_close", json!({ "id": id }));
    check(
        "the channel closes",
        closed["result"]["isError"] != json!(true),
        text_of(&closed),
    );

    let _ = std::fs::remove_file(std::env::temp_dir().join("vibedesk-channel-probe.html"));

    if failures == 0 {
        println!("\nthe webview backend is proven live");
        0
    } else {
        println!("\n{failures} claim(s) did not hold");
        1
    }
}

/// Point the webview at the probe page through the backend's own CDP route.
///
/// `channel_act` has no navigate verb and should not gain one: navigation is not
/// something an agent should be able to do to a page it is testing. The probe
/// reaches the protocol directly for this one step, which is also what proves
/// the protocol route works.
fn navigate(app: &tauri::AppHandle, url: &str) -> Result<(), String> {
    use tauri::Manager;

    let url = url.to_string();

    let window = app
        .get_webview_window("main")
        .ok_or_else(|| "there is no main window".to_string())?;
    let (sender, receiver) = std::sync::mpsc::channel();

    window
        .with_webview(move |platform| {
            let outcome = (|| {
                let controller = platform.controller();
                let core = unsafe { controller.CoreWebView2() }
                    .map_err(|error| format!("the webview has no core: {error}"))?;
                vibedesk_lib::channel::cdp::call(&core, "Page.navigate", json!({ "url": url }))
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            })();
            let _ = sender.send(outcome);
        })
        .map_err(|error| format!("could not reach the webview: {error}"))?;

    receiver
        .recv_timeout(Duration::from_secs(10))
        .map_err(|error| format!("the webview did not answer: {error}"))?
}

/// One JSON-RPC call, the way the agent makes it.
fn call(name: &str, arguments: Value) -> Value {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": name, "arguments": arguments },
    });
    vibedesk_lib::channel::mcp::dispatch(&request).unwrap_or_else(|| {
        json!({ "result": { "isError": true, "content": [{ "type": "text", "text": "no answer" }] } })
    })
}

fn text_of(reply: &Value) -> String {
    reply["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn body_of(reply: &Value) -> Option<Value> {
    serde_json::from_str(&text_of(reply)).ok()
}
