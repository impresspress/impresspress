//! Persistence for the dev sandbox control plane.
//!
//! One module per table; each owns its own `pub const TABLE` (the repo-wide
//! convention — see `auth/repo/users.rs`). Every query goes through the typed
//! `wafer_core::clients::database` client, so the schema stays swappable
//! between SQLite/D1 and Postgres.

pub mod builds;
pub mod generations;
pub mod runtime_state;
pub mod seed_info;

/// A fresh row id.
///
/// `new_v4` rather than `now_v7`: generation and build ids are quoted back to
/// the agent and embedded in export bundles, and a v7 id leaks the wall-clock
/// time of every workspace edit into those artifacts. Ordering comes from
/// `created_at`, which the rows carry explicitly.
///
/// Public because a generation's id is part of the manifest that is hashed
/// into `manifest_sha256` (design §11.3), so the caller has to hold it before
/// the row is written. One definition of "a row id" for the whole block.
pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// The current time, RFC 3339, as every other block stamps it.
pub(crate) fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// A JSON-encoded `TEXT` column, returned as **canonical** JSON (sorted keys,
/// no whitespace) on every backend.
///
/// A column can arrive in two shapes, and both are flattened here; of the
/// two points below, the second is the one that matters:
///
/// * A column declared `TEXT` comes back as the literal string on every backend
///   (SQLite, Postgres, D1, the browser's sql.js). A column declared JSON — or
///   one an earlier build's lazy column-add created for an object/array value —
///   comes back already decoded. `RecordExt::str_field` collapses the decoded
///   case to `""`, which would silently lose a whole manifest, so both are
///   accepted.
/// * Re-encoding a decoded value yields canonical JSON, whereas the literal
///   string is whatever was stored. Left alone, the *same manifest* would read
///   back differently depending on how its column is declared — and since
///   `manifest_sha256` is a hash over the canonical manifest (design §11.3),
///   that difference would make hash verification depend on the schema.
///
/// So both arms are normalized to canonical JSON. A generation whose manifest
/// was written non-canonically therefore fails its own hash check regardless
/// of how its column is declared — which is the correct outcome, and a loud
/// one.
///
/// The normalization is [`super::generation::canonicalize`] — the *same*
/// function [`super::generation::canonical_text`] writes with, not a second
/// implementation that happens to agree. Re-encoding through `serde_json`
/// alone would agree only while `Map` is `BTreeMap`-backed, and
/// `serde_json/preserve_order` is a feature any dependency in the graph can
/// turn on for everyone: the writer is already defended against that, and the
/// reader has to be defended by the same code rather than by a test noticing.
///
/// Text that does not parse as JSON is returned unchanged; the caller that
/// asked for it is the one that knows whether that is a problem.
pub(crate) fn json_text(record: &wafer_core::clients::database::Record, key: &str) -> String {
    let canonical = |value: serde_json::Value| super::generation::canonicalize(value).to_string();
    match record.data.get(key) {
        // A `TEXT` column, on every backend: the literal column text.
        Some(serde_json::Value::String(text)) => {
            serde_json::from_str::<serde_json::Value>(text).map_or_else(|_| text.clone(), canonical)
        }
        // A column declared JSON (or created by an earlier build's lazy
        // column-add for an object/array value): already decoded.
        Some(value) => canonical(value.clone()),
        None => String::new(),
    }
}

/// Every spelling a closed status enum's derived JSON schema lists.
///
/// The schema is generated from the enum's own variants (serde's spelling,
/// one entry per variant), so it is a list of every status that nobody had to
/// remember to update. The `ALL` completeness tests compare the hand-written
/// `ALL` arrays against it: a variant added to the enum and left out of `ALL`
/// fails them.
#[cfg(test)]
pub(crate) fn schema_spellings<T: schemars::JsonSchema>() -> std::collections::BTreeSet<String> {
    fn collect(value: &serde_json::Value, into: &mut std::collections::BTreeSet<String>) {
        match value {
            serde_json::Value::Object(map) => {
                for (key, value) in map {
                    match (key.as_str(), value) {
                        ("enum", serde_json::Value::Array(items)) => into.extend(
                            items
                                .iter()
                                .filter_map(|item| item.as_str().map(str::to_string)),
                        ),
                        ("const", serde_json::Value::String(item)) => {
                            into.insert(item.clone());
                        }
                        _ => collect(value, into),
                    }
                }
            }
            serde_json::Value::Array(items) => items.iter().for_each(|item| collect(item, into)),
            _ => {}
        }
    }
    let schema = serde_json::to_value(schemars::schema_for!(T)).expect("a schema encodes");
    let mut spellings = std::collections::BTreeSet::new();
    collect(&schema, &mut spellings);
    spellings
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use wafer_core::clients::database::Record;

    use super::json_text;

    fn record(value: serde_json::Value) -> Record {
        let mut data = HashMap::new();
        data.insert("manifest".to_string(), value);
        Record {
            id: String::new(),
            data: data.into_iter().collect(),
        }
    }

    const CANONICAL: &str = r#"{"a":1,"b":[{"x":true,"y":null}]}"#;

    /// The two shapes of the same stored manifest must decode to the same
    /// bytes — otherwise a `manifest_sha256` check would depend on how the
    /// column is declared.
    #[test]
    fn both_column_shapes_yield_the_same_canonical_text() {
        // A `TEXT` column hands back the literal string...
        let literal = record(serde_json::json!(CANONICAL));
        // ...a column declared JSON hands back the decoded value.
        let decoded = record(serde_json::from_str::<serde_json::Value>(CANONICAL).expect("parse"));

        assert_eq!(json_text(&literal, "manifest"), CANONICAL);
        assert_eq!(json_text(&decoded, "manifest"), CANONICAL);
    }

    /// Non-canonical input is canonicalized rather than passed through, so
    /// the result does not depend on how the column is declared. This also pins
    /// that `serde_json` has no `preserve_order`: were it ever enabled by
    /// feature unification, object key order would survive and this fails.
    #[test]
    fn non_canonical_json_is_canonicalized_on_both_paths() {
        let messy = r#"{ "b": [ { "y": null, "x": true } ], "a": 1 }"#;
        let literal = record(serde_json::json!(messy));
        let decoded = record(serde_json::from_str::<serde_json::Value>(messy).expect("parse"));

        assert_eq!(json_text(&literal, "manifest"), CANONICAL);
        assert_eq!(json_text(&decoded, "manifest"), CANONICAL);
    }

    #[test]
    fn unparseable_text_and_absent_columns_degrade_predictably() {
        assert_eq!(
            json_text(&record(serde_json::json!("not json at all")), "manifest"),
            "not json at all",
        );
        assert_eq!(json_text(&record(serde_json::json!("x")), "missing"), "");
    }
}
