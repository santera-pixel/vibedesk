pub mod acp;
mod actions;
mod billing;
pub mod capabilities;
pub mod channel;
mod chats;
mod commands;
pub mod config;
mod consent;
mod customize;
mod data;
mod error;
mod expr;
mod install;
mod layout;
mod manifest;
mod panels;
mod paths;
mod plugin_panels;
mod plugins;
mod provision;
/// The agent payload the swarm runs on, re-exported so the runner's own test can
/// lay out a `VIBE_HOME` from code instead of borrowing the user's install.
pub use provision::payload as agent_payload;
mod release_notes;
mod settings;
mod skills;
mod sources;
pub mod swarm;
mod update;
mod widget;

#[cfg(test)]
mod battletests;

use std::sync::{Arc, Mutex};

use tauri::{Emitter, Manager};

use acp::bridge::AcpBridge;
use plugins::PluginRecord;

/// Everything the IPC layer needs. The bridge owns the `vibe-acp` child
/// process; the plugin cache exists so `plugins_reload` can diff against the
/// snapshot the running session was pinned to.
pub struct AppState {
    pub bridge: Arc<AcpBridge>,
    pub plugins: Mutex<Vec<PluginRecord>>,
    /// The panel data plane. Owns one task per distinct external source, and
    /// the kill switch that stops all of them.
    pub data: data::DataPlane,
    /// Every widget document the current panels config can serve, keyed by the
    /// content hash in its URL.
    ///
    /// Held here rather than read from disk per request so the protocol handler
    /// stays synchronous and never has to guess which workspace a plugin panel
    /// came from: the config watcher already resolved that, and it is the same
    /// config the webview was handed.
    pub widgets: Mutex<widget::Registry>,
}

/// Keep WebView2's cache inside the install folder too.
///
/// Without this, a portable install on another drive still drops a few hundred
/// MB of browser cache into the user profile on `C:`. WebView2 reads this
/// variable when it creates its environment, which happens during
/// `Builder::build`, so it has to be set before the builder runs.
/// Watch the four customization files, pushing every valid change to the
/// webview.
///
/// Polling rather than a filesystem watcher: editors save by writing a temp
/// file and renaming it, which fires several events for one logical change, and
/// a config file that changes a few times a session does not justify the
/// platform quirks of `ReadDirectoryChangesW`. Comparing (mtime, length) is
/// enough to tell a real edit from a no-op.
///
/// The poll is four `metadata` calls per tick and nothing else — no file is read
/// unless a stamp moved. All four are read on any change, so the payload is
/// always a complete picture. A file that fails to parse contributes `None` for
/// its half, which tells the frontend to keep what it already has rather than
/// reset.
fn watch_customization(app: tauri::AppHandle) {
    if customize::safe_mode() {
        return;
    }

    tauri::async_runtime::spawn(async move {
        let mut last_config: [Option<(std::time::SystemTime, u64)>; 5] = [None; 5];
        let mut last_pending: [Option<(std::time::SystemTime, u64)>; 4] = [None; 4];

        loop {
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;

            let config_stamps = [
                stamp(&customize::theme_path()),
                stamp(&layout::layout_path()),
                stamp(&actions::actions_path()),
                stamp(&panels::panels_path()),
                // A widget's html may live in `widgets/<name>.html` rather than
                // in panels.json, so the directory is stamped too. Without it an
                // edit to a widget file would sit on disk until the next restart,
                // which is the opposite of editing the harness live.
                panels::widgets_stamp(),
            ];
            // The pending directory is watched separately so a proposal does not
            // force a re-read of four config files that did not move, and a
            // config edit does not re-parse proposals the user is still reading.
            let pending_stamps = sources::ConfigKind::ALL
                .map(|kind| stamp(&sources::pending_path(kind)));

            let config_changed = config_stamps != last_config;
            let pending_changed = pending_stamps != last_pending;
            if !config_changed && !pending_changed {
                continue;
            }
            last_config = config_stamps;
            last_pending = pending_stamps;

            // Let an atomic save finish before reading.
            tokio::time::sleep(std::time::Duration::from_millis(80)).await;

            if pending_changed {
                let _ = app.emit(sources::EVENT_PENDING, sources::read_pending());
            }

            if !config_changed {
                continue;
            }

            let mut errors: Vec<String> = Vec::new();

            let theme = match customize::load_theme() {
                Ok(theme) => Some(theme),
                Err(error) => {
                    errors.push(error);
                    None
                }
            };
            let layout = match layout::load_layout() {
                Ok(layout) => Some(layout),
                Err(error) => {
                    errors.push(error);
                    None
                }
            };
            let actions = match actions::load_actions() {
                Ok(actions) => Some(actions),
                Err(error) => {
                    errors.push(error);
                    None
                }
            };
            let panels = match panels::load_panels() {
                Ok(panels) => Some(panels),
                Err(error) => {
                    errors.push(error);
                    None
                }
            };

            // Plugin panels are read on every load, so a config edit must not
            // drop them from the rail: the payload is the whole list, and the
            // user's file is only half of it.
            let cwd = match app.try_state::<AppState>() {
                Some(state) => state.bridge.cwd().await,
                None => None,
            };
            let panels = panels.map(|config| plugin_panels::merged(cwd.as_deref(), config));

            // The widget documents the webview is about to be told about.
            // Updated before the emit so a frame that mounts on this payload can
            // fetch its document on the same tick. A config that failed to parse
            // leaves the previous registry in place, which matches the frontend
            // keeping its last good config.
            if let (Some(config), Some(state)) = (panels.as_ref(), app.try_state::<AppState>()) {
                if let Ok(mut registry) = state.widgets.lock() {
                    *registry = widget::Registry::from_config(config);
                }
            }

            // Bring the data plane in line with what the panels now declare.
            // Done before the emit so the webview receives a config whose
            // sources are already running, not one that starts them a tick
            // later. A panel that failed to parse contributes nothing, so a
            // broken file stops its own sources rather than leaving them
            // orphaned.
            if let Some(state) = app.try_state::<AppState>() {
                let declared = panels
                    .as_ref()
                    .map(|config| config.panels.as_slice())
                    .unwrap_or(&[]);
                state.data.reconcile(&app, declared);
            }

            let _ = app.emit(
                customize::EVENT_CONFIG,
                customize::ConfigPayload {
                    theme,
                    layout,
                    actions,
                    panels,
                    error: if errors.is_empty() {
                        None
                    } else {
                        Some(errors.join("\n"))
                    },
                    source: "user".into(),
                },
            );

            // The agent's map of what it may change. Rewritten after the emit so
            // it describes the state the webview just received, not the one
            // before it. A failure is logged and ignored: the app works without
            // a manifest, the agent just loses its map.
            if let Err(error) = manifest::write() {
                eprintln!("vibedesk: could not write the manifest: {error}");
            }
        }
    });
}

/// (mtime, length) for a file, or `None` when it does not exist. A missing file
/// is a real state — it means "use the default" — so it must be distinguishable
/// from "unchanged".
fn stamp(path: &std::path::Path) -> Option<(std::time::SystemTime, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((
        meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH),
        meta.len(),
    ))
}

fn pin_webview_data_dir() {
    if std::env::var_os("WEBVIEW2_USER_DATA_FOLDER").is_some() {
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let Some(dir) = exe.parent() else {
        return;
    };
    let target = dir.join("data").join("webview2");
    if std::fs::create_dir_all(&target).is_ok() {
        std::env::set_var("WEBVIEW2_USER_DATA_FOLDER", target);
    }
}

/// The browser arguments that open a CDP endpoint, or `None` when the request
/// is not one this build will honour.
///
/// `--remote-debugging-port` hands any local process the ability to drive the
/// webview, so it is opt-in and never in a release build: a shipped app that can
/// be inspected is a shipped app that can be driven. Port 0 is refused because a
/// random port cannot be found again, and an existing flag is left alone so a
/// caller that set its own arguments keeps them.
fn remote_debugging_args(existing: &str, port: u16) -> Option<String> {
    if port == 0 || existing.contains("--remote-debugging-port") {
        return None;
    }
    let flag = format!("--remote-debugging-port={port}");
    Some(if existing.trim().is_empty() {
        flag
    } else {
        format!("{existing} {flag}")
    })
}

/// The port the CDP endpoint was opened on, if it was.
///
/// A debugging port is a capability, not a setting: any local process that
/// finds it can drive this window, read the DOM and evaluate script. It is
/// debug-only and opt-in, but opt-in is not the same as invisible — the user
/// should be able to see that it is on, which is what this records.
static DEBUG_ENDPOINT: std::sync::OnceLock<u16> = std::sync::OnceLock::new();

/// The port the CDP endpoint is listening on, or `null` when it is off.
#[tauri::command]
fn debug_endpoint() -> Option<u16> {
    DEBUG_ENDPOINT.get().copied()
}

/// What this binary was built from.
///
/// The frontend is embedded at compile time, so "which UI am I looking at" is
/// not answerable from the source tree — only from the binary. Reporting the
/// bundle name is what makes a stale build identifiable instead of merely
/// suspicious.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct BuildInfo {
    version: &'static str,
    /// The `assets/index-*.js` embedded when this binary was compiled.
    bundle: &'static str,
    /// True when `dist/` was older than the frontend sources at build time.
    dist_stale: bool,
}

#[tauri::command]
fn build_info() -> BuildInfo {
    BuildInfo {
        version: env!("CARGO_PKG_VERSION"),
        bundle: env!("VIBEDESK_BUNDLE"),
        dist_stale: env!("VIBEDESK_DIST_STALE") == "1",
    }
}

/// Open a CDP endpoint when `VIBEDESK_REMOTE_DEBUGGING` names a port.
///
/// This is what makes live verification possible: the app runs, and a client
/// attaches over loopback to read the console, evaluate expressions and take
/// screenshots. It is the only way to check the things a unit test cannot — that
/// a panel renders, that a tab keeps its selection across a data tick, that a
/// widget's frame is sandboxed.
fn enable_remote_debugging() {
    if !cfg!(debug_assertions) {
        return;
    }
    let Ok(raw) = std::env::var("VIBEDESK_REMOTE_DEBUGGING") else {
        return;
    };
    let port: u16 = match raw.trim().parse() {
        Ok(port) => port,
        Err(_) => {
            eprintln!("vibedesk: VIBEDESK_REMOTE_DEBUGGING is not a port number: {raw:?}");
            return;
        }
    };
    let existing = std::env::var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS").unwrap_or_default();
    match remote_debugging_args(&existing, port) {
        Some(combined) => {
            std::env::set_var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS", combined);
            let _ = DEBUG_ENDPOINT.set(port);
            eprintln!("vibedesk: CDP endpoint on http://127.0.0.1:{port}");
        }
        None if port == 0 => {
            eprintln!("vibedesk: VIBEDESK_REMOTE_DEBUGGING must not be 0");
        }
        None => {}
    }
}

/// Point every Tauri app directory at the one root the rest of the app uses.
///
/// Tauri resolves `app_config_dir`, `app_data_dir`, `app_local_data_dir`,
/// `app_cache_dir` and `app_log_dir` under `%LOCALAPPDATA%\<identifier>` on
/// Windows and `~/Library/Application Support/<identifier>` on macOS, and on
/// Windows and Linux it also forces the webview's data directory to
/// `app_local_data_dir` and creates it. That is a folder on `C:` no matter
/// where the user installed the app, which defeats the installer's folder
/// picker. The override is the supported way to move all five at once.
///
/// The root is `paths::app_data_dir()`, which is already the single source of
/// truth for where VibeDesk keeps its state: `<exe dir>/data` when that is
/// writable, the per-user directory otherwise. It is absolute, which matters
/// because Tauri reads a relative override as relative to the executable.
fn pin_app_directories(context: &mut tauri::Context) {
    context.config_mut().app.app_directories_override = Some(
        tauri::utils::config::AppDirectoriesOverride::Root(paths::app_data_dir()),
    );
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    pin_webview_data_dir();
    enable_remote_debugging();

    let mut context = tauri::generate_context!();
    pin_app_directories(&mut context);

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        // The `widget` block's documents. A custom scheme is served on Windows
        // as `http://widget.localhost`, which is a different origin from the
        // app, so the response can carry its own `Content-Security-Policy`
        // instead of inheriting `script-src 'self'`. That is the whole reason a
        // widget can run scripts while the app's own policy stays closed. See
        // `widget.rs`.
        .register_uri_scheme_protocol(widget::SCHEME, |ctx, request| {
            let uri = request.uri().to_string();
            let empty = widget::Registry::default();
            let Some(state) = ctx.app_handle().try_state::<AppState>() else {
                return widget::serve(&empty, &uri);
            };
            let response = match state.widgets.lock() {
                Ok(registry) => widget::serve(&registry, &uri),
                Err(_) => widget::serve(&empty, &uri),
            };
            response
        })
        .setup(|app| {
            let bridge = Arc::new(AcpBridge::new(app.handle().clone()));
            app.manage(AppState {
                bridge,
                plugins: Mutex::new(Vec::new()),
                data: data::DataPlane::new(),
                widgets: Mutex::new(widget::Registry::default()),
            });
            watch_customization(app.handle().clone());
            // The agent the user is talking to can start a swarm, but only by
            // asking the process that owns the driver lock. This is that process.
            commands::watch(app.handle().clone());
            // The channeler is the app's own loopback MCP server, so it has to be
            // listening before a session asks for its server list. A failure here
            // is not fatal: the session simply has no channel tools, which is
            // what a build without them should look like.
            {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(error) = channel::mcp::ensure_started(handle).await {
                        eprintln!("vibedesk: the channeler is not available: {error}");
                    }
                });
            }
            // The capability layer reads its own file, so it is loaded before
            // anything can ask it a question. Three capabilities live at the
            // proxy boundary — compaction, reading and terminal — so the proxy
            // has to be listening whenever any of them is on, and the agent has
            // to be pointed at it.
            let capability_settings = capabilities::settings::load();
            capabilities::registry().load(&capability_settings);
            if capabilities::proxy::needed() {
                tauri::async_runtime::spawn(async move {
                    let outcome = match capabilities::proxy::ensure_started().await {
                        Ok(port) => capabilities::provider::enable(port).map(|_| ()),
                        Err(error) => Err(error),
                    };
                    if let Err(error) = outcome {
                        eprintln!("vibedesk: the capability proxy is not available: {error}");
                        // All three boundary capabilities are unreachable
                        // without the proxy, so all three say so rather than
                        // reporting a healthy zero.
                        for id in [
                            capabilities::CapabilityId::Compaction,
                            capabilities::CapabilityId::Reading,
                            capabilities::CapabilityId::Terminal,
                        ] {
                            if capabilities::registry().is_enabled(id) {
                                capabilities::registry().set_blocked(id, Some(error.to_string()));
                            }
                        }
                        // The provider left in `config.toml` points at a port
                        // nothing is listening on, so keeping it offers the user
                        // a model that cannot answer — and the failure it gives
                        // is "All connection attempts failed", which names
                        // neither the proxy nor the port. Removing it is the
                        // honest state; a later start adds it back.
                        if let Err(stale) = capabilities::provider::disable() {
                            eprintln!(
                                "vibedesk: the stale proxy provider could not be removed: {stale}"
                            );
                        }
                    }
                });
            }
            // Start the sources the panels already declare, so a panel is live
            // on launch rather than after the first config edit. The bridge has
            // no workspace yet, so this uses the one the agent will start in.
            if let Some(state) = app.try_state::<AppState>() {
                let cwd = paths::default_workspace();
                match panels::load_panels() {
                    Ok(config) => {
                        let live = plugin_panels::merged(Some(&cwd), config);
                        state.data.reconcile(app.handle(), &live.panels);
                        if let Ok(mut registry) = state.widgets.lock() {
                            *registry = widget::Registry::from_config(&live);
                        }
                    }
                    Err(error) => {
                        eprintln!("vibedesk: could not start the panel sources: {error}")
                    }
                }
            }
            // Write the manifest once at boot so the agent has a map even if no
            // config file ever changes during the session.
            if let Err(error) = manifest::write() {
                eprintln!("vibedesk: could not write the manifest: {error}");
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::agent_status,
            commands::agent_start,
            commands::agent_stop,
            commands::agent_restart,
            commands::provision_status,
            commands::provision_run,
            commands::provision_run_step,
            commands::session_new,
            commands::session_load,
            commands::session_prompt,
            commands::session_cancel,
            commands::session_list,
            commands::session_set_mode,
            commands::session_set_config_option,
            commands::session_set_title,
            commands::session_fork,
            commands::respond_permission,
            commands::respond_elicitation,
            commands::vibe_environment,
            commands::vibe_config_schema,
            commands::vibe_auth_status,
            commands::vibe_whoami,
            commands::vibe_account,
            commands::vibe_log_level,
            commands::vibe_release_notes,
            commands::vibe_install_status,
            commands::vibe_install,
            commands::vibe_skills,
            commands::plugins_list,
            commands::plugins_reload,
            commands::plugins_set_enabled,
            commands::workspace_files,
            commands::workspace_read,
            commands::workspace_apply_edit,
            commands::workspace_reveal,
            commands::image_read,
            commands::workstations_list,
            commands::workstation_add,
            commands::workstation_remove,
            commands::workstation_rename,
            commands::agent_set_workspace,
            commands::customize_paths,
            commands::chats_state,
            commands::chats_archive,
            commands::chats_archive_many,
            commands::chats_delete,
            commands::chats_delete_many,
            commands::prefs_get,
            commands::prefs_set,
            commands::theme_get,
            commands::theme_set,
            commands::theme_reset,
            commands::layout_get,
            commands::layout_set,
            commands::layout_reset,
            commands::actions_get,
            commands::actions_set,
            commands::actions_reset,
            commands::panels_get,
            commands::panels_set,
            commands::panels_reset,
            commands::panels_plugin_issues,
            commands::data_status,
            commands::data_set_paused,
            commands::data_consent_list,
            commands::data_consent_set,
            commands::sources_pending,
            commands::sources_apply,
            commands::sources_reject,
            commands::sources_plugins,
            commands::sources_apply_plugin,
            commands::capabilities_snapshot,
            commands::capabilities_channel_status,
            commands::capabilities_set_enabled,
            commands::capabilities_settings_get,
            commands::capabilities_settings_set,
            commands::capabilities_reset,
            commands::capabilities_compaction_status,
            commands::capabilities_memory_status,
            commands::capabilities_memory_dream,
            commands::capabilities_memory_forget,
            commands::capabilities_external_status,
            commands::capabilities_external_register,
            commands::capabilities_external_unregister,
            commands::capabilities_codegraph,
            commands::capabilities_codegraph_graph,
            commands::capabilities_terminal_rules,
            commands::capabilities_reading_probe,
            commands::swarm_snapshot,
            commands::swarm_rehearse,
            commands::swarm_rehearse_example,
            commands::swarm_example_plan,
            commands::swarm_start,
            commands::swarm_approve,
            commands::swarm_stop,
            commands::swarm_request_stop,
            commands::swarm_delete,
            commands::swarm_open,
            commands::swarm_preconditions,
            commands::swarm_catalog,
            commands::swarm_skills,
            commands::swarm_skill_accept,
            commands::swarm_models,
            commands::update_check,
            commands::update_apply,
            debug_endpoint,
            build_info,
        ])
        .build(context)
        .expect("VibeDesk could not start")
        .run(|handle, event| {
            if let tauri::RunEvent::ExitRequested { .. } = event {
                if let Some(state) = handle.try_state::<AppState>() {
                    let bridge = state.bridge.clone();
                    tauri::async_runtime::block_on(async move {
                        bridge.shutdown().await;
                    });
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::remote_debugging_args;

    #[test]
    fn remote_debugging_appends_to_existing_arguments() {
        let combined = remote_debugging_args("--disable-gpu", 9222).unwrap();
        assert_eq!(combined, "--disable-gpu --remote-debugging-port=9222");
    }

    #[test]
    fn remote_debugging_stands_alone_when_nothing_else_is_set() {
        assert_eq!(
            remote_debugging_args("", 9222).unwrap(),
            "--remote-debugging-port=9222"
        );
        assert_eq!(
            remote_debugging_args("   ", 9222).unwrap(),
            "--remote-debugging-port=9222"
        );
    }

    #[test]
    fn remote_debugging_refuses_port_zero() {
        assert!(remote_debugging_args("", 0).is_none());
    }

    #[test]
    fn remote_debugging_does_not_duplicate_a_flag_the_caller_set() {
        assert!(remote_debugging_args("--remote-debugging-port=9333", 9222).is_none());
    }
}
