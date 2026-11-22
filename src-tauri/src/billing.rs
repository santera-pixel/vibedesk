//! The account and plan the agent reports, shaped for the billing window.
//!
//! `_account/read` is an ACP extension, not part of the published protocol, so
//! its payload is treated as untrusted: every field is optional and a missing
//! one renders as "unknown" rather than failing the whole panel. The agent is
//! the only source of plan and budget data — VibeDesk never computes a
//! percentage of its own, because only the server knows the allowance.

use serde::Serialize;
use serde_json::Value;

/// The subscription half: what plan is active and how much of it is spent.
#[derive(Debug, Default, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PlanSnapshot {
    /// `ready`, `exhausted`, `unauthenticated`, …
    pub status: Option<String>,
    /// `Pro`, `Free`, …
    pub title: Option<String>,
    /// `INDIVIDUAL`, `TEAM`, …
    pub name: Option<String>,
    /// `chat`, `code`, …
    pub kind: Option<String>,
    /// Percentage of the allowance already spent, 0–100.
    pub usage_percentage: Option<f64>,
    /// When the allowance resets, as the server's ISO timestamp.
    pub reset_at: Option<String>,
    /// Whether pay-as-you-go is on once the allowance runs out.
    pub payg_enabled: bool,
    /// A masked key such as `****U4Ut`. Never the key itself.
    pub key_preview: Option<String>,
    /// `vibe`, `api`, …
    pub key_scope: Option<String>,
    /// `personal_and_shared`, `personal`, …
    pub key_access: Option<String>,
}

/// The identity half: who is signed in and where.
#[derive(Debug, Default, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct IdentitySnapshot {
    pub email: Option<String>,
    pub name: Option<String>,
    pub workspace: Option<String>,
    pub organization: Option<String>,
    /// `os_keyring`, `env`, …
    pub auth_state: Option<String>,
    pub authenticated: bool,
    pub sign_out_available: bool,
}

#[derive(Debug, Default, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AccountSnapshot {
    pub plan: PlanSnapshot,
    pub identity: IdentitySnapshot,
}

fn text(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn number(value: &Value, key: &str) -> Option<f64> {
    value.get(key).and_then(Value::as_f64)
}

fn flag(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// Read `_account/read`. A null or malformed payload yields an empty snapshot,
/// which the panel renders as "not reported" instead of an error.
pub fn parse_account(account: &Value) -> PlanSnapshot {
    let plan = account.get("plan").unwrap_or(&Value::Null);
    let budget = account.get("budget").unwrap_or(&Value::Null);
    let key = account.get("apiKey").unwrap_or(&Value::Null);

    PlanSnapshot {
        status: text(account, "status"),
        title: text(plan, "title"),
        name: text(plan, "name"),
        kind: text(plan, "kind"),
        // Clamp: a server that reports 140% must not draw a bar past its track.
        usage_percentage: number(budget, "usagePercentage").map(|p| p.clamp(0.0, 100.0)),
        reset_at: text(budget, "resetAt"),
        payg_enabled: flag(budget, "paygEnabled"),
        key_preview: text(key, "preview"),
        key_scope: text(key, "scope"),
        key_access: text(key, "accessScope"),
    }
}

/// Read `_identity/read` and `_auth/status` into one snapshot.
pub fn parse_identity(identity: &Value, auth: &Value) -> IdentitySnapshot {
    let first = text(identity, "firstName").unwrap_or_default();
    let last = text(identity, "lastName").unwrap_or_default();
    let name = format!("{first} {last}").trim().to_owned();

    IdentitySnapshot {
        email: text(identity, "email"),
        name: if name.is_empty() { None } else { Some(name) },
        workspace: identity
            .get("workspace")
            .and_then(|w| text(w, "name")),
        organization: identity
            .get("organization")
            .and_then(|o| text(o, "name")),
        auth_state: text(auth, "authState"),
        authenticated: flag(auth, "authenticated"),
        sign_out_available: flag(auth, "signOutAvailable"),
    }
}

pub fn parse(account: &Value, identity: &Value, auth: &Value) -> AccountSnapshot {
    AccountSnapshot {
        plan: parse_account(account),
        identity: parse_identity(identity, auth),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The exact payload `vibe-acp` 3.1.2 returned when probed.
    fn live_account() -> Value {
        json!({
            "status": "ready",
            "plan": { "kind": "chat", "name": "INDIVIDUAL", "title": "Pro" },
            "planOffer": null,
            "rateLimitAction": null,
            "apiKey": {
                "preview": "****U4Ut",
                "scope": "vibe",
                "accessScope": "personal_and_shared"
            },
            "budget": {
                "usagePercentage": 8.067122688666666,
                "resetAt": "2026-12-01T00:00:00Z",
                "paygEnabled": false
            }
        })
    }

    #[test]
    fn the_live_payload_parses_into_a_plan() {
        let plan = parse_account(&live_account());
        assert_eq!(plan.status.as_deref(), Some("ready"));
        assert_eq!(plan.title.as_deref(), Some("Pro"));
        assert_eq!(plan.name.as_deref(), Some("INDIVIDUAL"));
        assert_eq!(plan.kind.as_deref(), Some("chat"));
        assert_eq!(plan.reset_at.as_deref(), Some("2026-12-01T00:00:00Z"));
        assert!(!plan.payg_enabled);
        assert_eq!(plan.key_preview.as_deref(), Some("****U4Ut"));
        assert_eq!(plan.key_scope.as_deref(), Some("vibe"));
        assert_eq!(plan.key_access.as_deref(), Some("personal_and_shared"));
        let pct = plan.usage_percentage.unwrap();
        assert!((pct - 8.067).abs() < 0.01, "got {pct}");
    }

    #[test]
    fn a_percentage_past_the_allowance_is_clamped_to_the_track() {
        let account = json!({ "budget": { "usagePercentage": 140.0 } });
        assert_eq!(parse_account(&account).usage_percentage, Some(100.0));
        let negative = json!({ "budget": { "usagePercentage": -3.0 } });
        assert_eq!(parse_account(&negative).usage_percentage, Some(0.0));
    }

    #[test]
    fn a_missing_budget_reports_nothing_rather_than_zero() {
        // Zero would draw an empty bar and read as "nothing spent", which is a
        // different claim from "the server did not say".
        let plan = parse_account(&json!({ "status": "ready" }));
        assert_eq!(plan.usage_percentage, None);
        assert_eq!(plan.reset_at, None);
        assert!(!plan.payg_enabled);
    }

    #[test]
    fn a_null_payload_is_an_empty_snapshot() {
        let plan = parse_account(&Value::Null);
        assert_eq!(plan, PlanSnapshot::default());
        let identity = parse_identity(&Value::Null, &Value::Null);
        assert_eq!(identity, IdentitySnapshot::default());
    }

    #[test]
    fn a_wrongly_typed_field_is_ignored_not_fatal() {
        let account = json!({
            "status": 42,
            "plan": "Pro",
            "budget": { "usagePercentage": "8" },
            "apiKey": []
        });
        let plan = parse_account(&account);
        assert_eq!(plan.status, None);
        assert_eq!(plan.title, None);
        assert_eq!(plan.usage_percentage, None);
        assert_eq!(plan.key_preview, None);
    }

    #[test]
    fn identity_joins_the_name_and_reads_the_workspace() {
        let identity = json!({
            "id": "6e87ee9c",
            "email": "santera12@gmail.com",
            "firstName": "santera",
            "lastName": "1r1",
            "workspace": { "id": "b34d", "name": "Default Workspace" },
            "organization": { "id": "581e", "name": "santera12" }
        });
        let auth = json!({
            "authenticated": true,
            "authState": "os_keyring",
            "signOutAvailable": true
        });
        let snap = parse_identity(&identity, &auth);
        assert_eq!(snap.email.as_deref(), Some("santera12@gmail.com"));
        assert_eq!(snap.name.as_deref(), Some("santera 1r1"));
        assert_eq!(snap.workspace.as_deref(), Some("Default Workspace"));
        assert_eq!(snap.organization.as_deref(), Some("santera12"));
        assert_eq!(snap.auth_state.as_deref(), Some("os_keyring"));
        assert!(snap.authenticated);
        assert!(snap.sign_out_available);
    }

    #[test]
    fn a_missing_last_name_does_not_leave_a_trailing_space() {
        let identity = json!({ "firstName": "santera" });
        assert_eq!(parse_identity(&identity, &Value::Null).name.as_deref(), Some("santera"));
        let blank = json!({ "firstName": "  ", "lastName": "" });
        assert_eq!(parse_identity(&blank, &Value::Null).name, None);
    }

    #[test]
    fn the_snapshot_serialises_to_camel_case_for_the_webview() {
        let snap = parse(&live_account(), &Value::Null, &Value::Null);
        let body = serde_json::to_string(&snap).unwrap();
        assert!(body.contains("\"usagePercentage\""));
        assert!(body.contains("\"resetAt\""));
        assert!(body.contains("\"paygEnabled\""));
        assert!(body.contains("\"keyPreview\""));
        assert!(!body.contains("usage_percentage"));
    }
}
