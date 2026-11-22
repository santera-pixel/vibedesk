//! Per-origin consent for panel data sources.
//!
//! The loopback rule says a panel may only address this machine. That is a
//! necessary bound and not a sufficient one: a loopback service is usually
//! unauthenticated *because* it assumes only local processes can reach it, so
//! "this machine" still includes a database admin UI, a dev server with a debug
//! endpoint, and whatever else the user happens to be running.
//!
//! So a source needs two things, not one:
//!
//! 1. It must be loopback. Enforced by `data::check_loopback_url` at approval
//!    time; a non-loopback host is not expressible in `panels.json` at all.
//! 2. Its **origin** must be allowed by the user. Enforced here, at start time.
//!
//! The unit is the origin — `http://127.0.0.1:8787` — not the URL and not the
//! panel. One decision covers every path on that service and every panel that
//! reads it, which is the only granularity that does not turn into a prompt per
//! poll. Approving `:8787` says nothing about `:5432`.
//!
//! Three properties matter more than the mechanism:
//!
//! * **Fail closed.** A missing, unreadable or corrupt ledger means *no* origin
//!   is allowed. The failure mode of a broken file is a panel that asks, never a
//!   panel that reads.
//! * **Denial is sticky.** A refused origin stays refused across restarts, so a
//!   re-proposed panel does not nag. Only the user clears it.
//! * **The app writes this file, never the agent.** `consent.json` is not one of
//!   the four proposable config kinds, so the approval flow cannot reach it. An
//!   agent that writes arbitrary files can of course write this one — but such
//!   an agent already has the user's shell, and pretending otherwise would be
//!   the kind of claim this project is trying not to make.

// Wired. `data::split_by_consent` reads this ledger on every reconcile, and
// `data_consent_list` / `data_consent_set` are registered so the user can
// answer. An origin with no decision is a source that is declared and
// deliberately not fetched — the panel shows it as waiting for permission
// rather than as broken.
//
// The loopback rule still applies first: `data::check_loopback_url` refuses
// every non-loopback host, so consent can only ever widen what a panel reads
// *on this machine*, never what it can reach off it.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// How many origins the ledger will remember. A user with more local services
/// than this is not the case this is designed for, and an unbounded file is a
/// file an agent can grow.
pub const MAX_ORIGINS: usize = 64;

/// The ledger is small by construction; anything larger is not one.
pub const MAX_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Decision {
    Allow,
    Deny,
}

impl Decision {
    pub fn parse(value: &str) -> Option<Decision> {
        match value {
            "allow" => Some(Decision::Allow),
            "deny" => Some(Decision::Deny),
            _ => None,
        }
    }

    pub fn token(self) -> &'static str {
        match self {
            Decision::Allow => "allow",
            Decision::Deny => "deny",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub decision: Decision,
    pub at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ledger {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub origins: BTreeMap<String, Entry>,
}

fn default_version() -> u32 {
    1
}

impl Default for Ledger {
    fn default() -> Self {
        Ledger {
            version: 1,
            origins: BTreeMap::new(),
        }
    }
}

impl Ledger {
    /// The decision for an origin, or `None` when the user has not been asked.
    pub fn decision(&self, origin: &str) -> Option<Decision> {
        self.origins.get(origin).map(|entry| entry.decision)
    }

    pub fn allows(&self, origin: &str) -> bool {
        self.decision(origin) == Some(Decision::Allow)
    }
}

pub fn consent_path() -> PathBuf {
    super::customize::config_dir().join("consent.json")
}

/// Read the ledger. Every failure is an empty ledger, which allows nothing.
pub fn load() -> Ledger {
    load_at(&consent_path())
}

pub fn load_at(path: &std::path::Path) -> Ledger {
    let Ok(meta) = std::fs::metadata(path) else {
        return Ledger::default();
    };
    if meta.len() as usize > MAX_BYTES {
        return Ledger::default();
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ledger::default();
    };
    let Ok(mut ledger) = serde_json::from_str::<Ledger>(&text) else {
        return Ledger::default();
    };
    // A hand-edited file can carry an origin that is not loopback, or a key that
    // is not an origin at all. Neither is honoured: the ledger only ever speaks
    // about origins this module would have written.
    ledger
        .origins
        .retain(|origin, _| origin_of_origin(origin).is_some());
    ledger
}

/// Record a decision. `None` forgets the origin, which puts it back to "ask".
pub fn decide(origin: &str, decision: Option<Decision>) -> Result<Ledger, String> {
    decide_at(&consent_path(), origin, decision)
}

pub fn decide_at(
    path: &std::path::Path,
    origin: &str,
    decision: Option<Decision>,
) -> Result<Ledger, String> {
    let normalized = origin_of_origin(origin)
        .ok_or_else(|| format!("`{origin}` is not a loopback http origin"))?;

    let mut ledger = load_at(path);
    match decision {
        Some(decision) => {
            if !ledger.origins.contains_key(&normalized) && ledger.origins.len() >= MAX_ORIGINS {
                return Err(format!(
                    "the consent ledger already holds {MAX_ORIGINS} origins; remove one first"
                ));
            }
            ledger.origins.insert(
                normalized,
                Entry {
                    decision,
                    at: chrono::Utc::now().to_rfc3339(),
                    note: None,
                },
            );
        }
        None => {
            ledger.origins.remove(&normalized);
        }
    }

    save_at(path, &ledger)?;
    Ok(ledger)
}

fn save_at(path: &std::path::Path, ledger: &Ledger) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(ledger)
        .map_err(|e| format!("could not serialise the consent ledger: {e}"))?;
    if text.len() > MAX_BYTES {
        return Err(format!("the consent ledger would exceed {MAX_BYTES} bytes"));
    }
    // Write beside the target and rename, so a crash mid-write cannot leave a
    // half-file that reads as "nothing is allowed".
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, text)
        .map_err(|e| format!("could not write {}: {e}", temporary.display()))?;
    std::fs::rename(&temporary, path)
        .map_err(|e| format!("could not replace {}: {e}", path.display()))?;
    Ok(())
}

/// The origin of a source URL: `http://host:port`, lowercased, port always
/// present, path and query dropped.
///
/// The port is always written so that `http://localhost/` and
/// `http://localhost:80/` are one origin rather than two prompts for the same
/// service.
pub fn origin_of(url: &str) -> Result<String, String> {
    let trimmed = url.trim();
    let lower = trimmed.to_ascii_lowercase();
    let rest = lower
        .strip_prefix("http://")
        .ok_or_else(|| "a source url must start with http://".to_string())?;
    let authority = rest.split(['/', '?']).next().unwrap_or("");
    if authority.is_empty() {
        return Err("url has no host".into());
    }
    if authority.contains('@') {
        return Err("url must not embed credentials".into());
    }

    let (host, port) = if let Some(stripped) = authority.strip_prefix('[') {
        let end = stripped
            .find(']')
            .ok_or_else(|| "url has an unterminated IPv6 host".to_string())?;
        let host = &stripped[..end];
        let after = &stripped[end + 1..];
        let port = match after.strip_prefix(':') {
            Some(port) => port
                .parse::<u16>()
                .map_err(|_| "url has an invalid port".to_string())?,
            None if after.is_empty() => 80,
            None => return Err("url has trailing characters after the host".into()),
        };
        (format!("[{host}]"), port)
    } else {
        match authority.rsplit_once(':') {
            Some((host, port)) => (
                host.to_string(),
                port.parse::<u16>()
                    .map_err(|_| "url has an invalid port".to_string())?,
            ),
            None => (authority.to_string(), 80),
        }
    };

    if host.is_empty() {
        return Err("url has no host".into());
    }

    Ok(format!("http://{host}:{port}"))
}

/// Validate a string that is already an origin rather than a URL.
///
/// Used on load, so a hand-edited key cannot smuggle in a host the app would
/// never have written.
fn origin_of_origin(origin: &str) -> Option<String> {
    let normalized = origin_of(origin).ok()?;
    // An origin has no path. `origin_of` drops one, so a key that carried a path
    // would silently become a different key; refuse it instead.
    let rest = origin.trim().to_ascii_lowercase();
    let rest = rest.strip_prefix("http://")?;
    if rest.contains('/') || rest.contains('?') {
        return None;
    }
    let host = normalized
        .strip_prefix("http://")?
        .rsplit_once(':')?
        .0
        .to_string();
    if !matches!(host.as_str(), "127.0.0.1" | "[::1]" | "localhost") {
        return None;
    }
    Some(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vibedesk-consent-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // ---- origins ---------------------------------------------------------

    #[test]
    fn an_origin_drops_the_path_and_query() {
        assert_eq!(
            origin_of("http://127.0.0.1:8787/status/overview?since=1").unwrap(),
            "http://127.0.0.1:8787"
        );
    }

    #[test]
    fn the_default_port_is_written_out() {
        assert_eq!(origin_of("http://localhost/").unwrap(), "http://localhost:80");
        assert_eq!(
            origin_of("http://localhost:80/x").unwrap(),
            "http://localhost:80"
        );
    }

    #[test]
    fn the_host_is_lowercased() {
        assert_eq!(
            origin_of("http://LOCALHOST:3000/x").unwrap(),
            "http://localhost:3000"
        );
    }

    #[test]
    fn an_ipv6_host_keeps_its_brackets() {
        assert_eq!(origin_of("http://[::1]:8787/x").unwrap(), "http://[::1]:8787");
        assert_eq!(origin_of("http://[::1]/x").unwrap(), "http://[::1]:80");
    }

    #[test]
    fn two_paths_on_one_service_are_one_origin() {
        assert_eq!(
            origin_of("http://127.0.0.1:8787/a").unwrap(),
            origin_of("http://127.0.0.1:8787/b").unwrap()
        );
    }

    #[test]
    fn two_ports_are_two_origins() {
        assert_ne!(
            origin_of("http://127.0.0.1:8787/a").unwrap(),
            origin_of("http://127.0.0.1:5432/a").unwrap()
        );
    }

    #[test]
    fn https_is_refused() {
        assert!(origin_of("https://127.0.0.1:8787/x").is_err());
    }

    #[test]
    fn a_bad_port_is_refused() {
        assert!(origin_of("http://127.0.0.1:notaport/x").is_err());
        assert!(origin_of("http://127.0.0.1:/x").is_err());
        assert!(origin_of("http://127.0.0.1:99999/x").is_err());
    }

    // ---- the ledger ------------------------------------------------------

    #[test]
    fn a_missing_ledger_allows_nothing() {
        let dir = temp_dir("missing");
        let ledger = load_at(&dir.join("consent.json"));
        assert!(!ledger.allows("http://127.0.0.1:8787"));
        assert_eq!(ledger.decision("http://127.0.0.1:8787"), None);
    }

    #[test]
    fn a_corrupt_ledger_allows_nothing() {
        let dir = temp_dir("corrupt");
        let path = dir.join("consent.json");
        std::fs::write(&path, "{ this is not json").unwrap();
        let ledger = load_at(&path);
        assert!(!ledger.allows("http://127.0.0.1:8787"));
    }

    #[test]
    fn an_allow_survives_a_reload() {
        let dir = temp_dir("allow");
        let path = dir.join("consent.json");
        decide_at(&path, "http://127.0.0.1:8787", Some(Decision::Allow)).unwrap();
        assert!(load_at(&path).allows("http://127.0.0.1:8787"));
    }

    #[test]
    fn a_deny_is_sticky_and_is_not_an_allow() {
        let dir = temp_dir("deny");
        let path = dir.join("consent.json");
        decide_at(&path, "http://127.0.0.1:8787", Some(Decision::Deny)).unwrap();
        let ledger = load_at(&path);
        assert_eq!(
            ledger.decision("http://127.0.0.1:8787"),
            Some(Decision::Deny)
        );
        assert!(!ledger.allows("http://127.0.0.1:8787"));
    }

    #[test]
    fn clearing_a_decision_puts_the_origin_back_to_asking() {
        let dir = temp_dir("clear");
        let path = dir.join("consent.json");
        decide_at(&path, "http://127.0.0.1:8787", Some(Decision::Allow)).unwrap();
        decide_at(&path, "http://127.0.0.1:8787", None).unwrap();
        assert_eq!(load_at(&path).decision("http://127.0.0.1:8787"), None);
    }

    #[test]
    fn allowing_one_port_does_not_allow_another() {
        let dir = temp_dir("ports");
        let path = dir.join("consent.json");
        decide_at(&path, "http://127.0.0.1:8787", Some(Decision::Allow)).unwrap();
        let ledger = load_at(&path);
        assert!(ledger.allows("http://127.0.0.1:8787"));
        assert!(!ledger.allows("http://127.0.0.1:5432"));
    }

    #[test]
    fn a_hand_edited_non_loopback_key_is_dropped() {
        let dir = temp_dir("smuggle");
        let path = dir.join("consent.json");
        std::fs::write(
            &path,
            r#"{"version":1,"origins":{"http://example.com:80":{"decision":"allow","at":"x"}}}"#,
        )
        .unwrap();
        let ledger = load_at(&path);
        assert!(ledger.origins.is_empty(), "a non-loopback key is not honoured");
    }

    #[test]
    fn a_hand_edited_key_with_a_path_is_dropped() {
        let dir = temp_dir("pathkey");
        let path = dir.join("consent.json");
        std::fs::write(
            &path,
            r#"{"version":1,"origins":{"http://127.0.0.1:8787/admin":{"decision":"allow","at":"x"}}}"#,
        )
        .unwrap();
        assert!(load_at(&path).origins.is_empty());
    }

    #[test]
    fn deciding_on_a_non_loopback_origin_is_refused() {
        let dir = temp_dir("refuse");
        let path = dir.join("consent.json");
        assert!(decide_at(&path, "http://example.com:80", Some(Decision::Allow)).is_err());
        assert!(!path.exists(), "a refused decision writes nothing");
    }

    #[test]
    fn the_ledger_has_a_ceiling() {
        let dir = temp_dir("ceiling");
        let path = dir.join("consent.json");
        for port in 0..MAX_ORIGINS {
            decide_at(
                &path,
                &format!("http://127.0.0.1:{}", 10_000 + port),
                Some(Decision::Allow),
            )
            .unwrap();
        }
        let error = decide_at(&path, "http://127.0.0.1:9999", Some(Decision::Allow)).unwrap_err();
        assert!(error.contains("already holds"), "{error}");
        // Re-deciding an origin already in the ledger still works.
        decide_at(&path, "http://127.0.0.1:10000", Some(Decision::Deny)).unwrap();
    }

    #[test]
    fn an_oversized_ledger_is_ignored() {
        let dir = temp_dir("oversize");
        let path = dir.join("consent.json");
        let padding = "x".repeat(MAX_BYTES + 1);
        std::fs::write(&path, padding).unwrap();
        assert!(load_at(&path).origins.is_empty());
    }

    #[test]
    fn a_decision_round_trips_through_json() {
        let dir = temp_dir("roundtrip");
        let path = dir.join("consent.json");
        decide_at(&path, "http://127.0.0.1:8787", Some(Decision::Allow)).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"allow\""), "{text}");
        assert!(text.contains("http://127.0.0.1:8787"), "{text}");
        assert!(!text.contains(".tmp"), "the temporary file is renamed away");
    }
}
