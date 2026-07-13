//! DB schema for the `impresspress__llm__providers` collection.
//!
//! Owns the table declaration plus row ↔ [`ProviderConfig`] conversion
//! helpers.
//!
//! Secrets are NOT stored in this table. Providers reference an entry in
//! `impresspress__admin__variables` via [`ProviderConfig::key_var`]; the
//! feature block resolves `key_var` → plaintext `api_key` in
//! `routes::reload_provider_service` (the single point where stored rows
//! become live configs) before calling `ProviderAdmin::configure`.
//! This matches the project-wide convention for sensitive config (single
//! storage location, admin-gated, masked in API responses) and avoids a
//! `*_encrypted` column name that would not actually be encrypted.

use std::collections::HashMap;

use wafer_core::clients::database::Record;

use super::providers::config::{ProviderConfig, ProviderProtocol};

pub const TABLE: &str = "impresspress__llm__providers";

/// Encode a [`ProviderConfig`] as a row payload for insert/update.
///
/// `cfg.api_key` is intentionally dropped — it is a runtime-resolved value
/// and must not be persisted. The secret lives in
/// `impresspress__admin__variables` and is referenced by `cfg.key_var`.
pub fn config_to_row(cfg: &ProviderConfig) -> HashMap<String, serde_json::Value> {
    // Borrowed entry point — callers that already own the config and don't
    // need it afterward should prefer `config_into_row` to avoid the
    // per-model `clone`.
    config_into_row(cfg.clone())
}

/// Owning variant of [`config_to_row`]. Moves each field out of `cfg`
/// instead of cloning, which matters most for the `models` `Vec<String>`.
pub fn config_into_row(cfg: ProviderConfig) -> HashMap<String, serde_json::Value> {
    let ProviderConfig {
        name,
        protocol,
        endpoint,
        api_key: _,
        key_var,
        models,
        enabled,
    } = cfg;

    let mut row = HashMap::new();
    row.insert("name".to_string(), serde_json::Value::String(name));
    row.insert(
        "protocol".to_string(),
        serde_json::Value::String(protocol.as_str().to_string()),
    );
    row.insert("endpoint".to_string(), serde_json::Value::String(endpoint));
    if let Some(var) = key_var {
        row.insert("key_var".to_string(), serde_json::Value::String(var));
    }
    row.insert(
        "models".to_string(),
        serde_json::Value::Array(models.into_iter().map(serde_json::Value::String).collect()),
    );
    row.insert(
        "enabled".to_string(),
        serde_json::Value::Number(serde_json::Number::from(if enabled { 1 } else { 0 })),
    );
    row
}

/// Decode a database [`Record`] into a [`ProviderConfig`].
///
/// The returned `api_key` is always `None` — the reload path
/// (`routes::reload_provider_service`) resolves it from `key_var` via the
/// config client before handing the config to the in-memory service.
pub fn row_to_config(record: &Record) -> Result<ProviderConfig, String> {
    let name = record
        .data
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing `name`".to_string())?
        .to_string();

    let protocol_str = record
        .data
        .get("protocol")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing `protocol`".to_string())?;
    let protocol = ProviderProtocol::parse(protocol_str)
        .ok_or_else(|| format!("invalid `protocol`: {protocol_str}"))?;

    let endpoint = record
        .data
        .get("endpoint")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing `endpoint`".to_string())?
        .to_string();

    let key_var = record
        .data
        .get("key_var")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let models = parse_models_field(record.data.get("models"));
    let enabled = parse_enabled_field(record.data.get("enabled"));

    Ok(ProviderConfig {
        name,
        protocol,
        endpoint,
        api_key: None,
        key_var,
        models,
        enabled,
    })
}

/// Coerce a stored `models` column value into the explicit model list.
///
/// Accepts a JSON array of strings (canonical) or a JSON-string-encoded
/// array (some DB backends serialise json columns as TEXT — the legacy
/// `provider_llm` table also stored models this way). Anything malformed
/// degrades to an empty list with a warning rather than poisoning the whole
/// row: the provider still works and models can be re-discovered via
/// `/v1/models`.
pub fn parse_models_field(value: Option<&serde_json::Value>) -> Vec<String> {
    match value {
        Some(serde_json::Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        Some(serde_json::Value::String(s)) if !s.is_empty() => {
            match serde_json::from_str::<Vec<String>>(s) {
                Ok(models) => models,
                Err(e) => {
                    tracing::warn!("invalid `models` json — treating as empty: {e}");
                    Vec::new()
                }
            }
        }
        _ => Vec::new(),
    }
}

/// Coerce a stored `enabled` column value (bool / int / string across DB
/// backends) into a bool. Missing or unrecognised values default to `true`.
pub fn parse_enabled_field(value: Option<&serde_json::Value>) -> bool {
    match value {
        Some(serde_json::Value::Bool(b)) => *b,
        Some(serde_json::Value::Number(n)) => n.as_i64().unwrap_or(0) != 0,
        Some(serde_json::Value::String(s)) => matches!(s.as_str(), "1" | "true"),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use wafer_core::clients::database::Record;

    use super::*;

    #[test]
    fn config_to_row_encodes_minimal_config() {
        let cfg = ProviderConfig::new(
            "openai-main",
            ProviderProtocol::OpenAi,
            "https://api.openai.com/v1",
        );
        let row = config_to_row(&cfg);
        assert_eq!(
            row.get("name").and_then(|v| v.as_str()),
            Some("openai-main")
        );
        assert_eq!(
            row.get("protocol").and_then(|v| v.as_str()),
            Some("open_ai")
        );
        assert_eq!(
            row.get("endpoint").and_then(|v| v.as_str()),
            Some("https://api.openai.com/v1")
        );
        assert!(!row.contains_key("api_key_encrypted"));
        assert!(!row.contains_key("key_var"));
        assert_eq!(row.get("models"), Some(&serde_json::json!([])));
        assert_eq!(row.get("enabled").and_then(|v| v.as_i64()), Some(1));
    }

    #[test]
    fn config_to_row_never_persists_api_key() {
        // Even if a runtime ProviderConfig holds a resolved plaintext
        // api_key, it must not be written to the providers table.
        let cfg = ProviderConfig::new(
            "local-llama",
            ProviderProtocol::OpenAiCompatible,
            "http://localhost:11434/v1",
        )
        .with_api_key("resolved-plaintext-secret")
        .with_key_var("IMPRESSPRESS__LLM__OPENAI_KEY")
        .with_models(vec!["llama3".into(), "mistral".into()]);
        let row = config_to_row(&cfg);
        assert!(
            !row.contains_key("api_key"),
            "api_key must never be persisted"
        );
        assert!(
            !row.contains_key("api_key_encrypted"),
            "api_key_encrypted must not exist"
        );
        assert_eq!(
            row.get("key_var").and_then(|v| v.as_str()),
            Some("IMPRESSPRESS__LLM__OPENAI_KEY")
        );
        assert_eq!(
            row.get("models"),
            Some(&serde_json::json!(["llama3", "mistral"]))
        );
    }

    #[test]
    fn roundtrip_drops_api_key_on_load() {
        // The runtime api_key (if set) is lost through the DB roundtrip —
        // callers must re-resolve via key_var after loading.
        let cfg = ProviderConfig::new(
            "anthropic-main",
            ProviderProtocol::Anthropic,
            "https://api.anthropic.com/v1",
        )
        .with_api_key("runtime-only")
        .with_key_var("IMPRESSPRESS__LLM__ANTHROPIC_KEY")
        .with_models(vec!["claude-opus-4-7".into()]);
        let row = config_to_row(&cfg);
        let record = Record {
            id: "abc123".into(),
            data: row,
        };
        let decoded = row_to_config(&record).expect("decode");
        assert_eq!(decoded.name, cfg.name);
        assert_eq!(decoded.protocol, cfg.protocol);
        assert_eq!(decoded.endpoint, cfg.endpoint);
        assert_eq!(decoded.key_var, cfg.key_var);
        assert_eq!(decoded.models, cfg.models);
        assert_eq!(decoded.enabled, cfg.enabled);
        assert!(
            decoded.api_key.is_none(),
            "api_key must be None after roundtrip — resolve via key_var at call time"
        );
    }

    #[test]
    fn row_to_config_rejects_invalid_protocol() {
        let record = Record {
            id: "r1".into(),
            data: {
                let mut d = HashMap::new();
                d.insert("name".into(), serde_json::json!("x"));
                d.insert("protocol".into(), serde_json::json!("openai")); // non-canonical
                d.insert("endpoint".into(), serde_json::json!("https://x"));
                d
            },
        };
        let err = row_to_config(&record).expect_err("must reject alias");
        assert!(err.contains("invalid `protocol`"), "got: {err}");
    }

    #[test]
    fn row_to_config_handles_missing_optionals() {
        let record = Record {
            id: "r1".into(),
            data: {
                let mut d = HashMap::new();
                d.insert("name".into(), serde_json::json!("x"));
                d.insert("protocol".into(), serde_json::json!("open_ai"));
                d.insert("endpoint".into(), serde_json::json!("https://x"));
                // no api_key_encrypted, no key_var, no models, no enabled
                d
            },
        };
        let cfg = row_to_config(&record).expect("decode");
        assert_eq!(cfg.name, "x");
        assert!(cfg.api_key.is_none());
        assert!(cfg.key_var.is_none());
        assert!(cfg.models.is_empty());
        assert!(cfg.enabled, "enabled should default to true when missing");
    }

    #[test]
    fn row_to_config_accepts_models_as_json_string() {
        // Defensive: some DB backends may serialize json columns as strings.
        let record = Record {
            id: "r1".into(),
            data: {
                let mut d = HashMap::new();
                d.insert("name".into(), serde_json::json!("x"));
                d.insert("protocol".into(), serde_json::json!("open_ai"));
                d.insert("endpoint".into(), serde_json::json!("https://x"));
                d.insert("models".into(), serde_json::json!(r#"["a","b"]"#));
                d
            },
        };
        let cfg = row_to_config(&record).expect("decode");
        assert_eq!(cfg.models, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn row_to_config_treats_malformed_models_json_as_empty() {
        // A malformed models string must not poison the whole row — the
        // provider still decodes (models can be re-discovered).
        let record = Record {
            id: "r1".into(),
            data: {
                let mut d = HashMap::new();
                d.insert("name".into(), serde_json::json!("x"));
                d.insert("protocol".into(), serde_json::json!("open_ai"));
                d.insert("endpoint".into(), serde_json::json!("https://x"));
                d.insert("models".into(), serde_json::json!("this is not json"));
                d
            },
        };
        let cfg = row_to_config(&record).expect("decode");
        assert!(cfg.models.is_empty(), "bad JSON should fall back to []");
    }

    #[test]
    fn row_to_config_enabled_from_int_zero_is_false() {
        let record = Record {
            id: "r1".into(),
            data: {
                let mut d = HashMap::new();
                d.insert("name".into(), serde_json::json!("x"));
                d.insert("protocol".into(), serde_json::json!("open_ai"));
                d.insert("endpoint".into(), serde_json::json!("https://x"));
                d.insert("enabled".into(), serde_json::json!(0));
                d
            },
        };
        let cfg = row_to_config(&record).expect("decode");
        assert!(!cfg.enabled);
    }
}
