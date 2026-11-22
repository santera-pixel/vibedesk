//! Build script.
//!
//! Two jobs beyond `tauri_build::build()`, both about the same trap.
//!
//! `tauri::generate_context!()` embeds `dist/` into the binary at compile time.
//! That makes the frontend a build input, but nothing told Cargo so: a
//! `cargo build` after a `vite build` could reuse a cached crate and keep the
//! previous bundle, and the app would then serve old code while the source on
//! disk said otherwise. The change looks broken and nothing reports an error.
//!
//! So:
//!
//! 1. `dist/` is declared as an input, which forces a rebuild when the bundle
//!    changes. This is the half that removes the trap.
//! 2. The frontend sources are declared as inputs too, so this script re-runs
//!    when they change and can warn when `dist/` is older than them. This is
//!    the half that catches the other order — editing the frontend and building
//!    without running `vite build` at all.
//!
//! The bundle name is exported as `VIBEDESK_BUNDLE` so the running app can say
//! which build it is, and `VIBEDESK_DIST_STALE` so it can say when that build
//! was made from a stale bundle.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

fn main() {
    tauri_build::build();

    // Test binaries need the Common Controls v6 manifest too.
    //
    // `tauri_build::build()` embeds a manifest into the app binary, and that
    // manifest is what makes Windows bind `comctl32.dll` v6. The test harness is
    // a separate binary and gets no manifest, so the loader binds v5 from
    // System32 — which does not export `TaskDialogIndirect` or
    // `SetWindowSubclass` — and `cargo test` dies at process start with
    // `0xc0000139 STATUS_ENTRYPOINT_NOT_FOUND` before a single test runs.
    //
    // `rustc-link-arg` rather than `rustc-link-arg-tests`: this package has no
    // `[[test]]` target, so the unit tests are built under the lib target and
    // Cargo rejects the `-tests` key outright. The app binary already carries a
    // manifest from `tauri_build`, and a second dependency entry merges into it
    // rather than replacing it.
    println!(
        "cargo:rustc-link-arg=/MANIFESTDEPENDENCY:type='win32' \
         name='Microsoft.Windows.Common-Controls' version='6.0.0.0' \
         processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'"
    );

    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let index = root.join("../dist/index.html");

    // The bundle is an input: changing it must rebuild the crate that embeds it.
    println!("cargo:rerun-if-changed=../dist");
    // The sources are inputs: changing them must re-run this check.
    println!("cargo:rerun-if-changed=../src");
    println!("cargo:rerun-if-changed=../index.html");
    println!("cargo:rerun-if-changed=../package.json");
    println!("cargo:rerun-if-changed=../vite.config.ts");
    // The Tauri config is an input for the same reason, and it was the one that
    // was missing. The CSP, the window config and the bundle identifier are all
    // baked into the codegen output, so a config edit that does not re-run this
    // script leaves the binary serving the previous policy while the file on
    // disk says otherwise — a change that looks broken and reports nothing.
    println!("cargo:rerun-if-changed=tauri.conf.json");
    println!("cargo:rerun-if-changed=capabilities");

    println!("cargo:rustc-env=VIBEDESK_BUNDLE={}", bundle_name(&index));

    let Some(built) = modified(&index) else {
        println!(
            "cargo:warning=dist/index.html is missing — run `npm run build` before `cargo build`, \
             or the binary will have no frontend"
        );
        println!("cargo:rustc-env=VIBEDESK_DIST_STALE=1");
        return;
    };

    match newest_source(&root) {
        Some((path, changed)) if changed > built => {
            println!(
                "cargo:warning=dist/ is older than {} — the binary will embed a stale frontend. \
                 Run `npm run build` first.",
                relative(&root, &path)
            );
            println!("cargo:rustc-env=VIBEDESK_DIST_STALE=1");
        }
        _ => println!("cargo:rustc-env=VIBEDESK_DIST_STALE=0"),
    }
}

/// A path relative to the repository root, for a message a human reads.
///
/// The paths here are built as `src-tauri/../src/…`, which is correct and
/// unreadable. Stripping the prefix is the difference between a warning that
/// names the file and one that looks like a bug in the build script.
fn relative(root: &Path, path: &Path) -> String {
    let repo = root.join("..");
    let text = path.to_string_lossy().replace('\\', "/");
    let prefix = repo.to_string_lossy().replace('\\', "/");
    text.strip_prefix(&format!("{prefix}/"))
        .unwrap_or(&text)
        .to_string()
}

/// The `assets/index-*.js` the built `index.html` loads, or `unknown`.
///
/// Read from the file rather than guessed, so the value the app reports is the
/// bundle that was actually embedded.
fn bundle_name(index: &Path) -> String {
    let Ok(html) = std::fs::read_to_string(index) else {
        return "unknown".into();
    };
    let Some(start) = html.find("/assets/") else {
        return "unknown".into();
    };
    let rest = &html[start + 1..];
    let end = rest.find('"').unwrap_or(rest.len());
    rest[..end].to_string()
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

/// The most recently modified frontend source, and when.
///
/// `node_modules` and `dist` are skipped: the first is not source, and the
/// second is what is being compared against.
fn newest_source(root: &Path) -> Option<(PathBuf, SystemTime)> {
    let mut newest: Option<(PathBuf, SystemTime)> = None;

    for entry in [root.join("../index.html"), root.join("../package.json")] {
        if let Some(time) = modified(&entry) {
            newest = Some((entry, time));
        }
    }

    let mut stack = vec![root.join("../src")];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                stack.push(path);
                continue;
            }
            let Some(time) = modified(&path) else {
                continue;
            };
            if newest.as_ref().is_none_or(|(_, best)| time > *best) {
                newest = Some((path, time));
            }
        }
    }

    newest
}
