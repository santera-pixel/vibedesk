//! The two capabilities that are already servers.
//!
//! `codebase-memory-mcp` is a pure-C knowledge graph over 164 languages, and
//! `donsetch` is a Rust web reader with its own fetch engine. Both ship as
//! native binaries with their own MCP servers. Porting either one would be a
//! second product, not a capability — 1077 C files and a vendored V8 are not
//! something to reimplement behind a panel.
//!
//! So this module does the three things that *are* VibeDesk's job:
//!
//! 1. **Find** the binary, on `PATH` or in the app's own tools directory.
//! 2. **Register** it in the agent's `config.toml` as a stdio MCP server, so the
//!    agent gets its tools. Additive and reversible, like the proxy provider.
//! 3. **Report** whether it is installed, which version, and what it exposes, so
//!    the panel can say "not installed" instead of showing an empty list.
//!
//! A capability that is not installed is not a broken capability. It is a
//! capability with a reason, and the reason is what the panel shows.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::config;
use crate::error::{AppError, AppResult};

use super::{now, registry, CapabilityEvent, CapabilityId};

/// One external server VibeDesk knows how to register.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Server {
    /// The `[[mcp_servers]]` name, and the key the panel uses.
    pub name: &'static str,
    /// The executable to look for.
    pub binary: &'static str,
    /// The arguments that start its stdio MCP server.
    pub args: &'static [&'static str],
    /// Which capability it serves.
    pub capability: CapabilityId,
    /// What it gives the agent, in one line.
    pub gives: &'static str,
}

/// The servers this module manages.
pub const SERVERS: [Server; 2] = [
    Server {
        name: "codebase-memory",
        binary: "codebase-memory-mcp",
        args: &[],
        capability: CapabilityId::CodeGraph,
        gives: "A local knowledge graph of the repository, queried over MCP.",
    },
    Server {
        name: "donsetch",
        binary: "donsetch",
        args: &["mcp", "--supervised"],
        capability: CapabilityId::Web,
        gives: "Search, page reads, crawls and screenshots, without a browser.",
    },
];

/// The managed server that serves a capability, when there is one.
pub fn server(capability: CapabilityId) -> Option<&'static Server> {
    SERVERS.iter().find(|server| server.capability == capability)
}

/// What the panel shows for one server.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerStatus {
    pub name: String,
    pub binary: String,
    pub capability: String,
    pub gives: String,
    /// Where the executable is, when it was found.
    pub path: Option<String>,
    /// The version it reports, when it answers.
    pub version: Option<String>,
    /// Whether it is in the agent's `config.toml`.
    pub registered: bool,
    /// Whether VibeDesk declares it to the agent on `session/new`.
    ///
    /// Being in `config.toml` is not the same thing: the agent only gets the
    /// servers the client hands it, so a registered server that is not declared
    /// is a capability the agent cannot call.
    pub declared: bool,
    /// Why it cannot be used, when it cannot.
    pub blocked: Option<String>,
}

/// The file names to try, in the order that actually starts a process.
///
/// On Windows an extensionless file in an npm prefix is a POSIX shell script
/// with a `#!/bin/sh` shebang. `CreateProcess` cannot run it, so a server
/// registered against it never starts and never says why. `.cmd` and `.bat`
/// shims do run, because `CreateProcess` hands them to `%COMSPEC%`.
fn candidate_names(binary: &str) -> Vec<String> {
    if cfg!(windows) {
        vec![
            format!("{binary}.exe"),
            format!("{binary}.cmd"),
            format!("{binary}.bat"),
        ]
    } else {
        vec![binary.to_string()]
    }
}

/// Look for an executable on `PATH`, then in the app's tools directory.
pub fn find(binary: &str) -> Option<PathBuf> {
    let names = candidate_names(binary);

    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            for name in &names {
                let candidate = dir.join(name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }

    let local = crate::paths::app_data_dir().join("tools");
    for name in &names {
        let candidate = local.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Ask the binary what version it is.
///
/// A binary that does not answer is still usable — the version is for the panel,
/// not a precondition — so a failure here is `None`, not an error.
fn version_of(path: &Path) -> Option<String> {
    let output = std::process::Command::new(path)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let line = text.lines().next()?.trim();
    if line.is_empty() {
        None
    } else {
        Some(line.chars().take(80).collect())
    }
}

/// Every managed server, with its state.
pub fn status() -> Vec<ServerStatus> {
    let document = read_document().unwrap_or(toml::Value::Table(toml::map::Map::new()));
    let registered: Vec<String> = document
        .get("mcp_servers")
        .and_then(toml::Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|entry| {
                    entry
                        .get("name")
                        .and_then(toml::Value::as_str)
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default();

    let declared: Vec<String> = crate::config::mcp_servers_for_acp(None)
        .iter()
        .filter_map(|entry| {
            entry
                .get("name")
                .and_then(|name| name.as_str())
                .map(str::to_string)
        })
        .collect();

    SERVERS
        .iter()
        .map(|server| {
            let path = find(server.binary);
            let version = path.as_deref().and_then(version_of);
            let blocked = match &path {
                None => Some(format!(
                    "{} is not on PATH. Install it, or drop the binary in {}.",
                    server.binary,
                    crate::paths::app_data_dir().join("tools").display()
                )),
                Some(_) => None,
            };
            ServerStatus {
                name: server.name.to_string(),
                binary: server.binary.to_string(),
                capability: server.capability.key().to_string(),
                gives: server.gives.to_string(),
                path: path.map(|path| path.display().to_string()),
                version,
                registered: registered.iter().any(|name| name == server.name),
                declared: declared.iter().any(|name| name == server.name),
                blocked,
            }
        })
        .collect()
}

/// Register a server in the agent's `config.toml`.
pub fn register(name: &str) -> AppResult<Vec<ServerStatus>> {
    let server = SERVERS
        .iter()
        .find(|server| server.name == name)
        .ok_or_else(|| AppError::message(format!("{name} is not a managed server")))?;

    let path = find(server.binary).ok_or_else(|| {
        AppError::message(format!(
            "{} is not installed, so there is nothing to register",
            server.binary
        ))
    })?;

    let mut document = read_document()?;
    unregister_from(&mut document, server.name);

    let mut entry = toml::map::Map::new();
    entry.insert("name".into(), toml::Value::String(server.name.into()));
    entry.insert("transport".into(), toml::Value::String("stdio".into()));
    entry.insert(
        "command".into(),
        toml::Value::String(path.display().to_string()),
    );
    if !server.args.is_empty() {
        entry.insert(
            "args".into(),
            toml::Value::Array(
                server
                    .args
                    .iter()
                    .map(|arg| toml::Value::String((*arg).to_string()))
                    .collect(),
            ),
        );
    }
    push(&mut document, "mcp_servers", toml::Value::Table(entry));
    write_document(&document)?;

    registry().record(
        server.capability,
        CapabilityEvent {
            at: now(),
            label: "registered".into(),
            detail: format!("{} at {}", server.name, path.display()),
            bytes_in: 0,
            bytes_out: 0,
            tokens_in: 0,
            tokens_out: 0,
        },
    );
    registry().set_blocked(server.capability, None);
    Ok(status())
}

/// Remove a server from the agent's `config.toml`.
pub fn unregister(name: &str) -> AppResult<Vec<ServerStatus>> {
    let mut document = read_document()?;
    unregister_from(&mut document, name);
    write_document(&document)?;
    Ok(status())
}

fn unregister_from(document: &mut toml::Value, name: &str) {
    if let Some(list) = document
        .get_mut("mcp_servers")
        .and_then(toml::Value::as_array_mut)
    {
        list.retain(|entry| entry.get("name").and_then(toml::Value::as_str) != Some(name));
    }
}

fn push(document: &mut toml::Value, key: &str, value: toml::Value) {
    let Some(table) = document.as_table_mut() else {
        return;
    };
    match table.get_mut(key).and_then(toml::Value::as_array_mut) {
        Some(list) => list.push(value),
        None => {
            table.insert(key.to_string(), toml::Value::Array(vec![value]));
        }
    }
}

fn read_document() -> AppResult<toml::Value> {
    let path = config::user_config_path();
    let raw = std::fs::read_to_string(&path).unwrap_or_default();
    if raw.trim().is_empty() {
        return Ok(toml::Value::Table(toml::map::Map::new()));
    }
    toml::from_str(&raw).map_err(|error| AppError::message(format!("config.toml: {error}")))
}

fn write_document(document: &toml::Value) -> AppResult<()> {
    let path = config::user_config_path();
    if let Ok(raw) = std::fs::read_to_string(&path) {
        if !raw.trim().is_empty() {
            let _ = std::fs::write(path.with_extension("toml.bak"), raw);
        }
    }
    let body = toml::to_string_pretty(document)
        .map_err(|error| AppError::message(format!("could not serialise config.toml: {error}")))?;
    std::fs::write(&path, body)
        .map_err(|error| AppError::message(format!("could not write {}: {error}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_servers_name_a_distinct_capability_and_binary() {
        let mut names: Vec<&str> = SERVERS.iter().map(|server| server.name).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len());

        let mut binaries: Vec<&str> = SERVERS.iter().map(|server| server.binary).collect();
        binaries.sort_unstable();
        let before = binaries.len();
        binaries.dedup();
        assert_eq!(before, binaries.len());
    }

    #[test]
    fn a_binary_that_does_not_exist_is_not_found() {
        assert!(find("vibedesk-definitely-not-installed").is_none());
    }

    #[test]
    fn windows_candidates_never_include_a_bare_posix_shim() {
        let names = candidate_names("donsetch");
        if cfg!(windows) {
            assert!(
                !names.iter().any(|name| name == "donsetch"),
                "an extensionless npm shim is a #!/bin/sh script CreateProcess cannot run"
            );
            assert!(names.iter().any(|name| name == "donsetch.exe"));
            assert!(names.iter().any(|name| name == "donsetch.cmd"));
        } else {
            assert_eq!(names, vec!["donsetch".to_string()]);
        }
    }

    #[test]
    fn unregistering_leaves_the_users_own_servers_alone() {
        let mut document: toml::Value = toml::from_str(
            r#"
[[mcp_servers]]
name = "linear"
transport = "http"
url = "https://mcp.linear.app/mcp"

[[mcp_servers]]
name = "donsetch"
transport = "stdio"
command = "/usr/local/bin/donsetch"
"#,
        )
        .unwrap();

        unregister_from(&mut document, "donsetch");

        let list = document["mcp_servers"].as_array().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0]["name"].as_str(), Some("linear"));
    }

    #[test]
    fn registering_twice_does_not_stack_entries() {
        let mut document: toml::Value = toml::from_str("").unwrap();
        for _ in 0..2 {
            unregister_from(&mut document, "donsetch");
            push(
                &mut document,
                "mcp_servers",
                toml::from_str("name = \"donsetch\"\ntransport = \"stdio\"\ncommand = \"x\"\n")
                    .unwrap(),
            );
        }
        assert_eq!(document["mcp_servers"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn a_missing_binary_is_reported_as_a_reason_not_a_crash() {
        let statuses = status();
        assert_eq!(statuses.len(), SERVERS.len());
        for entry in &statuses {
            if entry.path.is_none() {
                assert!(entry.blocked.is_some());
                assert!(entry.blocked.as_deref().unwrap().contains("not on PATH"));
            }
        }
    }

    #[test]
    fn what_the_panel_calls_declared_is_what_the_agent_is_told() {
        let declared: Vec<String> = crate::config::mcp_servers_for_acp(None)
            .iter()
            .filter_map(|entry| {
                entry
                    .get("name")
                    .and_then(|name| name.as_str())
                    .map(str::to_string)
            })
            .collect();

        for entry in status() {
            assert_eq!(
                entry.declared,
                declared.iter().any(|name| name == &entry.name),
                "{} disagrees with the session payload",
                entry.name
            );
        }
    }
}
