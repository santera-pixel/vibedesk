//! The `widget` block's execution boundary.
//!
//! A `sandbox` block is markup and CSS only, and that is a platform fact rather
//! than a choice: a `srcdoc`, `blob:` or `data:` document inherits the *parent's*
//! CSP in Chromium, and the app's policy is `script-src 'self'`. A frame cannot
//! be given a looser policy than the app without loosening the app.
//!
//! A document served from a **different origin** is the exception. It carries its
//! own `Content-Security-Policy` response header, so it does not inherit the
//! app's. Tauri serves a custom scheme on Windows as `http://<scheme>.localhost`,
//! which is a distinct origin from the app, so this module can hand a widget a
//! policy that allows its own inline scripts while still forbidding the network,
//! storage and every route back into the app. The app's own `script-src` is not
//! touched; only `frame-src` grows by one origin.
//!
//! # What a widget gets
//!
//! * Its own scripts, its own DOM, its own canvas. A graph, a diagram, a control
//!   surface — anything the browser can draw.
//! * The panel's live data model, pushed on every tick.
//! * The live theme tokens, pushed on every theme change.
//! * A **capability-scoped** way to act: `prompt` sends text to the agent,
//!   `action:<id>` fires one action the panel declared in `actions.json`. A
//!   widget can do exactly what its block declared and nothing else, and the
//!   refusal is reported back to it rather than silently dropped.
//!
//! # What it does not get
//!
//! No network (`connect-src 'none'`, `img-src data:`), no storage or cookies
//! (the frame is sandboxed without `allow-same-origin`, so its origin is opaque),
//! no access to the app's DOM, no navigation of the window it sits in.
//!
//! # Addressing
//!
//! Documents are content-addressed: `http://widget.localhost/w/<hash>`, where
//! `<hash>` is FNV-1a over the block's html. Editing the html changes the URL, so
//! the frame reloads exactly when the content changes and a stale URL 404s
//! instead of serving the previous widget. `src/lib/customize.ts` computes the
//! same hash; `hash_matches_the_frontend` below pins the algorithm.

use std::collections::HashMap;

use tauri::http::{header, Response, StatusCode};

use crate::panels::{PanelBlock, PanelsConfig};

/// The scheme the widget documents are served from.
pub const SCHEME: &str = "widget";

/// Where a widget document lives, as the webview sees it.
///
/// Tauri serves a custom scheme as `http://<scheme>.localhost` on Windows and
/// Android, and as `<scheme>://localhost` on macOS and Linux. Both are in
/// `frame-src` in `tauri.conf.json`, and the frontend derives the same value
/// from the app's own origin, so the two cannot disagree.
pub const ORIGIN: &str = if cfg!(any(target_os = "windows", target_os = "android")) {
    "http://widget.localhost"
} else {
    "widget://localhost"
};

/// The path prefix a document is served under.
pub const PATH_PREFIX: &str = "/w/";

/// The policy served with every widget document.
///
/// `script-src 'unsafe-inline'` is the point of the whole module: it is what a
/// `sandbox` block cannot have. It is safe here only because everything else is
/// `'none'` — no network, no external script, no frame, no form, no base — and
/// because the document lives on its own origin behind a sandboxed frame.
pub const CSP: &str = "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data:; font-src data:; connect-src 'none'; form-action 'none'; base-uri 'none'";

/// FNV-1a over the UTF-8 bytes, as 16 lowercase hex digits.
///
/// Not a security hash and not used as one: it is a content address, so that a
/// URL changes when the html does. `src/lib/customize.ts` implements the same
/// function with `BigInt`; the test below pins a vector both sides must agree on.
pub fn hash(html: &str) -> String {
    let mut value: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in html.as_bytes() {
        value ^= u64::from(*byte);
        value = value.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{value:016x}")
}

/// The URL a block's document is served from.
///
/// Only the frontend builds one — it has the html and computes the same hash —
/// so this exists for the tests that have to hand `serve` a real address.
#[cfg(test)]
pub fn url_for(html: &str) -> String {
    format!("{ORIGIN}{PATH_PREFIX}{}", hash(html))
}

/// Every widget document the current config can serve, keyed by hash.
///
/// Rebuilt whenever the panels config is loaded, so the protocol handler never
/// touches the disk and never has to guess which workspace a plugin panel came
/// from. A widget whose panel failed validation is absent, which is the correct
/// answer: the panel is not on screen either.
#[derive(Debug, Default, Clone)]
pub struct Registry {
    documents: HashMap<String, String>,
}

impl Registry {
    pub fn from_config(config: &PanelsConfig) -> Self {
        let mut documents = HashMap::new();
        for panel in &config.panels {
            for block in &panel.blocks {
                if let PanelBlock::Widget { html, .. } = &block.body {
                    // `html` is always populated after a load: a block that
                    // points at `widgets/<name>.html` has had the file read
                    // into it by `panels::resolve_widget_srcs`.
                    if let Some(html) = html {
                        documents.insert(hash(html), html.clone());
                    }
                }
            }
        }
        Registry { documents }
    }

    pub fn get(&self, hash: &str) -> Option<&str> {
        self.documents.get(hash).map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.documents.len()
    }

    pub fn is_empty(&self) -> bool {
        self.documents.is_empty()
    }
}

/// The document served for one widget.
///
/// The block's html is body content. A full document also works — the browser
/// ignores a nested `<!doctype>`, `<html>`, `<head>` and `<body>` in body
/// context and still applies the `<style>` and runs the `<script>` inside them —
/// but the head this module writes is the one that carries the policy and the
/// bridge, so a widget should not try to own it.
pub fn document(html: &str) -> String {
    let mut out = String::with_capacity(html.len() + BOOTSTRAP.len() + 2048);
    out.push_str("<!doctype html><html><head><meta charset=\"utf-8\">");
    out.push_str("<meta http-equiv=\"Content-Security-Policy\" content=\"");
    out.push_str(CSP);
    out.push_str("\"><meta name=\"referrer\" content=\"no-referrer\">");
    out.push_str("<style>");
    out.push_str(BASE_CSS);
    out.push_str("</style><script>");
    out.push_str(BOOTSTRAP);
    out.push_str("</script></head><body>");
    out.push_str(html);
    out.push_str("</body></html>");
    out
}

/// The response for one request to the widget scheme.
///
/// A miss is a 404 with a readable body rather than an empty frame: a stale URL
/// means the panel changed under a mounted frame, and a blank box would leave
/// the user with nothing to report.
pub fn serve(registry: &Registry, uri: &str) -> Response<Vec<u8>> {
    let Some(hash) = path_of(uri).strip_prefix(PATH_PREFIX) else {
        return not_found("This address is not a widget.");
    };
    if hash.is_empty() || hash.contains('/') {
        return not_found("This address is not a widget.");
    }
    let Some(html) = registry.get(hash) else {
        return not_found(
            "This widget is no longer in the panel config. Reopen the panel to load the current one.",
        );
    };

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CONTENT_SECURITY_POLICY, CSP)
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(header::REFERRER_POLICY, "no-referrer")
        .body(document(html).into_bytes())
        .unwrap_or_else(|_| not_found("This widget could not be built."))
}

/// The path part of a request URI, with the scheme and authority removed.
///
/// The handler is given the whole URI, and the authority differs by platform:
/// `http://widget.localhost/w/…` on Windows and Android, `widget://localhost/w/…`
/// elsewhere. Both reduce to the same path.
fn path_of(uri: &str) -> &str {
    let rest = uri.split(['?', '#']).next().unwrap_or(uri);
    let Some(scheme) = rest.find("//") else {
        return rest;
    };
    let after = &rest[scheme + 2..];
    match after.find('/') {
        Some(slash) => &after[slash..],
        None => "/",
    }
}

fn not_found(message: &str) -> Response<Vec<u8>> {
    let body = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><style>{BASE_CSS}</style></head>\
         <body><p style=\"padding:8px;color:var(--vd-warn)\">{}</p></body></html>",
        escape(message)
    );
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CONTENT_SECURITY_POLICY, CSP)
        .header(header::CACHE_CONTROL, "no-store")
        .body(body.into_bytes())
        .unwrap_or_default()
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Base styling, so a widget that writes nothing but markup still looks like it
/// belongs in the panel. Every value is a custom property, so the theme push
/// repaints it without the widget knowing which theme is active.
const BASE_CSS: &str = concat!(
    ":root{color-scheme:dark;",
    "--vd-canvas:#0c0a09;--vd-canvas-raised:#131010;--vd-surface:#191514;",
    "--vd-surface-hover:#201b19;--vd-surface-sunken:#100d0c;",
    "--vd-line:#2b2422;--vd-line-soft:#201b19;--vd-line-strong:#3d332f;",
    "--vd-text:#f2ede8;--vd-text-secondary:#a89e96;--vd-text-tertiary:#6f655e;",
    "--vd-accent:#ff7a1a;--vd-accent-strong:#ff9445;--vd-accent-wash:rgba(255,122,26,.1);",
    "--vd-warn:#e0b062;--vd-danger:#e07a6e;--vd-info:#8fb0d4;--vd-ok:#5aa06a}",
    "html,body{margin:0;padding:0;background:transparent;color:var(--vd-text);",
    "font:12px/1.55 ui-sans-serif,system-ui,-apple-system,\"Segoe UI\",sans-serif;",
    "-webkit-font-smoothing:antialiased}",
    "*,*::before,*::after{box-sizing:border-box}",
    "a{color:var(--vd-accent)}",
    "table{border-collapse:collapse;width:100%}",
    "th,td{text-align:left;padding:3px 6px;border-bottom:1px solid var(--vd-line)}",
    "th{color:var(--vd-text-secondary);font-weight:600}",
    "button{font:inherit;color:inherit;background:var(--vd-surface);",
    "border:1px solid var(--vd-line-strong);border-radius:4px;padding:3px 8px;cursor:pointer}",
    "button:hover{background:var(--vd-surface-hover)}",
    "button:active{background:var(--vd-surface-sunken)}",
    "code,pre{font-family:ui-monospace,SFMono-Regular,Menlo,monospace;font-size:11px}",
    "pre{margin:0;overflow:auto}",
    "canvas,svg{display:block;max-width:100%}",
);

/// The bridge a widget talks to.
///
/// Runs in the document's head, before the block's own markup, so `vibedesk`
/// exists by the time the widget's first script line runs. Everything it exposes
/// is either a subscription or a capability call; there is no route to the app
/// that does not go through the parent's validation.
const BOOTSTRAP: &str = r#"(function () {
  'use strict';
  var api = {};
  var declared = [];
  var data = {};
  var theme = {};
  var initialised = false;
  var listeners = { data: [], theme: [] };
  var pending = new Map();
  var nextId = 1;
  var readyResolve;
  var ready = new Promise(function (resolve) { readyResolve = resolve; });

  function post(message) {
    try { parent.postMessage(message, '*'); } catch (error) { console.error(error); }
  }

  function emit(kind, value) {
    var list = listeners[kind];
    for (var index = 0; index < list.length; index += 1) {
      try { list[index](value); } catch (error) { console.error(error); }
    }
  }

  function applyTheme(tokens) {
    var root = document.documentElement;
    for (var name in tokens) {
      if (Object.prototype.hasOwnProperty.call(tokens, name)) {
        root.style.setProperty(name, tokens[name]);
      }
    }
  }

  // A subscriber registered after init is called once with the value already in
  // hand. Without that, the natural pattern -- markup first, then
  // `vibedesk.onData(draw)` -- would miss the first tick and draw nothing until
  // the next one, which for a slow source is a blank widget for ten seconds.
  function subscribe(kind) {
    return function (fn) {
      if (typeof fn !== 'function') return function () {};
      listeners[kind].push(fn);
      if (initialised) {
        try { fn(kind === 'data' ? data : theme); } catch (error) { console.error(error); }
      }
      return function () {
        var at = listeners[kind].indexOf(fn);
        if (at >= 0) listeners[kind].splice(at, 1);
      };
    };
  }

  // Resolves after init, so a widget may call this from its first line without
  // racing the handshake. Rejects when the panel did not declare the capability,
  // which is a bug in the widget rather than a refusal by the user.
  function invoke(capability, payload) {
    return ready.then(function () {
      if (declared.indexOf(capability) < 0) {
        throw new Error(
          'this widget did not declare the `' + capability + '` capability, so the app will refuse it'
        );
      }
      var id = nextId;
      nextId += 1;
      return new Promise(function (resolve, reject) {
        pending.set(id, { resolve: resolve, reject: reject });
        post({ type: 'vibedesk:invoke', id: id, capability: capability, payload: payload || {} });
      });
    });
  }

  window.addEventListener('message', function (event) {
    var message = event.data;
    if (!message || typeof message !== 'object') return;

    if (message.type === 'vibedesk:init') {
      declared = Array.isArray(message.capabilities) ? message.capabilities.slice() : [];
      data = message.data || {};
      theme = message.theme || {};
      applyTheme(theme);
      initialised = true;
      emit('theme', theme);
      emit('data', data);
      readyResolve(api);
      return;
    }

    if (message.type === 'vibedesk:data') {
      data = message.data || {};
      emit('data', data);
      return;
    }

    if (message.type === 'vibedesk:theme') {
      theme = message.theme || {};
      applyTheme(theme);
      emit('theme', theme);
      return;
    }

    if (message.type === 'vibedesk:ack') {
      var entry = pending.get(message.id);
      if (!entry) return;
      pending.delete(message.id);
      if (message.ok) entry.resolve(message.result === undefined ? null : message.result);
      else entry.reject(new Error(message.error || 'the app refused the request'));
    }
  });

  api.ready = ready;
  api.onData = subscribe('data');
  api.onTheme = subscribe('theme');
  api.has = function (capability) { return declared.indexOf(capability) >= 0; };
  api.prompt = function (text) {
    return invoke('prompt', { text: String(text == null ? '' : text) });
  };
  api.action = function (id) { return invoke('action:' + String(id), {}); };
  // Resolves with the file's text. The path is relative to the workspace root;
  // the app refuses anything that escapes it, and refuses a read storm.
  api.read = function (path) {
    return invoke('file', { path: String(path == null ? '' : path) });
  };
  api.resize = function (height) {
    var value = Math.round(Number(height));
    if (!isFinite(value)) return;
    post({ type: 'vibedesk:resize', height: value });
  };
  Object.defineProperty(api, 'data', { get: function () { return data; } });
  Object.defineProperty(api, 'theme', { get: function () { return theme; } });
  Object.defineProperty(api, 'capabilities', { get: function () { return declared.slice(); } });

  window.vibedesk = api;
})();"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panels::{Block, CustomPanel};

    fn config_with(html: &str) -> PanelsConfig {
        PanelsConfig {
            panels: vec![CustomPanel {
                id: "p".into(),
                title: "P".into(),
                icon: None,
                width: 320,
                layout: None,
                blocks: vec![Block::new(PanelBlock::Widget {
                    html: Some(html.into()),
                    src: None,
                    title: None,
                    height: 200,
                    capabilities: Vec::new(),
                    sources: Vec::new(),
                })],
                sources: Vec::new(),
                plugin: None,
            }],
        }
    }

    /// The frontend computes the same hash with `BigInt`. If the two ever
    /// disagree the iframe 404s, so the vector is pinned on both sides.
    #[test]
    fn hash_matches_the_frontend() {
        // Vectors computed by the `BigInt` implementation in
        // `src/lib/customize.ts`. A drift here is a 404 in the frame, so both
        // sides pin the same numbers.
        assert_eq!(hash(""), "cbf29ce484222325");
        assert_eq!(hash("a"), "af63dc4c8601ec8c");
        assert_eq!(hash("<p>hi</p>"), "2af7677882c30339");
        assert_eq!(hash("<canvas></canvas>"), "545f3ee9ddac8784");
        // Multi-byte input hashes its UTF-8 bytes, not its code points.
        assert_eq!(hash("héllo"), "a35ff71f960240e0");
    }

    /// The origin has to be the one this platform's webview will actually
    /// resolve. A hardcoded Windows origin is a blank frame on macOS and Linux,
    /// and a blank frame is indistinguishable from a broken widget.
    #[test]
    fn the_origin_matches_the_platform_scheme() {
        if cfg!(any(target_os = "windows", target_os = "android")) {
            assert_eq!(ORIGIN, "http://widget.localhost");
        } else {
            assert_eq!(ORIGIN, "widget://localhost");
        }
        // Both forms are in `frame-src`, so whichever this is, the app policy
        // allows it.
        let config = include_str!("../tauri.conf.json");
        assert!(config.contains("http://widget.localhost"));
        assert!(config.contains("widget://localhost"));
    }

    #[test]
    fn the_hash_is_sixteen_hex_digits() {
        for html in ["", "x", "<svg></svg>", "héllo"] {
            let value = hash(html);
            assert_eq!(value.len(), 16, "{value}");
            assert!(value.chars().all(|c| c.is_ascii_hexdigit()), "{value}");
        }
    }

    #[test]
    fn a_different_html_is_a_different_url() {
        assert_ne!(url_for("<p>a</p>"), url_for("<p>b</p>"));
        assert_eq!(url_for("<p>a</p>"), url_for("<p>a</p>"));
        assert!(url_for("<p>a</p>").starts_with(&format!("{ORIGIN}/w/")));
    }

    #[test]
    fn the_document_carries_the_policy_and_the_bridge() {
        let document = document("<p>hello</p>");
        assert!(document.contains(CSP));
        assert!(document.contains("window.vibedesk = api"));
        assert!(document.contains("<p>hello</p>"));
        // The bridge has to be in the head, before the block's own markup, or a
        // widget that calls `vibedesk.onData` on its first line throws.
        let bridge = document.find("window.vibedesk = api").unwrap();
        let body = document.find("<p>hello</p>").unwrap();
        assert!(bridge < body, "the bridge must run before the widget's markup");
    }

    #[test]
    fn the_policy_allows_inline_scripts_and_nothing_else() {
        assert!(CSP.contains("script-src 'unsafe-inline'"));
        assert!(CSP.contains("connect-src 'none'"));
        assert!(CSP.contains("default-src 'none'"));
        assert!(!CSP.contains("http://"));
        assert!(!CSP.contains("https://"));
    }

    #[test]
    fn the_registry_finds_a_widget_by_its_hash() {
        let config = config_with("<canvas></canvas>");
        let registry = Registry::from_config(&config);
        assert_eq!(registry.len(), 1);
        assert_eq!(
            registry.get(&hash("<canvas></canvas>")),
            Some("<canvas></canvas>")
        );
        assert_eq!(registry.get("0000000000000000"), None);
    }

    #[test]
    fn a_served_document_is_html_with_its_own_policy() {
        let config = config_with("<p>x</p>");
        let registry = Registry::from_config(&config);
        let response = serve(&registry, &url_for("<p>x</p>"));
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/html; charset=utf-8"
        );
        assert_eq!(
            response.headers().get(header::CONTENT_SECURITY_POLICY).unwrap(),
            CSP
        );
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-store"
        );
        let body = String::from_utf8(response.body().clone()).unwrap();
        assert!(body.contains("<p>x</p>"));
    }

    #[test]
    fn a_stale_url_is_a_readable_404() {
        let registry = Registry::from_config(&config_with("<p>x</p>"));
        let response = serve(&registry, "http://widget.localhost/w/deadbeefdeadbeef");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = String::from_utf8(response.body().clone()).unwrap();
        assert!(body.contains("no longer in the panel config"));
    }

    #[test]
    fn anything_that_is_not_a_widget_path_is_refused() {
        let registry = Registry::from_config(&config_with("<p>x</p>"));
        for uri in [
            "http://widget.localhost/",
            "http://widget.localhost/w/",
            "http://widget.localhost/w/a/b",
            "http://widget.localhost/index.html",
        ] {
            assert_eq!(
                serve(&registry, uri).status(),
                StatusCode::NOT_FOUND,
                "{uri}"
            );
        }
    }

    #[test]
    fn a_query_string_does_not_change_the_lookup() {
        let registry = Registry::from_config(&config_with("<p>x</p>"));
        let url = format!("{}?v=2", url_for("<p>x</p>"));
        assert_eq!(serve(&registry, &url).status(), StatusCode::OK);
    }

    /// The authority differs by platform: Tauri serves a custom scheme as
    /// `http://<scheme>.localhost` on Windows and Android, and as
    /// `widget://localhost` on macOS and Linux. The handler is handed the whole
    /// URI, so both shapes have to reduce to the same path or the frame is a 404
    /// on half the platforms.
    #[test]
    fn both_platform_uri_shapes_reach_the_same_widget() {
        let registry = Registry::from_config(&config_with("<p>x</p>"));
        let hash = hash("<p>x</p>");
        for uri in [
            format!("http://widget.localhost/w/{hash}"),
            format!("widget://localhost/w/{hash}"),
            format!("https://widget.localhost/w/{hash}"),
            format!("widget://localhost/w/{hash}?v=2"),
            format!("widget://localhost/w/{hash}#top"),
        ] {
            assert_eq!(serve(&registry, &uri).status(), StatusCode::OK, "{uri}");
        }
    }

    #[test]
    fn the_path_is_the_same_whatever_the_authority_is() {
        assert_eq!(path_of("http://widget.localhost/w/abc"), "/w/abc");
        assert_eq!(path_of("widget://localhost/w/abc"), "/w/abc");
        assert_eq!(path_of("widget://localhost/w/abc?x=1"), "/w/abc");
        assert_eq!(path_of("widget://localhost"), "/");
        assert_eq!(path_of("/w/abc"), "/w/abc");
    }

    #[test]
    fn an_empty_registry_serves_nothing() {
        let registry = Registry::default();
        assert!(registry.is_empty());
        assert_eq!(
            serve(&registry, &url_for("<p>x</p>")).status(),
            StatusCode::NOT_FOUND
        );
    }

    /// The whole design rests on the app's own policy staying closed. If a
    /// future change "fixes" a widget by adding `'unsafe-inline'` to the app's
    /// `script-src`, the boundary is gone and nothing else would notice.
    ///
    /// The policy also lives in a `<meta>` tag in `index.html`, and that is the
    /// one the webview actually enforces — the config copy is the fallback. The
    /// two had drifted once before, so this test pins them together: a change to
    /// one without the other fails here instead of showing up as a frame that
    /// silently refuses to load.
    #[test]
    fn the_app_policy_is_not_loosened_to_make_widgets_work() {
        let raw = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tauri.conf.json"))
            .expect("tauri.conf.json");
        let config: serde_json::Value = serde_json::from_str(&raw).expect("valid json");
        let security = &config["app"]["security"];

        for key in ["csp", "devCsp"] {
            let policy = security[key].as_str().unwrap_or_else(|| panic!("{key}"));
            let frame = policy
                .split(';')
                .map(str::trim)
                .find(|part| part.starts_with("frame-src"))
                .unwrap_or_else(|| panic!("{key} has no frame-src"));
            assert!(
                frame.contains(ORIGIN),
                "{key} does not allow the widget origin in frame-src: {frame}"
            );
        }

        // `devCsp` is exempt from the script check: Vite's dev server injects
        // inline scripts for HMR, so it already carried `'unsafe-inline'` before
        // widgets existed. The shipped policy is the one that matters.
        let policy = security["csp"].as_str().expect("csp");
        let script = policy
            .split(';')
            .map(str::trim)
            .find(|part| part.starts_with("script-src"))
            .expect("csp has no script-src");
        assert!(
            !script.contains("unsafe-inline"),
            "the shipped policy allows inline scripts in the app itself: {script}"
        );
        assert!(
            !script.contains("unsafe-eval"),
            "the shipped policy allows eval in the app itself: {script}"
        );

        let html = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../index.html"))
            .expect("index.html");
        let meta = html
            .split("http-equiv=\"Content-Security-Policy\"")
            .nth(1)
            .and_then(|rest| rest.split("content=\"").nth(1))
            .and_then(|rest| rest.split('"').next())
            .expect("the meta policy in index.html");
        assert_eq!(
            meta, policy,
            "the meta policy in index.html and `app.security.csp` have drifted apart; \
             the meta is the one the webview enforces"
        );
    }
}
