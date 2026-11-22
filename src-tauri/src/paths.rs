use std::path::{Path, PathBuf};

use crate::error::{AppError, AppResult};
/// Drop the Windows verbatim prefix from a path.
///
/// `std::fs::canonicalize` returns `\\?\D:\project` on Windows. That form is
/// correct for the filesystem but wrong for anything that compares or displays
/// paths: the agent keys its session history on the plain form, so a verbatim
/// cwd never matches its own chats, and the title bar shows the prefix.
pub fn strip_verbatim(path: &str) -> String {
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = path.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        path.to_string()
    }
}

/// `$HOME` on POSIX, `%USERPROFILE%` on Windows.
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

/// `$VIBE_HOME`, else `~/.vibe`.
pub fn vibe_home() -> PathBuf {
    if let Some(value) = std::env::var_os("VIBE_HOME") {
        let path = PathBuf::from(value);
        if !path.as_os_str().is_empty() {
            return path;
        }
    }
    home_dir()
        .map(|home| home.join(".vibe"))
        .unwrap_or_else(|| PathBuf::from(".vibe"))
}

/// Directory holding VibeDesk's own settings.
///
/// Resolution order:
/// 1. `VIBEDESK_DATA_DIR`, for tests and unusual deployments.
/// 2. `<folder containing VibeDesk.exe>/data`, when that folder is writable.
///    The installer lets the user pick the install folder, so an install on
///    `D:` keeps its settings on `D:` instead of scattering them into
///    `%APPDATA%` on `C:`.
/// 3. The per-user platform directory, for read-only installs such as
///    `C:\Program Files`.
pub fn app_data_dir() -> PathBuf {
    static RESOLVED: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    RESOLVED.get_or_init(resolve_app_data_dir).clone()
}

fn resolve_app_data_dir() -> PathBuf {
    if let Some(value) = std::env::var_os("VIBEDESK_DATA_DIR") {
        let path = PathBuf::from(value);
        if !path.as_os_str().is_empty() {
            return path;
        }
    }

    if let Some(portable) = portable_data_dir() {
        return portable;
    }

    #[cfg(target_os = "windows")]
    {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            return PathBuf::from(appdata).join("VibeDesk");
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = home_dir() {
            return home.join("Library/Application Support/VibeDesk");
        }
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg).join("vibedesk");
    }
    home_dir()
        .map(|home| home.join(".config/vibedesk"))
        .unwrap_or_else(|| PathBuf::from(".vibedesk"))
}

/// `<exe dir>/data`, but only if we can actually write there.
fn portable_data_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.join("data");
    if is_writable(&dir) {
        Some(dir)
    } else {
        None
    }
}

fn is_writable(dir: &Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(".write-probe");
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

pub fn settings_path() -> PathBuf {
    app_data_dir().join("settings.json")
}

/// Where the agent binary was found, and how.
#[derive(Debug, Clone)]
pub struct BinaryResolution {
    pub path: PathBuf,
    pub source: String,
}

/// Resolve `vibe-acp`.
///
/// Order: explicit override, PATH, then the install locations used by
/// `uv tool install` and `pipx`, so a desktop launch (which inherits a
/// minimal PATH) still finds the CLI.
pub fn resolve_acp_binary() -> AppResult<BinaryResolution> {
    if let Some(value) = std::env::var_os("VIBEDESK_ACP_BIN") {
        let path = PathBuf::from(value);
        if path.is_file() {
            return Ok(BinaryResolution {
                path,
                source: "VIBEDESK_ACP_BIN".into(),
            });
        }
        return Err(AppError::BinaryNotFound(format!(
            "VIBEDESK_ACP_BIN points at {}, which is not a file",
            path.display()
        )));
    }

    let names: &[&str] = if cfg!(windows) {
        &["vibe-acp.exe", "vibe-acp"]
    } else {
        &["vibe-acp"]
    };

    if let Some(path) = search_path(names) {
        return Ok(BinaryResolution {
            path,
            source: "PATH".into(),
        });
    }

    for candidate in known_install_dirs() {
        for name in names {
            let path = candidate.join(name);
            if path.is_file() {
                return Ok(BinaryResolution {
                    path,
                    source: format!("install dir {}", candidate.display()),
                });
            }
        }
    }

    Err(AppError::BinaryNotFound(
        "install it with `uv tool install mistral-vibe`, or set VIBEDESK_ACP_BIN".into(),
    ))
}

/// The file names a program can have on this platform.
///
/// Windows programs are `git.exe`, not `git`. Looking for the bare name found
/// nothing on a machine where git was installed and on `PATH`, which made the
/// swarm's "git on PATH" precondition report a missing tool that was present.
fn candidates(name: &str) -> Vec<String> {
    if cfg!(windows) && !name.to_ascii_lowercase().ends_with(".exe") {
        vec![format!("{name}.exe"), name.to_string()]
    } else {
        vec![name.to_string()]
    }
}

fn search_path(names: &[&str]) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        for name in names {
            for candidate in candidates(name) {
                let path = dir.join(candidate);
                if path.is_file() {
                    return Some(path);
                }
            }
        }
    }
    None
}

/// Locate a tool by name across `PATH` and the directories package managers
/// install into. Used to find `uv`, `pipx` and `python` for the first-run
/// install flow, where a desktop launch's minimal `PATH` is the usual problem.
pub fn find_program(names: &[&str]) -> Option<PathBuf> {
    if let Some(path) = search_path(names) {
        return Some(path);
    }
    for dir in tool_dirs() {
        for name in names {
            for candidate in candidates(name) {
                let path = dir.join(candidate);
                if path.is_file() {
                    return Some(path);
                }
            }
        }
    }
    None
}

fn tool_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = home_dir() {
        dirs.push(home.join(".local/bin"));
        dirs.push(home.join(".cargo/bin"));
    }
    if cfg!(windows) {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            dirs.push(PathBuf::from(appdata).join("Python/Scripts"));
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(PathBuf::from(local).join("Programs/Python/Scripts"));
        }
    }
    dirs
}

fn known_install_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = home_dir() {
        dirs.push(home.join(".local/bin"));
        dirs.push(home.join(".cargo/bin"));
        dirs.push(home.join(".local/share/uv/tools/mistral-vibe/bin"));
        dirs.push(home.join(".local/share/pipx/venvs/mistral-vibe/bin"));
        dirs.push(home.join("Library/Application Support/uv/tools/mistral-vibe/bin"));
    }
    if cfg!(windows) {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            let appdata = PathBuf::from(appdata);
            dirs.push(appdata.join("uv/tools/mistral-vibe/Scripts"));
            dirs.push(appdata.join("Python/Scripts"));
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(PathBuf::from(local).join("uv/tools/mistral-vibe/Scripts"));
        }
    }
    dirs
}

/// The folder a new session opens in.
///
/// `VIBEDESK_CWD` wins, then the last folder the user opened, then the home
/// directory. A desktop launch has no meaningful process cwd, so guessing
/// from it would land in `C:\Windows\System32`.
pub fn default_workspace() -> PathBuf {
    if let Some(value) = std::env::var_os("VIBEDESK_CWD") {
        let path = PathBuf::from(value);
        if path.is_dir() {
            return path;
        }
    }
    if let Some(last) = crate::settings::load().last_workspace {
        let path = PathBuf::from(last);
        if path.is_dir() {
            return path;
        }
    }
    home_dir().unwrap_or_else(|| PathBuf::from("."))
}

/// Locate the `vibe` Python package directory, used for bundled release notes.
pub fn vibe_package_dir(acp_binary: &Path) -> Option<PathBuf> {
    if let Some(value) = std::env::var_os("VIBEDESK_VIBE_PACKAGE") {
        let path = PathBuf::from(value);
        if path.is_dir() {
            return Some(path);
        }
    }

    let bin_dir = acp_binary.parent()?;

    // uv tool layout: <tool>/Scripts/vibe-acp.exe + <tool>/Lib/site-packages/vibe
    let windows_layout = bin_dir.join("../Lib/site-packages/vibe");
    if windows_layout.is_dir() {
        return Some(normalize(windows_layout));
    }

    // POSIX uv/pipx layout: <venv>/bin/vibe-acp + <venv>/lib/python3.x/site-packages/vibe
    let lib_dir = bin_dir.join("../lib");
    if let Ok(entries) = std::fs::read_dir(&lib_dir) {
        for entry in entries.flatten() {
            let candidate = entry.path().join("site-packages/vibe");
            if candidate.is_dir() {
                return Some(normalize(candidate));
            }
        }
    }

    None
}

fn normalize(path: PathBuf) -> PathBuf {
    std::fs::canonicalize(&path).unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `lib.rs` hands this path to Tauri as `appDirectoriesOverride`, and Tauri
    /// reads a relative override as relative to the executable. A relative
    /// answer here would silently move every app directory somewhere else.
    #[test]
    fn the_data_dir_is_absolute() {
        assert!(app_data_dir().is_absolute(), "{:?}", app_data_dir());
    }

    /// The override is a single root, so every file the app owns has to sit
    /// under it. `settings.json` is the one the app reads before anything else.
    #[test]
    fn settings_live_under_the_data_dir() {
        assert_eq!(settings_path(), app_data_dir().join("settings.json"));
    }

    #[test]
    fn the_verbatim_prefix_is_dropped() {
        assert_eq!(strip_verbatim(r"\\?\D:\project"), r"D:\project");
        assert_eq!(strip_verbatim(r"\\?\UNC\host\share"), r"\\host\share");
        assert_eq!(strip_verbatim(r"D:\project"), r"D:\project");
    }
}
