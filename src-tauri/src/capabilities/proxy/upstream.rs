//! Where the proxy forwards to.
//!
//! The proxy stands in front of exactly one provider: the one the user was
//! already using. It does not hold a key and it does not rewrite auth — the
//! client sends the same `Authorization` header it would have sent upstream, and
//! the proxy relays it. That is the whole reason the proxy can be honest about
//! being transparent: it never has to know a secret.

use crate::config;
use crate::error::{AppError, AppResult};

/// The provider the proxy forwards to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upstream {
    pub provider: String,
    pub base: String,
}

/// The base URL of a provider the CLI ships with.
///
/// A provider with no `[[providers]]` entry is a built-in, and its base URL is
/// not in any file the app can read. Only the one the CLI documents is listed
/// here; anything else has to be written down by the user, and the error says so
/// rather than guessing an endpoint and sending a key to it.
fn builtin_base(provider: &str) -> Option<&'static str> {
    match provider {
        "mistral" => Some("https://api.mistral.ai/v1"),
        _ => None,
    }
}

/// A provider's base URL, from `config.toml` or from the built-in table.
pub fn base_for(provider: &str) -> AppResult<String> {
    if provider.trim().is_empty() {
        return Err(AppError::message(
            "no upstream provider is recorded, so the proxy has nowhere to forward to",
        ));
    }
    let path = config::user_config_path();
    let raw = std::fs::read_to_string(&path).unwrap_or_default();
    let document: toml::Value = if raw.trim().is_empty() {
        toml::Value::Table(toml::map::Map::new())
    } else {
        toml::from_str(&raw).map_err(|error| AppError::message(format!("config.toml: {error}")))?
    };

    if let Some(providers) = document.get("providers").and_then(toml::Value::as_array) {
        for entry in providers {
            let name = entry
                .get("name")
                .and_then(toml::Value::as_str)
                .unwrap_or_default();
            if name != provider {
                continue;
            }
            let base = entry
                .get("api_base")
                .and_then(toml::Value::as_str)
                .unwrap_or_default()
                .to_string();
            if base.trim().is_empty() {
                return Err(AppError::message(format!(
                    "provider {provider} has no api_base, so the proxy cannot forward to it"
                )));
            }
            return Ok(base);
        }
    }

    if let Some(base) = builtin_base(provider) {
        return Ok(base.to_string());
    }

    Err(AppError::message(format!(
        "provider {provider} is built into the CLI and its api_base is not in config.toml.          Add a [[providers]] entry for it with its api_base so the proxy can forward to it."
    )))
}

/// Look up a provider's base URL.
pub fn resolve(provider: &str) -> AppResult<Upstream> {
    Ok(Upstream {
        provider: provider.to_string(),
        base: base_for(provider)?,
    })
}

/// Join a base URL and an incoming path without doubling the version segment.
///
/// A provider's `api_base` may or may not carry `/v1`, and the client always
/// sends it. Getting this wrong produces `/v1/v1/chat/completions`, which every
/// provider answers with a 404.
pub fn join_url(base: &str, path: &str) -> String {
    let base = base.trim_end_matches('/');
    let path = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    };
    let base = if path.starts_with("/v1") {
        base.strip_suffix("/v1").unwrap_or(base)
    } else {
        base
    };
    format!("{base}{path}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_base_that_already_carries_the_version_is_not_doubled() {
        assert_eq!(
            join_url("https://api.mistral.ai/v1", "/v1/chat/completions"),
            "https://api.mistral.ai/v1/chat/completions"
        );
    }

    #[test]
    fn a_base_without_the_version_gets_it_from_the_path() {
        assert_eq!(
            join_url("https://api.anthropic.com", "/v1/messages"),
            "https://api.anthropic.com/v1/messages"
        );
    }

    #[test]
    fn a_trailing_slash_on_the_base_does_not_produce_a_double_slash() {
        assert_eq!(
            join_url("http://127.0.0.1:8080/v1/", "/v1/models"),
            "http://127.0.0.1:8080/v1/models"
        );
    }

    #[test]
    fn a_path_that_is_not_versioned_is_appended_as_it_is() {
        assert_eq!(
            join_url("https://example.test/v1", "/health"),
            "https://example.test/v1/health"
        );
    }

    #[test]
    fn an_empty_provider_name_is_refused_with_a_reason() {
        let error = resolve("").unwrap_err();
        assert!(error.to_string().contains("nowhere to forward"));
    }

    #[test]
    fn the_documented_builtin_provider_has_a_base_url() {
        assert_eq!(
            builtin_base("mistral"),
            Some("https://api.mistral.ai/v1")
        );
    }

    #[test]
    fn an_unknown_builtin_is_refused_rather_than_guessed() {
        assert_eq!(builtin_base("someones-local-llama"), None);
    }
}
