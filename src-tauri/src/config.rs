use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::paths;

/* ------------------------------------------------------------------ *
 * config.toml
 * ------------------------------------------------------------------ */

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct VibeConfig {
    pub active_model: Option<String>,
    pub default_agent: Option<String>,
    pub theme: Option<String>,
    pub vibe_base_url: Option<String>,
    pub enable_update_checks: Option<bool>,
    pub providers: Vec<ProviderConfig>,
    pub models: Vec<ModelConfig>,
    pub mcp_servers: Vec<McpServerConfig>,
    pub disabled_plugins: Vec<String>,
    pub enabled_plugins: Vec<String>,
    pub skill_paths: Vec<String>,
    pub agent_paths: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ProviderConfig {
    pub name: String,
    pub api_base: String,
    pub api_key_env_var: String,
    pub backend: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ModelConfig {
    pub name: String,
    pub provider: String,
    pub alias: String,
    pub display_name: Option<String>,
    pub thinking: Option<String>,
    /// The reasoning levels this model accepts, as the file declares them.
    ///
    /// Empty means the file did not say, which is not the same as "none": the
    /// swarm then treats the level as unconstrained rather than refusing a run.
    pub thinking_levels: Vec<String>,
    pub supports_images: bool,
    pub max_context_length: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct McpServerConfig {
    pub name: String,
    pub transport: String,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub url: Option<String>,
    pub auth: Option<McpAuth>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct McpAuth {
    #[serde(rename = "type")]
    pub kind: String,
    pub api_key_env: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct HooksFile {
    pub hooks: Vec<HookConfig>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct HookConfig {
    pub name: String,
    #[serde(rename = "type")]
    pub event: String,
    #[serde(rename = "match")]
    pub matcher: Option<String>,
    pub command: String,
    pub description: Option<String>,
}

/* ------------------------------------------------------------------ *
 * Wire types
 * ------------------------------------------------------------------ */

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRecord {
    pub name: String,
    pub api_base: String,
    pub api_key_env_var: String,
    pub backend: String,
    pub has_key: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRecord {
    pub name: String,
    pub provider: String,
    pub alias: String,
    pub display_name: Option<String>,
    pub thinking: Option<String>,
    pub supports_images: bool,
    pub context_window: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerRecord {
    pub name: String,
    pub transport: String,
    pub target: String,
    pub auth: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HookRecord {
    pub name: String,
    pub event: String,
    pub matcher: Option<String>,
    pub command: String,
    pub scope: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VibeEnvironment {
    pub vibe_home: String,
    pub config_path: String,
    pub config_exists: bool,
    pub hooks_path: String,
    pub plugins_dirs: Vec<String>,
    pub active_model: Option<String>,
    pub default_agent: Option<String>,
    pub theme: Option<String>,
    pub providers: Vec<ProviderRecord>,
    pub models: Vec<ModelRecord>,
    pub mcp_servers: Vec<McpServerRecord>,
    pub hooks: Vec<HookRecord>,
    pub config_issues: Vec<String>,
    pub cli_version: Option<String>,
    pub acp_binary: Option<String>,
    pub acp_binary_source: String,
    pub api_key_present: bool,
    /// Where VibeDesk keeps its own settings. Portable installs put this next
    /// to the executable; read-only installs fall back to the user profile.
    pub data_dir: String,
}

/* ------------------------------------------------------------------ *
 * Loading
 * ------------------------------------------------------------------ */

pub fn user_config_path() -> PathBuf {
    paths::vibe_home().join("config.toml")
}

pub fn project_config_path(cwd: Option<&Path>) -> Option<PathBuf> {
    let cwd = cwd?;
    let candidate = cwd.join(".vibe/config.toml");
    candidate.is_file().then_some(candidate)
}

pub fn load_config_file(path: &Path) -> Result<VibeConfig, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    toml::from_str(&raw).map_err(|e| format!("{}: {e}", path.display()))
}

/// Effective config: project layer over user layer, matching Vibe's precedence.
pub fn load_effective(cwd: Option<&Path>) -> (VibeConfig, Vec<String>) {
    let mut issues = Vec::new();
    let mut config = VibeConfig::default();

    let user_path = user_config_path();
    if user_path.is_file() {
        match load_config_file(&user_path) {
            Ok(parsed) => config = parsed,
            Err(error) => issues.push(format!("config.toml could not be parsed — {error}")),
        }
    }

    if let Some(project_path) = project_config_path(cwd) {
        match load_config_file(&project_path) {
            Ok(project) => merge(&mut config, project),
            Err(error) => issues.push(format!(".vibe/config.toml could not be parsed — {error}")),
        }
    }

    (config, issues)
}

fn merge(base: &mut VibeConfig, overlay: VibeConfig) {
    if overlay.active_model.is_some() {
        base.active_model = overlay.active_model;
    }
    if overlay.default_agent.is_some() {
        base.default_agent = overlay.default_agent;
    }
    if overlay.theme.is_some() {
        base.theme = overlay.theme;
    }
    if !overlay.providers.is_empty() {
        base.providers = overlay.providers;
    }
    if !overlay.models.is_empty() {
        base.models = overlay.models;
    }
    if !overlay.mcp_servers.is_empty() {
        base.mcp_servers = overlay.mcp_servers;
    }
    base.disabled_plugins.extend(overlay.disabled_plugins);
    base.enabled_plugins.extend(overlay.enabled_plugins);
    base.skill_paths.extend(overlay.skill_paths);
    base.agent_paths.extend(overlay.agent_paths);
}

/// The MCP servers to declare to the agent when a session starts.
///
/// ACP carries the client's server list on `session/new`, `session/load` and
/// `session/fork`. VibeDesk used to send an empty array here, which was read as
/// "this client has no servers to add".
///
/// That is not the same as "the agent has none". `vibe-acp` integrates the
/// servers in its own `config.toml` regardless of what the client sends — a
/// session opened with `mcpServers: []` still reaches `codebase-memory` and
/// `donsetch`, which was checked live against a real agent. So this list is
/// additive, and an empty one is not a way to take a capability away.
///
/// Entries that cannot start are dropped rather than declared: a stdio server
/// with no command, or a remote one with no URL, fails its handshake and takes
/// the rest of the session's MCP surface down with it.
pub fn mcp_servers_for_acp(cwd: Option<&Path>) -> Vec<serde_json::Value> {
    let (config, _) = load_effective(cwd);
    acp_servers_from(&config.mcp_servers)
}

/// The MCP servers a VibeDesk session gets.
///
/// The user's own servers, plus the swarm. The swarm server is deliberately not
/// written into `~/.vibe/config.toml`: it is a VibeDesk capability, it is only
/// meaningful while the app is running to answer it, and registering it globally
/// would hand it to every other client on the machine.
pub fn mcp_servers_for_session(cwd: Option<&Path>) -> Vec<serde_json::Value> {
    let mut servers = mcp_servers_for_acp(cwd);
    if let Some(swarm) = swarm_mcp_server() {
        servers.push(swarm);
    }
    // The channeler is the app's own loopback server, so it is registered only
    // once it is listening. Before that there is no URL to point at, and a
    // server entry with a dead URL fails its handshake and takes the rest of the
    // session's MCP surface down with it.
    if let Some(channeler) = crate::channel::mcp::server_entry() {
        servers.push(channeler);
    }
    servers
}

/// Where the swarm MCP server lives, if this build shipped one.
///
/// Beside the app in a bundle, beside the test binary in a dev tree. A missing
/// binary is not an error: the session simply has no swarm tools, which is what a
/// build without them should look like.
fn swarm_mcp_server() -> Option<serde_json::Value> {
    let exe = std::env::current_exe().ok()?;
    let name = if cfg!(windows) {
        "swarm-mcp.exe"
    } else {
        "swarm-mcp"
    };
    let path = exe.parent()?.join(name);
    if !path.is_file() {
        return None;
    }
    Some(serde_json::json!({
        "name": "vibedesk-swarm",
        "command": path.display().to_string(),
        "args": [],
        "env": [],
    }))
}

fn acp_servers_from(configured: &[McpServerConfig]) -> Vec<serde_json::Value> {
    use serde_json::json;

    let mut declared: Vec<String> = Vec::new();
    let mut servers = Vec::new();

    for server in configured {
        let name = server.name.trim();
        if name.is_empty() || declared.iter().any(|existing| existing == name) {
            continue;
        }

        let transport = if server.transport.is_empty() {
            "stdio"
        } else {
            server.transport.as_str()
        };

        let entry = match transport {
            "stdio" => {
                let Some(command) = server
                    .command
                    .as_deref()
                    .map(str::trim)
                    .filter(|command| !command.is_empty())
                else {
                    continue;
                };
                json!({
                    "name": name,
                    "command": command,
                    "args": server.args.clone(),
                    "env": [],
                })
            }
            "http" | "sse" => {
                let Some(url) = server
                    .url
                    .as_deref()
                    .map(str::trim)
                    .filter(|url| !url.is_empty())
                else {
                    continue;
                };
                // The config schema expresses auth as an environment variable
                // holding the token, so that is the whole of what can be
                // forwarded. A missing variable is not fatal: the agent may
                // still resolve the server through its own auth provider.
                let headers: Vec<serde_json::Value> = server
                    .auth
                    .as_ref()
                    .and_then(|auth| auth.api_key_env.as_deref())
                    .and_then(|variable| std::env::var(variable).ok())
                    .filter(|token| !token.is_empty())
                    .map(|token| {
                        vec![json!({
                            "name": "Authorization",
                            "value": format!("Bearer {token}"),
                        })]
                    })
                    .unwrap_or_default();
                json!({
                    "type": transport,
                    "name": name,
                    "url": url,
                    "headers": headers,
                })
            }
            _ => continue,
        };

        declared.push(name.to_string());
        servers.push(entry);
    }

    servers
}

pub fn load_hooks(cwd: Option<&Path>) -> Vec<HookRecord> {
    let mut records = Vec::new();

    let mut push_file = |path: PathBuf, scope: &str| {
        let Ok(raw) = std::fs::read_to_string(&path) else {
            return;
        };
        let Ok(parsed) = toml::from_str::<HooksFile>(&raw) else {
            return;
        };
        for hook in parsed.hooks {
            records.push(HookRecord {
                name: hook.name,
                event: hook.event,
                matcher: hook.matcher,
                command: hook.command,
                scope: scope.to_string(),
                source: path.display().to_string(),
            });
        }
    };

    if let Some(cwd) = cwd {
        push_file(cwd.join(".vibe/hooks.toml"), "project");
    }
    push_file(paths::vibe_home().join("hooks.toml"), "user");

    records
}

pub fn plugins_dirs(cwd: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = vec![paths::vibe_home().join("plugins")];
    if let Some(cwd) = cwd {
        dirs.push(cwd.join(".vibe/plugins"));
    }
    dirs
}

fn env_has(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|value| !value.is_empty())
}

pub fn build_environment(cwd: Option<&Path>) -> VibeEnvironment {
    let (config, mut issues) = load_effective(cwd);
    let config_path = user_config_path();
    let hooks_path = paths::vibe_home().join("hooks.toml");

    let providers: Vec<ProviderRecord> = config
        .providers
        .iter()
        .map(|provider| ProviderRecord {
            name: provider.name.clone(),
            api_base: provider.api_base.clone(),
            api_key_env_var: provider.api_key_env_var.clone(),
            backend: provider.backend.clone(),
            has_key: !provider.api_key_env_var.is_empty() && env_has(&provider.api_key_env_var),
        })
        .collect();

    let models: Vec<ModelRecord> = config
        .models
        .iter()
        .map(|model| ModelRecord {
            name: model.name.clone(),
            provider: model.provider.clone(),
            alias: model.alias.clone(),
            display_name: model.display_name.clone(),
            thinking: model.thinking.clone(),
            supports_images: model.supports_images,
            context_window: model.max_context_length,
        })
        .collect();

    let mcp_servers: Vec<McpServerRecord> = config
        .mcp_servers
        .iter()
        .map(|server| {
            let target = server
                .url
                .clone()
                .or_else(|| server.command.clone())
                .unwrap_or_default();
            let auth = match &server.auth {
                Some(auth) if !auth.kind.is_empty() => auth.kind.clone(),
                Some(auth) => auth.api_key_env.clone().unwrap_or_else(|| "static".into()),
                None => "none".into(),
            };
            McpServerRecord {
                name: server.name.clone(),
                transport: if server.transport.is_empty() {
                    "stdio".into()
                } else {
                    server.transport.clone()
                },
                target,
                auth,
            }
        })
        .collect();

    let api_key_present = env_has("MISTRAL_API_KEY")
        || providers.iter().any(|p| p.has_key);

    if !api_key_present {
        issues.push(
            "No MISTRAL_API_KEY in the environment. The agent loads ~/.vibe/.env itself; \
             VibeDesk never reads that file."
                .into(),
        );
    }

    let (acp_binary, acp_binary_source) = match paths::resolve_acp_binary() {
        Ok(resolution) => (
            Some(resolution.path.display().to_string()),
            resolution.source,
        ),
        Err(error) => (None, error.to_string()),
    };

    VibeEnvironment {
        vibe_home: paths::vibe_home().display().to_string(),
        config_path: config_path.display().to_string(),
        config_exists: config_path.is_file(),
        hooks_path: hooks_path.display().to_string(),
        plugins_dirs: plugins_dirs(cwd)
            .iter()
            .map(|p| p.display().to_string())
            .collect(),
        active_model: config.active_model.clone(),
        default_agent: config.default_agent.clone(),
        theme: config.theme.clone(),
        providers,
        models,
        mcp_servers,
        hooks: load_hooks(cwd),
        config_issues: issues,
        cli_version: None,
        acp_binary,
        acp_binary_source,
        api_key_present,
        data_dir: paths::app_data_dir().display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stdio(name: &str, command: Option<&str>, args: &[&str]) -> McpServerConfig {
        McpServerConfig {
            name: name.into(),
            transport: "stdio".into(),
            command: command.map(str::to_string),
            args: args.iter().map(|arg| (*arg).to_string()).collect(),
            ..McpServerConfig::default()
        }
    }

    #[test]
    fn a_stdio_server_is_declared_with_its_command_and_args() {
        let servers = acp_servers_from(&[stdio(
            "codebase-memory",
            Some("C:/tools/codebase-memory-mcp.exe"),
            &["--flag"],
        )]);
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0]["name"], "codebase-memory");
        assert_eq!(servers[0]["command"], "C:/tools/codebase-memory-mcp.exe");
        assert_eq!(servers[0]["args"][0], "--flag");
        assert!(servers[0]["env"].as_array().is_some_and(Vec::is_empty));
    }

    #[test]
    fn a_stdio_server_without_a_command_is_not_declared() {
        assert!(acp_servers_from(&[stdio("broken", None, &[])]).is_empty());
        assert!(acp_servers_from(&[stdio("blank", Some("   "), &[])]).is_empty());
    }

    #[test]
    fn a_remote_server_without_a_url_is_not_declared() {
        let server = McpServerConfig {
            name: "remote".into(),
            transport: "http".into(),
            ..McpServerConfig::default()
        };
        assert!(acp_servers_from(&[server]).is_empty());
    }

    #[test]
    fn a_remote_server_is_declared_with_its_url() {
        let server = McpServerConfig {
            name: "remote".into(),
            transport: "http".into(),
            url: Some("https://mcp.example.com/mcp".into()),
            ..McpServerConfig::default()
        };
        let servers = acp_servers_from(&[server]);
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0]["url"], "https://mcp.example.com/mcp");
        assert!(servers[0]["headers"].as_array().is_some_and(Vec::is_empty));
    }

    /// The agent parses `mcpServers` as a tagged union and answers a remote
    /// entry without `type` with `-32602 Invalid params`, which fails
    /// `session/new` for the whole session. The discriminator is required.
    #[test]
    fn a_remote_server_carries_its_transport_as_the_type_tag() {
        let http = McpServerConfig {
            name: "remote".into(),
            transport: "http".into(),
            url: Some("https://mcp.example.com/mcp".into()),
            ..McpServerConfig::default()
        };
        let sse = McpServerConfig {
            name: "stream".into(),
            transport: "sse".into(),
            url: Some("https://mcp.example.com/sse".into()),
            ..McpServerConfig::default()
        };
        let servers = acp_servers_from(&[http, sse]);
        assert_eq!(servers.len(), 2);
        assert_eq!(servers[0]["type"], "http");
        assert_eq!(servers[1]["type"], "sse");
    }

    #[test]
    fn a_repeated_name_is_declared_once() {
        let servers = acp_servers_from(&[
            stdio("donsetch", Some("a.exe"), &[]),
            stdio("donsetch", Some("b.exe"), &[]),
        ]);
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0]["command"], "a.exe");
    }

    #[test]
    fn an_unknown_transport_is_not_declared() {
        let server = McpServerConfig {
            name: "weird".into(),
            transport: "carrier-pigeon".into(),
            command: Some("x".into()),
            ..McpServerConfig::default()
        };
        assert!(acp_servers_from(&[server]).is_empty());
    }

    #[test]
    fn an_empty_transport_is_treated_as_stdio() {
        let server = McpServerConfig {
            name: "implicit".into(),
            command: Some("x.exe".into()),
            ..McpServerConfig::default()
        };
        let servers = acp_servers_from(&[server]);
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0]["command"], "x.exe");
    }

    /// Diagnostic, not an assertion: what this machine would declare to the
    /// agent. Ignored by default because it reads the real `config.toml`.
    #[test]
    #[ignore]
    fn print_the_servers_declared_to_the_agent() {
        let servers = mcp_servers_for_acp(None);
        println!(
            "{}",
            serde_json::to_string_pretty(&servers).unwrap_or_default()
        );
    }
}
