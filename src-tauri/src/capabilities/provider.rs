//! Registering the proxy as a provider the agent can talk to.
//!
//! The agent reads `config.toml`, so the only way to put the proxy on the model
//! request path is to give the agent a provider that points at it. This module
//! writes that provider, and the models that use it, and nothing else.
//!
//! Three rules make the write safe:
//!
//! * **Additive.** The user's own providers and models are never edited. The
//!   proxy gets its own `vibedesk` provider and its own `vibedesk/<alias>`
//!   models, so a bug here cannot corrupt a working configuration.
//! * **Reversible.** Disabling removes exactly what enabling added and restores
//!   `active_model` to the alias it was on. A `.bak` copy is written first,
//!   because the agent owns this file.
//! * **`preserve_api_base = true`.** Without it the CLI adopts the
//!   whoami-advertised `api_base` on sign-in and `/reload`, which would silently
//!   point the provider back at the real API and take the proxy off the path.
//!
//! The mirrored model also sets `auto_compact_threshold` as high as the CLI
//! allows. That is what actually *replaces* the CLI's compaction rather than
//! racing it: the CLI's summariser never fires, so the kernel is the only thing
//! folding the conversation. "As high as the CLI allows" is not the window — the
//! CLI reserves a summary headroom and refuses a threshold that eats it.

use serde_json::json;

use crate::config;
use crate::error::{AppError, AppResult};

use super::proxy::upstream;

/// The provider name the proxy registers under.
pub const PROXY_PROVIDER: &str = "vibedesk";

/// The prefix on every mirrored model alias.
pub const PROXY_MODEL_PREFIX: &str = "vibedesk/";

/// Appended to a mirrored model's `display_name`.
///
/// The agent lists models by display name, so without this the mirrored model
/// and the one it copies are two identical rows in the picker. The user has to
/// be able to see which one goes through the proxy, because that is the whole
/// difference between them.
pub const MIRRORED_SUFFIX: &str = " · via VibeDesk";

/// What enabling added, so disabling can take exactly that away.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Registration {
    /// The provider the proxy stands in front of.
    pub upstream_provider: String,
    /// The aliases that were mirrored.
    pub models: Vec<String>,
    /// The alias `active_model` held before the switch.
    pub previous_model: Option<String>,
}

/// Whether the proxy can be registered at all, and why not when it cannot.
///
/// `enable` mirrors the models in `[[models]]` that use the upstream provider,
/// because a mirrored model is a copy of a real entry with its `provider`
/// rewritten. A server-routed active model — the CLI's built-in Mistral Medium
/// 3.5, which is what a fresh `config.toml` runs on — has no `[[models]]` entry
/// to copy, so there is nothing to mirror and nothing for the proxy to serve.
///
/// That is a precondition rather than a failure: retrying changes nothing until
/// the user pins a model in `[[models]]`. Reporting it as `failed` would offer a
/// retry button that cannot work, which is the shape of a lie this repo does not
/// ship.
///
/// The real fix is to take the canonical model name from the agent's own
/// `model` config option — its `description` is the API name — and synthesise
/// the entry. That needs a live session, so it belongs where the catalog is
/// already read, not here.
pub fn mirrorable() -> Result<(), String> {
    let document = read_document().map_err(|error| error.to_string())?;
    let active = document
        .get("active_model")
        .and_then(toml::Value::as_str)
        .unwrap_or_default();
    let base = active.strip_prefix(PROXY_MODEL_PREFIX).unwrap_or(active);

    let upstream = active_provider(&document).ok_or_else(|| {
        format!(
            "the active model `{base}` is server-routed, so it has no [[models]] entry to mirror and the proxy has nothing to stand in front of. Pin a model in [[models]] to route it through the proxy."
        )
    })?;

    let mirrorable = models(&document).into_iter().any(|entry| {
        entry.get("provider").and_then(toml::Value::as_str) == Some(upstream.as_str())
    });
    if !mirrorable {
        return Err(format!(
            "no [[models]] entry uses provider `{upstream}`, so the proxy has nothing to serve"
        ));
    }
    Ok(())
}

/// Whether the proxy provider is currently in `config.toml`.
pub fn is_registered() -> bool {
    read_document()
        .ok()
        .map(|document| {
            providers(&document)
                .iter()
                .any(|entry| name_of(entry) == PROXY_PROVIDER)
        })
        .unwrap_or(false)
}

/// Add the proxy provider and mirror the active model onto it.
pub fn enable(port: u16) -> AppResult<Registration> {
    let mut document = read_document()?;
    let previous_model = document
        .get("active_model")
        .and_then(toml::Value::as_str)
        .map(str::to_string);

    // A re-enable starts from a config that already points at the proxy, and
    // this runs on every launch while compaction is on. Put `active_model` back
    // on the alias it mirrors before anything reads it: otherwise the lookup
    // below resolves `vibedesk/deepseek` to the `vibedesk` provider and the
    // proxy ends up standing in front of itself.
    let base_alias = previous_model
        .as_deref()
        .map(|alias| alias.strip_prefix(PROXY_MODEL_PREFIX).unwrap_or(alias).to_string());
    if let Some(alias) = base_alias.as_deref() {
        set_active_model(&mut document, alias);
    }

    let upstream_provider = active_provider(&document).ok_or_else(|| {
        AppError::message(
            "no active model is configured, so there is nothing for the proxy to stand in front of",
        )
    })?;
    let base = upstream::base_for(&upstream_provider)?;

    // Idempotent: a second enable replaces the first rather than stacking.
    strip(&mut document);

    let upstream_entry = providers(&document)
        .into_iter()
        .find(|entry| name_of(entry) == upstream_provider);
    let (api_key_env_var, backend) = inherited(upstream_entry.as_ref(), &upstream_provider);
    let api_style = upstream_entry
        .as_ref()
        .and_then(|entry| entry.get("api_style"))
        .and_then(toml::Value::as_str)
        .unwrap_or("openai")
        .to_string();

    let mut provider = toml::map::Map::new();
    provider.insert("name".into(), toml::Value::String(PROXY_PROVIDER.into()));
    provider.insert(
        "api_base".into(),
        toml::Value::String(format!("http://127.0.0.1:{port}/v1")),
    );
    provider.insert(
        "api_key_env_var".into(),
        toml::Value::String(api_key_env_var),
    );
    provider.insert("backend".into(), toml::Value::String(backend));
    provider.insert("api_style".into(), toml::Value::String(api_style));
    // Without this the CLI re-homes `api_base` onto the real API on sign-in.
    provider.insert("preserve_api_base".into(), toml::Value::Boolean(true));
    push(&mut document, "providers", toml::Value::Table(provider));

    let mut mirrored = Vec::new();
    let originals: Vec<toml::Value> = models(&document)
        .into_iter()
        .filter(|entry| {
            entry.get("provider").and_then(toml::Value::as_str) == Some(upstream_provider.as_str())
        })
        .collect();

    for original in originals {
        let Some((alias, copy)) = mirror_model(&original) else {
            continue;
        };
        mirrored.push(alias);
        push(&mut document, "models", copy);
    }

    if mirrored.is_empty() {
        return Err(AppError::message(format!(
            "provider {upstream_provider} has no models to mirror, so the proxy has nothing to serve"
        )));
    }

    if let Some(active) = mirrored_active(base_alias.as_deref(), &mirrored) {
        set_active_model(&mut document, &active);
    }

    write_document(&document)?;
    let _ = base;
    Ok(Registration {
        upstream_provider,
        models: mirrored,
        previous_model: base_alias,
    })
}

/// Remove the proxy provider and everything it added.
pub fn disable() -> AppResult<()> {
    let mut document = read_document()?;
    strip(&mut document);

    // Put the user back on the model they were on, not on a mirrored alias that
    // no longer exists.
    if let Some(active) = document
        .get("active_model")
        .and_then(toml::Value::as_str)
        .map(str::to_string)
    {
        if let Some(rest) = active.strip_prefix(PROXY_MODEL_PREFIX) {
            set_active_model(&mut document, rest);
        }
    }

    write_document(&document)
}

/// The credential env var and backend a mirrored provider inherits.
///
/// A provider the CLI ships with has no entry in `config.toml`, so there is
/// nothing to copy from. The CLI's own convention fills the gap: the credential
/// is stored under `<PROVIDER>_API_KEY` and the backend is the provider's name.
///
/// The env var name is load-bearing. The proxy deliberately holds no key — it
/// forwards the `Authorization` header the client sends — so a mirrored provider
/// that names no env var makes the client send no header at all, and every
/// request comes back "Invalid API Key".
fn inherited(entry: Option<&toml::Value>, provider: &str) -> (String, String) {
    let env_var = entry
        .and_then(|entry| entry.get("api_key_env_var"))
        .and_then(toml::Value::as_str)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("{}_API_KEY", provider.to_uppercase()));
    let backend = entry
        .and_then(|entry| entry.get("backend"))
        .and_then(toml::Value::as_str)
        .unwrap_or(provider)
        .to_string();
    (env_var, backend)
}

/// One model, rewritten to point at the proxy.
///
/// Returns the mirrored alias and the rewritten entry, or `None` for an entry
/// that cannot be mirrored because it has no alias or is not a table.
fn mirror_model(original: &toml::Value) -> Option<(String, toml::Value)> {
    let alias = original.get("alias").and_then(toml::Value::as_str)?;
    let mut copy = original.clone();
    let table = copy.as_table_mut()?;
    table.insert(
        "provider".into(),
        toml::Value::String(PROXY_PROVIDER.into()),
    );
    table.insert(
        "alias".into(),
        toml::Value::String(format!("{PROXY_MODEL_PREFIX}{alias}")),
    );
    if let Some(name) = table.get("name").and_then(toml::Value::as_str) {
        let name = format!("{PROXY_MODEL_PREFIX}{name}");
        table.insert("name".into(), toml::Value::String(name));
    }
    // The picker lists models by `display_name`, so a verbatim copy puts two
    // identical rows in front of the user and no way to tell which one is
    // routed through the proxy. The suffix is the only thing that makes the
    // choice answerable.
    if let Some(display) = table.get("display_name").and_then(toml::Value::as_str) {
        let display = format!("{display}{MIRRORED_SUFFIX}");
        table.insert("display_name".into(), toml::Value::String(display));
    }
    // The CLI's own summariser must never fire, or two compactors fight over
    // the same conversation. Pinning the threshold as high as the CLI allows is
    // how the kernel becomes the only one folding.
    if let Some(window) = table
        .get("max_context_length")
        .and_then(toml::Value::as_integer)
    {
        if let Some(threshold) = compaction_threshold(window) {
            table.insert(
                "auto_compact_threshold".into(),
                toml::Value::Integer(threshold),
            );
        }
    }
    Some((format!("{PROXY_MODEL_PREFIX}{alias}"), copy))
}

/// The highest compaction threshold the CLI accepts for a window.
///
/// The CLI does not take the threshold on trust. `threshold_bounds` in
/// `mistralai_vibe_local_harness/vibe/completion_profile/bounds.py` allows
/// `minimum..maximum` and refuses anything outside it; the caller then fits the
/// value into range and warns. Writing `max_context_length` — which is what this
/// did — is always above `maximum` by exactly the summary headroom, so every
/// mirrored model was capped and warned on every session, and the number in the
/// file was never the number in use.
///
/// `maximum` is `window - min(24_000, ceil(window / 4))`: the headroom is the
/// summary request's budget, and the CLI will not let a threshold eat it. That
/// is the value written here, so the file states what the CLI will actually do.
///
/// `None` for a window too small to leave a positive threshold, which leaves the
/// key out and lets the CLI's own default apply rather than writing a value it
/// would reject.
fn compaction_threshold(window: i64) -> Option<i64> {
    /// `SUMMARY_HEADROOM_CAP` in the CLI's `bounds.py`.
    const SUMMARY_HEADROOM_CAP: i64 = 24_000;
    if window <= 0 {
        return None;
    }
    let headroom = (window + 3) / 4;
    let threshold = window - headroom.min(SUMMARY_HEADROOM_CAP);
    (threshold > 0).then_some(threshold)
}

/// The mirrored alias `active_model` should hold after enabling.
///
/// The alias must carry the prefix. The mirrored model is a different model
/// from the one it copies, and pointing `active_model` at the unprefixed alias
/// leaves the agent talking to the real API with the proxy registered but
/// unused. That failure is silent — the panel reads "on", the proxy listens,
/// and no traffic is intercepted — so the prefix is applied here, once, where
/// it can be tested.
fn mirrored_active(previous: Option<&str>, mirrored: &[String]) -> Option<String> {
    previous
        .map(|alias| format!("{PROXY_MODEL_PREFIX}{alias}"))
        .filter(|alias| mirrored.iter().any(|m| m == alias))
        .or_else(|| mirrored.first().cloned())
}

/// The provider the active model belongs to.
fn active_provider(document: &toml::Value) -> Option<String> {
    let active = document.get("active_model").and_then(toml::Value::as_str)?;
    models(document)
        .into_iter()
        .find(|entry| entry.get("alias").and_then(toml::Value::as_str) == Some(active))
        .and_then(|entry| {
            entry
                .get("provider")
                .and_then(toml::Value::as_str)
                .map(str::to_string)
        })
}

fn strip(document: &mut toml::Value) {
    if let Some(list) = document
        .get_mut("providers")
        .and_then(toml::Value::as_array_mut)
    {
        list.retain(|entry| name_of(entry) != PROXY_PROVIDER);
    }
    if let Some(list) = document.get_mut("models").and_then(toml::Value::as_array_mut) {
        list.retain(|entry| {
            entry.get("provider").and_then(toml::Value::as_str) != Some(PROXY_PROVIDER)
        });
    }
}

fn set_active_model(document: &mut toml::Value, alias: &str) {
    if let Some(table) = document.as_table_mut() {
        table.insert(
            "active_model".into(),
            toml::Value::String(alias.to_string()),
        );
    }
}

fn name_of(entry: &toml::Value) -> &str {
    entry
        .get("name")
        .and_then(toml::Value::as_str)
        .unwrap_or_default()
}

fn providers(document: &toml::Value) -> Vec<toml::Value> {
    document
        .get("providers")
        .and_then(toml::Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn models(document: &toml::Value) -> Vec<toml::Value> {
    document
        .get("models")
        .and_then(toml::Value::as_array)
        .cloned()
        .unwrap_or_default()
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

/// The registration as the panel reads it.
///
/// `mirrored` carries the display name as well as the alias because the agent
/// does not announce a new model list when it re-reads `config.toml`: after
/// `/reload` the mirrored model is selectable but the picker still shows the
/// list it captured at session start. The panel merges these entries in so the
/// model the session is on is one the user can see and choose.
pub fn describe() -> serde_json::Value {
    let document = read_document().unwrap_or(toml::Value::Table(toml::map::Map::new()));
    let registered = providers(&document)
        .iter()
        .any(|entry| name_of(entry) == PROXY_PROVIDER);
    let mirrored_entries: Vec<toml::Value> = models(&document)
        .into_iter()
        .filter(|entry| {
            entry.get("provider").and_then(toml::Value::as_str) == Some(PROXY_PROVIDER)
        })
        .collect();
    let mirrored: Vec<String> = mirrored_entries
        .iter()
        .filter_map(|entry| {
            entry
                .get("alias")
                .and_then(toml::Value::as_str)
                .map(str::to_string)
        })
        .collect();
    let described: Vec<serde_json::Value> = mirrored_entries
        .iter()
        .filter_map(|entry| {
            let alias = entry.get("alias").and_then(toml::Value::as_str)?;
            Some(json!({
                "alias": alias,
                "displayName": entry.get("display_name").and_then(toml::Value::as_str),
            }))
        })
        .collect();
    json!({
        "registered": registered,
        "provider": PROXY_PROVIDER,
        "models": mirrored,
        "mirrored": described,
        "activeModel": document.get("active_model").and_then(toml::Value::as_str),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> toml::Value {
        toml::from_str(
            r#"
active_model = "deepseek"

[[providers]]
name = "mistral"
api_base = "https://api.mistral.ai/v1"
api_key_env_var = "MISTRAL_API_KEY"
backend = "mistral"

[[models]]
name = "deepseek-v4"
provider = "mistral"
alias = "deepseek"
display_name = "DeepSeek V4"
max_context_length = 1000000
"#,
        )
        .unwrap()
    }

    #[test]
    fn the_active_model_names_the_provider_to_stand_in_front_of() {
        assert_eq!(active_provider(&sample()).as_deref(), Some("mistral"));
    }

    #[test]
    fn stripping_removes_only_what_the_proxy_added() {
        let mut document = sample();
        push(
            &mut document,
            "providers",
            toml::from_str(
                r#"
name = "vibedesk"
api_base = "http://127.0.0.1:9/v1"
"#,
            )
            .unwrap(),
        );
        push(
            &mut document,
            "models",
            toml::from_str(
                r#"
name = "vibedesk/deepseek-v4"
provider = "vibedesk"
alias = "vibedesk/deepseek"
"#,
            )
            .unwrap(),
        );

        strip(&mut document);

        assert_eq!(providers(&document).len(), 1);
        assert_eq!(name_of(&providers(&document)[0]), "mistral");
        assert_eq!(models(&document).len(), 1);
        assert_eq!(
            models(&document)[0].get("alias").and_then(toml::Value::as_str),
            Some("deepseek")
        );
    }

    #[test]
    fn the_mirrored_alias_is_the_one_disable_strips_back_off() {
        let mut document = sample();
        set_active_model(&mut document, "vibedesk/deepseek");
        let active = document
            .get("active_model")
            .and_then(toml::Value::as_str)
            .unwrap()
            .to_string();
        let rest = active.strip_prefix(PROXY_MODEL_PREFIX).unwrap();
        set_active_model(&mut document, rest);
        assert_eq!(
            document.get("active_model").and_then(toml::Value::as_str),
            Some("deepseek")
        );
    }

    #[test]
    fn a_mirrored_model_is_distinguishable_from_the_one_it_copies() {
        // The picker lists models by display name. A verbatim copy gave the
        // user two identical rows and no way to pick the routed one.
        let original = models(&sample()).remove(0);
        let (alias, copy) = mirror_model(&original).unwrap();

        assert_eq!(alias, "vibedesk/deepseek");
        assert_eq!(
            copy.get("alias").and_then(toml::Value::as_str),
            Some("vibedesk/deepseek")
        );
        assert_eq!(
            copy.get("provider").and_then(toml::Value::as_str),
            Some("vibedesk")
        );
        let display = copy
            .get("display_name")
            .and_then(toml::Value::as_str)
            .unwrap();
        assert!(display.ends_with(MIRRORED_SUFFIX), "{display}");
        assert_ne!(
            Some(display),
            original.get("display_name").and_then(toml::Value::as_str)
        );
    }

    #[test]
    fn a_mirrored_model_pins_the_compaction_threshold_as_high_as_the_cli_allows() {
        // Otherwise the CLI's summariser and the kernel both fold the same
        // conversation. Not the window itself: the CLI reserves a summary
        // headroom and caps anything above `window - headroom`, so writing the
        // window meant the file never stated the value in use.
        let original = models(&sample()).remove(0);
        let (_, copy) = mirror_model(&original).unwrap();
        assert_eq!(
            copy.get("auto_compact_threshold")
                .and_then(toml::Value::as_integer),
            Some(976_000)
        );
    }

    #[test]
    fn the_threshold_written_is_the_one_the_cli_would_accept() {
        // The CLI's own numbers, from `threshold_bounds` in
        // `completion_profile/bounds.py`: `maximum = window - min(24_000,
        // ceil(window / 4))`. A value above it is refused and fitted, with a
        // warning, so the file would be lying.
        assert_eq!(compaction_threshold(1_048_576), Some(1_024_576));
        assert_eq!(compaction_threshold(1_000_000), Some(976_000));
        assert_eq!(compaction_threshold(262_144), Some(238_144));
        assert_eq!(compaction_threshold(200_000), Some(176_000));
        // Below 96_000 the quarter share is under the cap and the headroom
        // shrinks with the window.
        assert_eq!(compaction_threshold(80_000), Some(60_000));
        assert_eq!(compaction_threshold(0), None);
        assert_eq!(compaction_threshold(-1), None);
    }

    #[test]
    fn a_model_with_no_window_is_left_to_the_clis_own_threshold() {
        let entry = toml::from_str(
            "name = \"n\"\nprovider = \"mistral\"\nalias = \"a\"\n",
        )
        .unwrap();
        let (_, copy) = mirror_model(&entry).unwrap();
        assert!(copy.get("auto_compact_threshold").is_none());
    }

    #[test]
    fn a_model_with_no_alias_cannot_be_mirrored() {
        let entry = toml::from_str("name = \"nameless\"\nprovider = \"mistral\"\n").unwrap();
        assert!(mirror_model(&entry).is_none());
    }

    #[test]
    fn a_built_in_provider_lends_its_credential_convention() {
        // The bug this pins: with no entry to copy from, the mirrored provider
        // named no env var, so the client sent no `Authorization` header and
        // upstream answered "Invalid API Key" on every request.
        let (env_var, backend) = inherited(None, "mistral");
        assert_eq!(env_var, "MISTRAL_API_KEY");
        assert_eq!(backend, "mistral");
    }

    #[test]
    fn an_explicit_provider_lends_its_own_credential() {
        let entry = providers(&sample()).remove(0);
        let (env_var, backend) = inherited(Some(&entry), "mistral");
        assert_eq!(env_var, "MISTRAL_API_KEY");
        assert_eq!(backend, "mistral");
    }

    #[test]
    fn an_empty_env_var_falls_back_to_the_convention() {
        // `api_key_env_var = ""` is what a keyring-backed provider looks like.
        // Copying it verbatim is what broke the header.
        let entry = toml::from_str("name = \"mistral\"\napi_key_env_var = \"\"\n").unwrap();
        let (env_var, _) = inherited(Some(&entry), "mistral");
        assert_eq!(env_var, "MISTRAL_API_KEY");
    }

    #[test]
    fn a_config_with_no_active_model_has_no_provider_to_wrap() {
        let document = toml::from_str("active_model = \"\"\n").unwrap();
        assert_eq!(active_provider(&document), None);
    }

    #[test]
    fn enabling_points_the_active_model_at_the_mirrored_alias() {
        // The bug this pins: the alias was chosen from `mirrored` but returned
        // unprefixed, so `active_model` stayed on the direct model and the
        // proxy was registered, listening, and never used.
        let mirrored = vec!["vibedesk/deepseek".to_string()];
        assert_eq!(
            mirrored_active(Some("deepseek"), &mirrored).as_deref(),
            Some("vibedesk/deepseek")
        );
    }

    #[test]
    fn enabling_falls_back_to_the_first_mirrored_alias() {
        let mirrored = vec![
            "vibedesk/deepseek".to_string(),
            "vibedesk/other".to_string(),
        ];
        assert_eq!(
            mirrored_active(Some("not-mirrored"), &mirrored).as_deref(),
            Some("vibedesk/deepseek")
        );
        assert_eq!(
            mirrored_active(None, &mirrored).as_deref(),
            Some("vibedesk/deepseek")
        );
    }

    #[test]
    fn a_re_enable_wraps_the_users_provider_not_the_proxy() {
        // `enable` runs on every launch while compaction is on, so it always
        // starts from a config that already points at the proxy. Reading the
        // provider before normalising the alias resolved `vibedesk/deepseek` to
        // the `vibedesk` provider and mirrored the mirror.
        let mut document = sample();
        set_active_model(&mut document, "vibedesk/deepseek");
        push(
            &mut document,
            "providers",
            toml::from_str(
                r#"
name = "vibedesk"
api_base = "http://127.0.0.1:9/v1"
"#,
            )
            .unwrap(),
        );
        push(
            &mut document,
            "models",
            toml::from_str(
                r#"
name = "vibedesk/deepseek-v4"
provider = "vibedesk"
alias = "vibedesk/deepseek"
max_context_length = 1000000
"#,
            )
            .unwrap(),
        );

        let active = document
            .get("active_model")
            .and_then(toml::Value::as_str)
            .unwrap()
            .to_string();
        let base = active.strip_prefix(PROXY_MODEL_PREFIX).unwrap().to_string();
        set_active_model(&mut document, &base);
        strip(&mut document);

        assert_eq!(active_provider(&document).as_deref(), Some("mistral"));
        assert_eq!(models(&document).len(), 1);
    }
}
