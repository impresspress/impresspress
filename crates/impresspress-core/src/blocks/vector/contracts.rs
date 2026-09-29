//! Typed request/response contracts for the `/b/vector/api/*` JSON surface.
//!
//! These did not exist before this module. Every vector handler
//! deserialized its body into a private in-function struct and answered
//! with a `serde_json::json!` literal, so the block declared no schemas and
//! its JSON API was invisible in `/openapi.json`. The types below are the
//! *only* source of the schemas declared in [`super::VectorBlock`]'s
//! `BlockInfo::endpoints`: `.input::<T>()` / `.output::<T>()` derive them
//! from the same types the handlers deserialize into and serialize out of.
//!
//! # Mirrors of wafer-run wire types
//!
//! [`DistanceMetric`], [`SearchMode`], [`VectorEntryInput`],
//! [`MetadataFilterInput`] and [`VectorMatchView`] are field-for-field
//! mirrors of `wafer_core::clients::vector::{DistanceMetric, SearchMode,
//! VectorEntry, MetadataFilter, VectorMatch}`. Those live in wafer-run and
//! do not derive `schemars::JsonSchema`, so the handlers convert at the
//! boundary and the type the schema is derived from is the type that goes
//! out on (or comes in off) the wire — same reasoning as
//! `blocks::files::contracts`. The serde attributes are copied so the bytes
//! are identical to what the wire types produced.
//!
//! # One request has two parsers
//!
//! `POST /b/vector/api/indexes` is also the target of the admin modal's
//! URL-encoded form, where the checkbox arrives as the string `on` and only
//! when ticked. [`CreateIndexRequest::from_form`] builds the same type from
//! that shape with the coercions a form needs; the JSON path deserializes
//! the type directly and gets no such coercions, which is what the published
//! schema says.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use wafer_core::clients::vector as wire;

// ---------------------------------------------------------------------------
// Mirrors
// ---------------------------------------------------------------------------

/// Distance metric a vector index is built with. Mirrors
/// `wafer_core::clients::vector::DistanceMetric`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum DistanceMetric {
    /// Cosine similarity.
    Cosine,
    /// Euclidean (L2) distance.
    Euclidean,
    /// Dot product.
    DotProduct,
}

impl From<DistanceMetric> for wire::DistanceMetric {
    fn from(metric: DistanceMetric) -> Self {
        match metric {
            DistanceMetric::Cosine => Self::Cosine,
            DistanceMetric::Euclidean => Self::Euclidean,
            DistanceMetric::DotProduct => Self::DotProduct,
        }
    }
}

impl From<wire::DistanceMetric> for DistanceMetric {
    fn from(metric: wire::DistanceMetric) -> Self {
        match metric {
            wire::DistanceMetric::Cosine => Self::Cosine,
            wire::DistanceMetric::Euclidean => Self::Euclidean,
            wire::DistanceMetric::DotProduct => Self::DotProduct,
        }
    }
}

/// Search modality. Mirrors `wafer_core::clients::vector::SearchMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SearchMode {
    /// Pure vector similarity search.
    Vector,
    /// Pure keyword (full-text) search.
    Keyword,
    /// Hybrid vector + keyword search.
    Hybrid,
}

impl From<SearchMode> for wire::SearchMode {
    fn from(mode: SearchMode) -> Self {
        match mode {
            SearchMode::Vector => Self::Vector,
            SearchMode::Keyword => Self::Keyword,
            SearchMode::Hybrid => Self::Hybrid,
        }
    }
}

/// One row to upsert. Mirrors `wafer_core::clients::vector::VectorEntry`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct VectorEntryInput {
    /// Caller-supplied row id. Upserting the same id again replaces the row.
    pub id: String,
    /// Embedding vector; its length must match the index's `dimensions`.
    pub vector: Vec<f32>,
    /// Arbitrary JSON metadata stored alongside the vector and echoed on
    /// query hits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    /// Text to index for keyword search. Required when the index was created
    /// with `keyword_search`; ignored otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

impl From<VectorEntryInput> for wire::VectorEntry {
    fn from(entry: VectorEntryInput) -> Self {
        Self {
            id: entry.id,
            vector: entry.vector,
            metadata: entry.metadata,
            text: entry.text,
        }
    }
}

/// Equality-only metadata filter. Mirrors
/// `wafer_core::clients::vector::MetadataFilter`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MetadataFilterInput {
    /// Equality constraints: each key is a dot-path into the stored metadata
    /// and the value must match exactly.
    #[serde(default)]
    pub equals: BTreeMap<String, serde_json::Value>,
}

impl From<MetadataFilterInput> for wire::MetadataFilter {
    fn from(filter: MetadataFilterInput) -> Self {
        Self {
            equals: filter.equals,
        }
    }
}

/// One query hit. Mirrors `wafer_core::clients::vector::VectorMatch`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct VectorMatchView {
    /// Matched row id.
    pub id: String,
    /// Similarity score; its scale depends on the index's metric.
    pub score: f32,
    /// The metadata stored with the row. Absent when the row stored none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

impl From<wire::VectorMatch> for VectorMatchView {
    fn from(hit: wire::VectorMatch) -> Self {
        Self {
            id: hit.id,
            score: hit.score,
            metadata: hit.metadata,
        }
    }
}

// ---------------------------------------------------------------------------
// POST /b/vector/api/indexes
// ---------------------------------------------------------------------------

/// `POST /b/vector/api/indexes` request body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CreateIndexRequest {
    /// Index name: `[A-Za-z0-9_]` only. Every other endpoint addresses the
    /// index by this name.
    pub name: String,
    /// Embedding model id from the catalog. Omitted means the catalog's
    /// default model.
    pub model: Option<String>,
    /// Consistency check only: when given it must equal the model's own
    /// dimensionality, which is what the index is created with either way.
    pub dimensions: Option<u32>,
    /// Distance metric. Omitted means `cosine`.
    pub metric: Option<DistanceMetric>,
    /// Also store text for keyword and hybrid search.
    #[serde(default)]
    pub keyword_search: bool,
}

impl CreateIndexRequest {
    /// Build the request from the admin modal's URL-encoded form.
    ///
    /// Carries exactly the coercions the handler applied inline before this
    /// type existed, because a form can only send strings: `dimensions`
    /// parses as a number and is otherwise ignored, `metric` is one of the
    /// enum's tokens and is otherwise ignored, `keyword_search` is a checkbox
    /// (`on`) and also accepts `true` / `1` / `yes`, and an empty `model` is
    /// absent. The JSON path has none of these — there a string where a
    /// number belongs is a 400, as the schema says.
    pub fn from_form(form: &HashMap<String, String>) -> Self {
        let non_empty = |key: &str| form.get(key).filter(|v| !v.is_empty()).cloned();
        Self {
            name: form.get("name").cloned().unwrap_or_default(),
            model: non_empty("model"),
            dimensions: non_empty("dimensions").and_then(|v| v.parse().ok()),
            metric: non_empty("metric")
                .and_then(|v| serde_json::from_value(serde_json::Value::String(v)).ok()),
            keyword_search: crate::config_vars::form_bool(form, "keyword_search"),
        }
    }
}

/// `POST /b/vector/api/indexes` response body: the configuration the index
/// was created with, after model and metric defaults were applied.
///
/// Only for JSON callers. The admin modal posts the same endpoint with an
/// `HX-Request` header and receives the refreshed index list as HTML.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CreateIndexResponse {
    /// The name the index is addressed by.
    pub name: String,
    /// Embedding model id the index is bound to.
    pub model: String,
    /// Vector dimensionality, taken from the model.
    pub dimensions: u32,
    /// Distance metric the index was created with.
    pub metric: DistanceMetric,
    /// Whether the index also stores text for keyword and hybrid search.
    pub keyword_search: bool,
}

// ---------------------------------------------------------------------------
// GET /b/vector/api/indexes, GET /b/vector/api/stats
// ---------------------------------------------------------------------------

/// `GET /b/vector/api/indexes` response body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct IndexListResponse {
    /// Index names, in lexical order. Empty when no vector backend is
    /// available on this deployment.
    pub indexes: Vec<String>,
}

/// One index with its row count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct IndexStatsView {
    /// Index name.
    pub name: String,
    /// Rows currently stored. `0` when the count could not be read.
    pub count: u64,
}

/// `GET /b/vector/api/stats` response body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct IndexStatsResponse {
    /// Every index, in lexical order. Empty when no vector backend is
    /// available on this deployment.
    pub indexes: Vec<IndexStatsView>,
}

// ---------------------------------------------------------------------------
// Acknowledgement shared by the writes that return nothing else
// ---------------------------------------------------------------------------

/// Response body of `DELETE /b/vector/api/indexes/{name}`,
/// `POST /b/vector/api/upsert` and `DELETE /b/vector/api/{index}/{id}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AckResponse {
    /// Always `true`: a write that did not happen is an error response.
    pub ok: bool,
}

// ---------------------------------------------------------------------------
// POST /b/vector/api/upsert
// ---------------------------------------------------------------------------

/// `POST /b/vector/api/upsert` request body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UpsertRequest {
    /// Index name.
    pub index: String,
    /// Rows to insert or replace.
    pub entries: Vec<VectorEntryInput>,
}

// ---------------------------------------------------------------------------
// POST /b/vector/api/query
// ---------------------------------------------------------------------------

/// `POST /b/vector/api/query` request body. At least one of `text` and
/// `vector` is required. With `vector` alone it is the query vector as is;
/// with `text` alone the text is embedded with the index's own model. When
/// both are present `vector` is used as is and `text` is not embedded — it
/// then serves only as the keyword-query fallback in keyword and hybrid
/// mode.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct QueryRequest {
    /// Index name.
    pub index: String,
    /// Query text. Embedded with the model the index was created with when
    /// `vector` is absent; never embedded when `vector` is present. In
    /// keyword and hybrid mode it is also the keyword query unless
    /// `keyword_query` is given.
    pub text: Option<String>,
    /// Pre-computed query vector, used as is; its length must match the
    /// index's `dimensions`. When present, `text` is not embedded.
    pub vector: Option<Vec<f32>>,
    /// Maximum number of hits. Omitted means 10.
    pub top_k: Option<usize>,
    /// Restrict hits to rows whose metadata matches.
    pub filter: Option<MetadataFilterInput>,
    /// Search modality. Omitted means `hybrid` for an index created with
    /// `keyword_search`, `vector` otherwise.
    pub mode: Option<SearchMode>,
    /// Keyword query for keyword and hybrid mode. Omitted means `text`.
    pub keyword_query: Option<String>,
}

/// `POST /b/vector/api/query` response body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct QueryResponse {
    /// Hits, best first.
    pub matches: Vec<VectorMatchView>,
}

// ---------------------------------------------------------------------------
// POST /b/vector/api/ingest
// ---------------------------------------------------------------------------

/// `POST /b/vector/api/ingest` request body: chunk a document, embed each
/// chunk with the index's model and upsert the chunks. Re-ingesting the
/// same `document_id` replaces its previous chunks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct IngestRequest {
    /// Index name.
    pub index: String,
    /// Caller-supplied document id. Chunk ids are `{document_id}:{n}`.
    pub document_id: String,
    /// The document text.
    pub text: String,
    /// Arbitrary JSON stored on every chunk as `user_metadata`, beside the
    /// `document_id` and `chunk_index` the block adds.
    pub metadata: Option<serde_json::Value>,
    /// Prepend an LLM-written one-paragraph summary of the document to every
    /// chunk before embedding. Silently skipped when no default LLM is
    /// configured.
    #[serde(default)]
    pub contextual: bool,
}

/// `POST /b/vector/api/ingest` response body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct IngestResponse {
    /// Chunks written. `0` when the text was empty or whitespace only.
    pub chunks_created: usize,
}

// ---------------------------------------------------------------------------
// POST /b/vector/api/embed
// ---------------------------------------------------------------------------

/// `POST /b/vector/api/embed` request body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct EmbedRequest {
    /// Embedding model id. Omitted means the catalog's default model.
    pub model: Option<String>,
    /// Texts to embed; one vector is returned per text, in order. May be
    /// empty.
    pub texts: Vec<String>,
}

/// `POST /b/vector/api/embed` response body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct EmbedResponse {
    /// Embedding model that produced the vectors.
    pub model: String,
    /// Vector dimensionality.
    pub dimensions: u32,
    /// One vector per input text, in input order.
    pub vectors: Vec<Vec<f32>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The mirrors must serialize byte-for-byte like the wire types they
    /// stand in for, or the schema derived from them describes a body the
    /// handler does not send.
    #[test]
    fn mirrors_serialize_like_the_wire_types() {
        for (metric, wire_metric) in [
            (DistanceMetric::Cosine, wire::DistanceMetric::Cosine),
            (DistanceMetric::Euclidean, wire::DistanceMetric::Euclidean),
            (DistanceMetric::DotProduct, wire::DistanceMetric::DotProduct),
        ] {
            assert_eq!(
                serde_json::to_value(metric).expect("json"),
                serde_json::to_value(wire_metric).expect("json")
            );
            assert_eq!(wire::DistanceMetric::from(metric), wire_metric);
            assert_eq!(DistanceMetric::from(wire_metric), metric);
        }
        for (mode, wire_mode) in [
            (SearchMode::Vector, wire::SearchMode::Vector),
            (SearchMode::Keyword, wire::SearchMode::Keyword),
            (SearchMode::Hybrid, wire::SearchMode::Hybrid),
        ] {
            assert_eq!(
                serde_json::to_value(mode).expect("json"),
                serde_json::to_value(wire_mode).expect("json")
            );
            assert_eq!(wire::SearchMode::from(mode), wire_mode);
        }

        for hit in [
            wire::VectorMatch {
                id: "a".into(),
                score: 0.5,
                metadata: Some(serde_json::json!({ "k": "v" })),
            },
            wire::VectorMatch {
                id: "b".into(),
                score: 0.25,
                metadata: None,
            },
        ] {
            assert_eq!(
                serde_json::to_value(VectorMatchView::from(hit.clone())).expect("json"),
                serde_json::to_value(&hit).expect("json"),
                "{hit:?}"
            );
        }

        let entry = VectorEntryInput {
            id: "a".into(),
            vector: vec![0.1, 0.2],
            metadata: Some(serde_json::json!({ "k": "v" })),
            text: Some("hello".into()),
        };
        assert_eq!(
            serde_json::to_value(wire::VectorEntry::from(entry.clone())).expect("json"),
            serde_json::to_value(&entry).expect("json")
        );
    }

    #[test]
    fn from_form_applies_the_form_coercions() {
        let form = |pairs: &[(&str, &str)]| -> HashMap<String, String> {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };

        let ticked = CreateIndexRequest::from_form(&form(&[
            ("name", "docs"),
            ("model", ""),
            ("dimensions", "1024"),
            ("metric", "euclidean"),
            ("keyword_search", "on"),
        ]));
        assert_eq!(
            ticked,
            CreateIndexRequest {
                name: "docs".into(),
                model: None,
                dimensions: Some(1024),
                metric: Some(DistanceMetric::Euclidean),
                keyword_search: true,
            }
        );

        // Unticked checkbox is absent; unparsable number and unknown metric
        // are ignored, as the inline parser ignored them.
        let unticked = CreateIndexRequest::from_form(&form(&[
            ("name", "docs"),
            ("model", "bge-m3"),
            ("dimensions", "lots"),
            ("metric", "manhattan"),
        ]));
        assert_eq!(
            unticked,
            CreateIndexRequest {
                name: "docs".into(),
                model: Some("bge-m3".into()),
                dimensions: None,
                metric: None,
                keyword_search: false,
            }
        );
    }
}
