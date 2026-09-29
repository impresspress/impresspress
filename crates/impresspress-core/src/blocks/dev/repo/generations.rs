//! The publication ledger (`impresspress__dev__generations`).
//!
//! Append-only: a generation is never edited back into a previous shape.
//! Rolling back publishes a *new* generation that copies an old one's
//! manifests (design §7.2), so the history stays a straight line.

use wafer_block::{
    db::{Filter, FilterOp, FilterTree, ListOptions, SortField},
    wire::database::BatchWrite,
};
use wafer_core::clients::database as db;
use wafer_run::{context::Context, ErrorCode, WaferError};

use crate::util::RecordExt;

pub const TABLE: &str = "impresspress__dev__generations";

/// Where a generation sits in its lifecycle.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum GenerationStatus {
    /// Recorded, not yet examined.
    Staged,
    /// Blobs, artifacts and guests are being checked.
    Validating,
    /// The runtime swap and site publish are in progress.
    Activating,
    /// Currently serving.
    Active,
    /// Abandoned; `failure_message` says why.
    Failed,
    /// No longer serving — replaced by a later generation.
    ///
    /// Written by the activation that replaced it, and by nothing else. It is
    /// not an ageing marker: a generation that falls out of the retention
    /// window is deleted rather than relabelled, so a `failed` generation
    /// still reads as `failed` for as long as the ledger keeps it.
    //
    // Worded for the agent, not for this file: these doc comments are
    // published in `/openapi.json`. The rule itself lives in
    // `super::super::retention`.
    Superseded,
}

impl GenerationStatus {
    /// Every status a row can rest in.
    ///
    /// Retention derives the statuses it may delete from this list and
    /// [`Self::survives_retention`] rather than restating them, so a status
    /// added to the enum cannot be silently left out of either set.
    pub const ALL: [Self; 6] = [
        Self::Staged,
        Self::Validating,
        Self::Activating,
        Self::Active,
        Self::Failed,
        Self::Superseded,
    ];

    /// Whether a row in this status is kept however far down the ledger it
    /// has fallen.
    ///
    /// Two kinds qualify, and both for the same reason — deleting one would
    /// destroy something the sandbox still needs rather than merely something
    /// old. [`Self::Active`] is what the site *is*: twenty failed activations
    /// after a good one do not make the good one collectable. The in-flight
    /// three are what boot convergence re-runs (design §7.3) — the journal
    /// may name one, and a recovery that cannot find its row is an
    /// interrupted activation nothing can finish.
    ///
    /// Exhaustive on purpose: a new status has to state which side it is on.
    pub fn survives_retention(self) -> bool {
        self.is_in_flight() || self == Self::Active
    }

    /// Whether an activation is still working on this generation.
    ///
    /// The journal may name such a row and boot convergence re-runs it
    /// (design §7.3), so it is a recovery target rather than history. Kept
    /// separate from [`Self::survives_retention`] because the two are looked
    /// up differently: the serving generation is one targeted row, the
    /// in-flight ones are a bounded set (see [`list_in_flight`]).
    ///
    /// Exhaustive on purpose: a new status has to state which side it is on.
    pub fn is_in_flight(self) -> bool {
        match self {
            Self::Staged | Self::Validating | Self::Activating => true,
            Self::Active | Self::Failed | Self::Superseded => false,
        }
    }

    /// Canonical string form (matches the serde representation and the
    /// column's `CHECK` constraint).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Staged => "staged",
            Self::Validating => "validating",
            Self::Activating => "activating",
            Self::Active => "active",
            Self::Failed => "failed",
            Self::Superseded => "superseded",
        }
    }

    /// Inverse of [`Self::as_str`]; `None` for an unrecognized value.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "staged" => Some(Self::Staged),
            "validating" => Some(Self::Validating),
            "activating" => Some(Self::Activating),
            "active" => Some(Self::Active),
            "failed" => Some(Self::Failed),
            "superseded" => Some(Self::Superseded),
            _ => None,
        }
    }
}

/// Why a generation was created.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum GenerationCause {
    /// A `site/**` write. Site-only — no runtime rebuild.
    SiteWrite,
    /// A `site/**` delete. Site-only — no runtime rebuild.
    SiteDelete,
    /// A successful compile stage; the block set changed.
    BlockCompile,
    /// A block was removed; the block set changed.
    BlockRemove,
    /// A republish of an earlier generation's manifests.
    Rollback,
    /// Generation 0, imported from the seed bundle on cold boot.
    Seed,
}

impl GenerationCause {
    /// Canonical string form (matches the serde representation and the
    /// column's `CHECK` constraint).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SiteWrite => "site_write",
            Self::SiteDelete => "site_delete",
            Self::BlockCompile => "block_compile",
            Self::BlockRemove => "block_remove",
            Self::Rollback => "rollback",
            Self::Seed => "seed",
        }
    }

    /// Inverse of [`Self::as_str`]; `None` for an unrecognized value.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "site_write" => Some(Self::SiteWrite),
            "site_delete" => Some(Self::SiteDelete),
            "block_compile" => Some(Self::BlockCompile),
            "block_remove" => Some(Self::BlockRemove),
            "rollback" => Some(Self::Rollback),
            "seed" => Some(Self::Seed),
            _ => None,
        }
    }

    /// Whether a generation with this cause changes the block set, and so
    /// needs a runtime rebuild rather than a site-only republish (§7.2).
    pub fn rebuilds_runtime(self) -> bool {
        match self {
            Self::SiteWrite | Self::SiteDelete => false,
            Self::BlockCompile | Self::BlockRemove | Self::Rollback | Self::Seed => true,
        }
    }
}

/// One ledger row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenerationRow {
    /// Generation id.
    pub id: String,
    /// The generation this one was derived from, or `None` for generation 0.
    pub parent_id: Option<String>,
    /// Lifecycle status.
    pub status: GenerationStatus,
    /// What created it.
    pub cause: GenerationCause,
    /// Canonical JSON of the `site` half of the manifest (§11.3).
    pub site_manifest_json: String,
    /// Canonical JSON of the `blocks` half of the manifest (§11.3).
    pub block_manifest_json: String,
    /// SHA-256 of the canonical manifest, hex-encoded.
    pub manifest_sha256: String,
    /// RFC 3339 creation time.
    pub created_at: String,
    /// RFC 3339 time the generation went live, if it ever did.
    pub activated_at: Option<String>,
    /// Why it failed, if it did.
    pub failure_message: Option<String>,
}

/// The caller-supplied half of a new ledger row; the repo stamps the
/// timestamp and the initial status.
///
/// The id is caller-supplied, unlike every other repo in the tree, because a
/// generation's id is *part of the manifest* that `manifest_sha256` is taken
/// over (design §11.3). A repo that minted it would hand back a row whose
/// stored hash could not cover its own identity — the caller mints the id
/// with [`super::new_id`], stamps it into the manifest, hashes, and inserts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewGeneration {
    /// The id to file the row under, from [`super::new_id`].
    pub id: String,
    /// Parent generation, or `None` for generation 0.
    pub parent_id: Option<String>,
    /// What created it.
    pub cause: GenerationCause,
    /// Canonical JSON of the `site` half of the manifest.
    pub site_manifest_json: String,
    /// Canonical JSON of the `blocks` half of the manifest.
    pub block_manifest_json: String,
    /// SHA-256 of the canonical manifest, hex-encoded.
    pub manifest_sha256: String,
}

/// Append a generation in [`GenerationStatus::Staged`], returning the stored
/// row (so the caller has the minted id without re-reading).
pub async fn insert(ctx: &dyn Context, new: &NewGeneration) -> Result<GenerationRow, WaferError> {
    let row = GenerationRow {
        id: new.id.clone(),
        parent_id: new.parent_id.clone(),
        status: GenerationStatus::Staged,
        cause: new.cause,
        site_manifest_json: new.site_manifest_json.clone(),
        block_manifest_json: new.block_manifest_json.clone(),
        manifest_sha256: new.manifest_sha256.clone(),
        created_at: super::now(),
        activated_at: None,
        failure_message: None,
    };

    db::create(
        ctx,
        TABLE,
        crate::util::json_map(serde_json::json!({
            "id": row.id,
            "parent_id": row.parent_id,
            "status": row.status.as_str(),
            "cause": row.cause.as_str(),
            "site_manifest_json": row.site_manifest_json,
            "block_manifest_json": row.block_manifest_json,
            "manifest_sha256": row.manifest_sha256,
            "created_at": row.created_at,
            "activated_at": row.activated_at,
            "failure_message": row.failure_message,
        })),
    )
    .await?;

    Ok(row)
}

/// Read one generation by id.
pub async fn get(ctx: &dyn Context, id: &str) -> Result<GenerationRow, WaferError> {
    decode(&db::get(ctx, TABLE, id).await?)
}

/// [`get`] for a caller that must tell the three outcomes apart: the outer
/// `Result` is the read itself, `None` is no such row, and the inner `Result`
/// is the row's decode. A missing or undecodable row is a fact about the
/// ledger; a read that failed is not.
pub async fn lookup(
    ctx: &dyn Context,
    id: &str,
) -> Result<Option<Result<GenerationRow, WaferError>>, WaferError> {
    match db::get(ctx, TABLE, id).await {
        Ok(record) => Ok(Some(decode(&record))),
        Err(e) if e.code == ErrorCode::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Move a generation to `status`, optionally recording why it failed and when
/// it went live.
///
/// `failure` and `activated_at` are `Option` writes: `None` leaves the column
/// alone rather than clearing it, so a status change never silently erases the
/// message that explains an earlier one.
pub async fn set_status(
    ctx: &dyn Context,
    id: &str,
    status: GenerationStatus,
    failure: Option<&str>,
    activated_at: Option<&str>,
) -> Result<(), WaferError> {
    let mut data = crate::util::json_map(serde_json::json!({
        "status": status.as_str(),
    }));
    if let Some(failure) = failure {
        data.insert("failure_message".into(), serde_json::json!(failure));
    }
    if let Some(activated_at) = activated_at {
        data.insert("activated_at".into(), serde_json::json!(activated_at));
    }
    db::update(ctx, TABLE, id, data).await?;
    Ok(())
}

/// The `limit` newest generations, newest first.
///
/// `id` is the tiebreaker, and it is not cosmetic. `created_at` is stamped
/// from `chrono::Utc::now()`, which on wasm32 resolves to whole milliseconds —
/// and an agent's activations arrive in bursts, so two generations minted in
/// one millisecond is an ordinary event rather than a race. Without a second
/// key their order is whatever the backend happens to return, which decides
/// what the ledger view shows first, which of them a rollback offers, and —
/// through the boundary [`list_prunable`] compares against — which one falls
/// outside the retention window and is deleted. `id` is a v4 uuid, so the
/// order it imposes is arbitrary; being arbitrary and *stable* is the whole
/// requirement, and [`list_prunable`] orders rows the same way for exactly
/// that reason.
pub async fn list_recent(ctx: &dyn Context, limit: u32) -> Result<Vec<GenerationRow>, WaferError> {
    let list = db::list(
        ctx,
        TABLE,
        &ListOptions {
            sort: newest_first(),
            limit: Some(limit.clamp(1, MAX_LIST_LIMIT)),
            skip_count: true,
            ..Default::default()
        },
    )
    .await?;
    list.records.iter().map(decode).collect()
}

/// Upper bound on one page of any listing here.
///
/// It bounds a *page*, not the retention pass: [`super::super::retention`]
/// deletes what a page holds and asks for the next one, so a ledger far past
/// the window is collected in full rather than down to this many rows.
const MAX_LIST_LIMIT: u32 = 200;

/// The generation that is serving, or `None` on a fresh instance.
///
/// A targeted lookup rather than a slice of a listing, and that is the whole
/// point: this row is what the site *is*, and it must reach the retained set
/// no matter how far down the ledger it has fallen or how many other rows
/// share its status filter. Exactly one row is [`GenerationStatus::Active`]
/// at a time — the activation that commits a generation supersedes the one it
/// replaced — so `limit: 1` under the usual ordering is the whole answer, and
/// the newest wins if a hand-edited row ever made it two.
pub async fn find_active(ctx: &dyn Context) -> Result<Option<GenerationRow>, WaferError> {
    let list = db::list(
        ctx,
        TABLE,
        &ListOptions {
            filters: vec![Filter {
                field: "status".into(),
                operator: FilterOp::Equal,
                value: serde_json::json!(GenerationStatus::Active.as_str()),
            }],
            sort: newest_first(),
            limit: Some(1),
            skip_count: true,
            ..Default::default()
        },
    )
    .await?;
    list.records.first().map(decode).transpose()
}

/// Every generation an activation has not finished with, newest first.
///
/// Staged, validating or activating: rows the journal may be converging on,
/// which retention therefore keeps whatever their age. Capped at
/// [`MAX_LIST_LIMIT`] like every listing here, and the cap is safe because the
/// set is bounded rather than merely usually small —
/// `activation::converge_on_boot` retires every in-flight row the journal does
/// not name, so the only rows this can return are the one being converged on
/// and the at-most-one an activation is running right now.
pub async fn list_in_flight(ctx: &dyn Context) -> Result<Vec<GenerationRow>, WaferError> {
    let list = db::list(
        ctx,
        TABLE,
        &ListOptions {
            filters: vec![status_in(GenerationStatus::is_in_flight)],
            sort: newest_first(),
            limit: Some(MAX_LIST_LIMIT),
            skip_count: true,
            ..Default::default()
        },
    )
    .await?;
    list.records.iter().map(decode).collect()
}

/// One page of the rows retention may delete: everything strictly older than
/// `boundary` whose status does not survive retention, newest first.
///
/// "Older than" is the order [`list_recent`] imposes, tiebreaker included —
/// `created_at` first, `id` when two rows share a millisecond — so the row a
/// caller passes as the boundary is exactly the last row it decided to keep,
/// and nothing between the two definitions can fall through.
///
/// A query rather than a listing walked in memory: the rows to delete are the
/// ones a `list_recent` page would *not* return, and asking the database for
/// them directly is what stops the pass being bounded by [`MAX_LIST_LIMIT`].
pub async fn list_prunable(
    ctx: &dyn Context,
    boundary: &GenerationRow,
) -> Result<Vec<GenerationRow>, WaferError> {
    let list = db::list(
        ctx,
        TABLE,
        &ListOptions {
            filters: vec![status_in(|status| !status.survives_retention())],
            // AND-ed onto the status filter by the client: an OR group is the
            // one shape a flat filter list cannot express, and the tiebreaker
            // needs it.
            filter_tree: Some(vec![FilterTree::Any(vec![
                FilterTree::Leaf(Filter {
                    field: "created_at".into(),
                    operator: FilterOp::LessThan,
                    value: serde_json::json!(boundary.created_at),
                }),
                FilterTree::All(vec![
                    FilterTree::Leaf(Filter {
                        field: "created_at".into(),
                        operator: FilterOp::Equal,
                        value: serde_json::json!(boundary.created_at),
                    }),
                    FilterTree::Leaf(Filter {
                        field: "id".into(),
                        operator: FilterOp::LessThan,
                        value: serde_json::json!(boundary.id),
                    }),
                ]),
            ])]),
            sort: newest_first(),
            limit: Some(MAX_LIST_LIMIT),
            skip_count: true,
            ..Default::default()
        },
    )
    .await?;
    list.records.iter().map(decode).collect()
}

/// Delete the rows `ids` names, as one transaction.
///
/// Retention is the only caller: the ledger is append-only for as long as a
/// generation is retained, and the one thing that removes a row is falling
/// out of the window. One `db::batch` for a page of [`list_prunable`] —
/// never more than [`MAX_LIST_LIMIT`] ids, well inside what one batch takes —
/// so a pass deletes a page in one write (one OPFS flush in the browser)
/// rather than one per row. An id no row has any more is not an error: the
/// row is gone, which is what was asked.
pub async fn delete_many(ctx: &dyn Context, ids: &[String]) -> Result<(), WaferError> {
    if ids.is_empty() {
        return Ok(());
    }
    let ops = ids
        .iter()
        .map(|id| BatchWrite::Delete {
            collection: TABLE.to_string(),
            id: id.clone(),
        })
        .collect();
    db::batch(ctx, ops).await?;
    Ok(())
}

/// A `status IN (…)` filter over the statuses `keep` accepts.
///
/// Derived from [`GenerationStatus::ALL`] rather than written out, so the two
/// halves of the split — retained and prunable — cannot both forget a status
/// that was added to the enum.
fn status_in(keep: impl Fn(GenerationStatus) -> bool) -> Filter {
    let statuses: Vec<&str> = GenerationStatus::ALL
        .iter()
        .filter(|status| keep(**status))
        .map(|status| status.as_str())
        .collect();
    Filter {
        field: "status".into(),
        operator: FilterOp::In,
        value: serde_json::json!(statuses),
    }
}

/// Newest first, with `id` breaking a `created_at` tie — the one ordering
/// every listing here uses, and the one the retention boundary is defined in.
///
/// The database appends a table's primary key (`id` here) to a sorted list
/// anyway, in the last sort term's direction. It is named here regardless because
/// [`list_prunable`]'s boundary filter compares `id` in exactly this
/// direction, and the order that filter depends on belongs beside it rather
/// than in the runtime's defaults.
fn newest_first() -> Vec<SortField> {
    vec![
        SortField {
            field: "created_at".into(),
            desc: true,
        },
        SortField {
            field: "id".into(),
            desc: true,
        },
    ]
}

/// Decode a stored row. An unrecognized `status` or `cause` is an error, never
/// a default: the ledger drives what gets served, so an unreadable row must
/// stop the caller rather than be quietly reinterpreted.
fn decode(record: &db::Record) -> Result<GenerationRow, WaferError> {
    let status_text = record.str_field("status");
    let cause_text = record.str_field("cause");
    Ok(GenerationRow {
        id: record.str_field("id").to_string(),
        parent_id: record.opt_str_field("parent_id"),
        status: GenerationStatus::parse(status_text)
            .ok_or_else(|| unknown_value("status", status_text))?,
        cause: GenerationCause::parse(cause_text)
            .ok_or_else(|| unknown_value("cause", cause_text))?,
        site_manifest_json: super::json_text(record, "site_manifest_json"),
        block_manifest_json: super::json_text(record, "block_manifest_json"),
        manifest_sha256: record.str_field("manifest_sha256").to_string(),
        created_at: record.str_field("created_at").to_string(),
        activated_at: record.opt_str_field("activated_at"),
        failure_message: record.opt_str_field("failure_message"),
    })
}

fn unknown_value(column: &str, value: &str) -> WaferError {
    WaferError::new(
        ErrorCode::Internal,
        format!("{TABLE}: unknown {column} `{value}`"),
    )
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::{blocks::dev::test_support::FakeControl, test_support::TestContext};

    /// A manifest pair in the canonical form design §11.3 mandates (sorted
    /// keys, no whitespace) — the form `manifest_sha256` is a hash over.
    ///
    /// Both are JSON-shaped, which is the point: the SQLite backend sniffs
    /// JSON-shaped `TEXT` back into a decoded value in `row_to_record`, so a
    /// repo reading these columns with `str_field` would round-trip them to
    /// `""` here while staying green on Postgres. `repo::json_text` is what
    /// makes the round trip byte-exact, and only for canonical input — see
    /// its own tests for the non-canonical case.
    const SITE_MANIFEST: &str = r#"{"files":[{"content_type":"text/html; charset=utf-8","path":"index.html","sha256":"aa","size":5}]}"#;
    const BLOCK_MANIFEST: &str = r#"[{"artifact_sha256":"bb","capabilities":{},"name":"site/newsletter","routes":[{"access":"Public","prefix":"/b/newsletter/"}],"wafer_guest_version":1}]"#;

    fn new_generation(cause: GenerationCause) -> NewGeneration {
        NewGeneration {
            id: super::super::new_id(),
            parent_id: None,
            cause,
            site_manifest_json: SITE_MANIFEST.to_string(),
            block_manifest_json: BLOCK_MANIFEST.to_string(),
            manifest_sha256: "cc".to_string(),
        }
    }

    fn record(status: &str, cause: &str) -> db::Record {
        let mut data = HashMap::new();
        data.insert("id".to_string(), serde_json::json!("g1"));
        data.insert("parent_id".to_string(), serde_json::Value::Null);
        data.insert("status".to_string(), serde_json::json!(status));
        data.insert("cause".to_string(), serde_json::json!(cause));
        data.insert("site_manifest_json".to_string(), serde_json::json!("{}"));
        data.insert("block_manifest_json".to_string(), serde_json::json!("[]"));
        data.insert("manifest_sha256".to_string(), serde_json::json!("cc"));
        data.insert("created_at".to_string(), serde_json::json!("1970-01-01"));
        data.insert("activated_at".to_string(), serde_json::Value::Null);
        data.insert("failure_message".to_string(), serde_json::Value::Null);
        db::Record {
            id: "g1".to_string(),
            data,
        }
    }

    /// `created_at` is millisecond-resolution on wasm32 and an agent's
    /// activations arrive in bursts, so a tie is ordinary. The order the
    /// ledger view shows, the set a rollback offers and the retention boundary
    /// all read this list, so a tie must resolve the same way every time.
    ///
    /// Written through `db::create` rather than `insert` because `insert`
    /// stamps `created_at` itself — a tie cannot be produced through the API
    /// that is being defended.
    #[tokio::test]
    async fn generations_minted_in_the_same_millisecond_have_a_stable_order() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        let minted_at = "2026-09-03T00:00:00.000Z";
        for id in ["b-second", "a-first", "c-third"] {
            db::create(
                &ctx,
                TABLE,
                crate::util::json_map(serde_json::json!({
                    "id": id,
                    "parent_id": serde_json::Value::Null,
                    "status": GenerationStatus::Active.as_str(),
                    "cause": GenerationCause::SiteWrite.as_str(),
                    "site_manifest_json": SITE_MANIFEST,
                    "block_manifest_json": BLOCK_MANIFEST,
                    "manifest_sha256": "cc",
                    "created_at": minted_at,
                    "activated_at": serde_json::Value::Null,
                    "failure_message": serde_json::Value::Null,
                })),
            )
            .await
            .expect("create");
        }

        let ids: Vec<String> = list_recent(&ctx, 10)
            .await
            .expect("list")
            .into_iter()
            .map(|row| row.id)
            .collect();
        assert_eq!(
            ids,
            vec![
                "c-third".to_string(),
                "b-second".to_string(),
                "a-first".to_string()
            ],
            "a `created_at` tie is broken by id, descending"
        );
        // And it is an order, not a coincidence: reading again answers the
        // same thing.
        let again: Vec<String> = list_recent(&ctx, 10)
            .await
            .expect("list")
            .into_iter()
            .map(|row| row.id)
            .collect();
        assert_eq!(again, ids);
    }

    #[tokio::test]
    async fn insert_then_get_round_trips_every_column() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        let inserted = insert(&ctx, &new_generation(GenerationCause::BlockCompile))
            .await
            .expect("insert");

        assert_eq!(inserted.status, GenerationStatus::Staged);
        assert!(inserted.activated_at.is_none());
        assert!(inserted.failure_message.is_none());

        let read_back = get(&ctx, &inserted.id).await.expect("get");
        assert_eq!(read_back, inserted);
        // The manifests must come back byte-identical, not as a re-serialized
        // value: `manifest_sha256` is computed over the canonical text.
        assert_eq!(read_back.site_manifest_json, SITE_MANIFEST);
        assert_eq!(read_back.block_manifest_json, BLOCK_MANIFEST);
        assert_eq!(read_back.cause, GenerationCause::BlockCompile);
    }

    #[tokio::test]
    async fn set_status_records_the_failure_and_the_activation_time() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        let row = insert(&ctx, &new_generation(GenerationCause::SiteWrite))
            .await
            .expect("insert");

        set_status(
            &ctx,
            &row.id,
            GenerationStatus::Active,
            None,
            Some("2026-09-03T00:00:00Z"),
        )
        .await
        .expect("activate");
        let active = get(&ctx, &row.id).await.expect("get");
        assert_eq!(active.status, GenerationStatus::Active);
        assert_eq!(active.activated_at.as_deref(), Some("2026-09-03T00:00:00Z"));

        // A later status change with `None` must leave the earlier columns
        // alone: a supersede that erased why a generation went live (or why
        // an earlier one failed) would destroy the ledger's only record of it.
        set_status(&ctx, &row.id, GenerationStatus::Superseded, None, None)
            .await
            .expect("supersede");
        let superseded = get(&ctx, &row.id).await.expect("get");
        assert_eq!(superseded.status, GenerationStatus::Superseded);
        assert_eq!(
            superseded.activated_at.as_deref(),
            Some("2026-09-03T00:00:00Z")
        );

        set_status(
            &ctx,
            &row.id,
            GenerationStatus::Failed,
            Some("probe trapped"),
            None,
        )
        .await
        .expect("fail");
        let failed = get(&ctx, &row.id).await.expect("get");
        assert_eq!(failed.status, GenerationStatus::Failed);
        assert_eq!(failed.failure_message.as_deref(), Some("probe trapped"));
    }

    #[tokio::test]
    async fn list_recent_is_newest_first_and_honours_the_limit() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        let mut ids = Vec::new();
        for _ in 0..3 {
            ids.push(
                insert(&ctx, &new_generation(GenerationCause::SiteWrite))
                    .await
                    .expect("insert"),
            );
        }

        // Ordering is by `created_at`, so the fixture is only meaningful if
        // the three stamps differ. Assert that first, so a clock too coarse
        // to separate them fails here instead of as a confusing order
        // mismatch below.
        assert!(
            ids[0].created_at < ids[1].created_at && ids[1].created_at < ids[2].created_at,
            "insert must produce strictly increasing created_at: {:?}",
            ids.iter().map(|r| &r.created_at).collect::<Vec<_>>(),
        );

        let listed = list_recent(&ctx, 10).await.expect("list");
        assert_eq!(
            listed.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec![ids[2].id.as_str(), ids[1].id.as_str(), ids[0].id.as_str()],
        );

        let capped = list_recent(&ctx, 2).await.expect("list");
        assert_eq!(capped.len(), 2);
        assert_eq!(capped[0].id, ids[2].id);
    }

    /// The two halves of the status split must partition the enum: a status
    /// missing from both would make retention keep a row forever *and* never
    /// list it as retained, and one in both would make it prunable and
    /// unprunable at once.
    #[test]
    fn every_status_is_either_retained_or_prunable() {
        let retained: Vec<GenerationStatus> = GenerationStatus::ALL
            .into_iter()
            .filter(|s| s.survives_retention())
            .collect();
        let prunable: Vec<GenerationStatus> = GenerationStatus::ALL
            .into_iter()
            .filter(|s| !s.survives_retention())
            .collect();
        assert_eq!(retained.len() + prunable.len(), GenerationStatus::ALL.len());
        assert_eq!(
            retained,
            vec![
                GenerationStatus::Staged,
                GenerationStatus::Validating,
                GenerationStatus::Activating,
                GenerationStatus::Active,
            ],
        );
        // The serving row is retained but is NOT in flight: it is looked up on
        // its own, and the in-flight set is what boot retirement bounds.
        assert!(!GenerationStatus::Active.is_in_flight());
        assert_eq!(
            GenerationStatus::ALL
                .into_iter()
                .filter(|s| s.is_in_flight())
                .count(),
            3,
        );
        assert_eq!(
            prunable,
            vec![GenerationStatus::Failed, GenerationStatus::Superseded],
        );
    }

    /// The boundary row itself is kept, everything strictly older than it is
    /// offered up, and a status that survives retention is never offered
    /// however old it is.
    #[tokio::test]
    async fn list_prunable_offers_the_rows_older_than_the_boundary() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        let mut rows = Vec::new();
        for _ in 0..4 {
            rows.push(
                insert(&ctx, &new_generation(GenerationCause::SiteWrite))
                    .await
                    .expect("insert"),
            );
        }
        // Oldest first as inserted: [0] [1] [2] [3]. Everything is `Staged`,
        // which survives retention — so nothing is prunable yet.
        assert!(list_prunable(&ctx, &rows[2])
            .await
            .expect("list")
            .is_empty());

        for row in &rows[..3] {
            set_status(&ctx, &row.id, GenerationStatus::Superseded, None, None)
                .await
                .expect("supersede");
        }
        // The boundary is row 2, so rows 0 and 1 go and row 2 stays.
        let prunable = list_prunable(&ctx, &rows[2]).await.expect("list");
        assert_eq!(
            prunable.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec![rows[1].id.as_str(), rows[0].id.as_str()],
            "newest first, and the boundary itself is not in it",
        );

        // A `Failed` row is prunable too — retention deletes by age, not by
        // how the generation ended.
        set_status(&ctx, &rows[0].id, GenerationStatus::Failed, None, None)
            .await
            .expect("fail");
        assert_eq!(list_prunable(&ctx, &rows[2]).await.expect("list").len(), 2);
    }

    /// A `created_at` tie is broken by id in `list_prunable` exactly as it is
    /// in `list_recent`. Written through `db::create` because `insert` stamps
    /// the timestamp itself and a tie cannot be produced through it.
    #[tokio::test]
    async fn a_created_at_tie_is_split_by_id_on_both_sides_of_the_boundary() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        let minted_at = "2026-09-03T00:00:00.000Z";
        for id in ["a", "b", "c"] {
            db::create(
                &ctx,
                TABLE,
                crate::util::json_map(serde_json::json!({
                    "id": id,
                    "parent_id": serde_json::Value::Null,
                    "status": GenerationStatus::Superseded.as_str(),
                    "cause": GenerationCause::SiteWrite.as_str(),
                    "site_manifest_json": SITE_MANIFEST,
                    "block_manifest_json": BLOCK_MANIFEST,
                    "manifest_sha256": "cc",
                    "created_at": minted_at,
                    "activated_at": serde_json::Value::Null,
                    "failure_message": serde_json::Value::Null,
                })),
            )
            .await
            .expect("create");
        }

        let boundary = get(&ctx, "b").await.expect("get");
        let prunable = list_prunable(&ctx, &boundary).await.expect("list");
        assert_eq!(
            prunable.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["a"],
            "only the row the same-millisecond ordering puts BELOW the boundary",
        );
    }

    #[tokio::test]
    async fn the_serving_and_in_flight_rows_are_found_by_status_not_by_age() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        let staged = insert(&ctx, &new_generation(GenerationCause::SiteWrite))
            .await
            .expect("insert");
        let active = insert(&ctx, &new_generation(GenerationCause::SiteWrite))
            .await
            .expect("insert");
        let gone = insert(&ctx, &new_generation(GenerationCause::SiteWrite))
            .await
            .expect("insert");
        set_status(&ctx, &active.id, GenerationStatus::Active, None, None)
            .await
            .expect("activate");
        set_status(&ctx, &gone.id, GenerationStatus::Superseded, None, None)
            .await
            .expect("supersede");

        assert_eq!(
            find_active(&ctx).await.expect("find").map(|row| row.id),
            Some(active.id),
        );
        assert_eq!(
            list_in_flight(&ctx)
                .await
                .expect("list")
                .into_iter()
                .map(|row| row.id)
                .collect::<Vec<_>>(),
            vec![staged.id],
        );
    }

    #[tokio::test]
    async fn a_fresh_ledger_is_serving_nothing() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        assert_eq!(find_active(&ctx).await.expect("find"), None);
        assert!(list_in_flight(&ctx).await.expect("list").is_empty());
    }

    #[tokio::test]
    async fn delete_many_removes_exactly_the_named_rows_from_the_ledger() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        let mut rows = Vec::new();
        for _ in 0..3 {
            rows.push(
                insert(&ctx, &new_generation(GenerationCause::SiteWrite))
                    .await
                    .expect("insert"),
            );
        }
        let gone = [rows[0].id.clone(), rows[2].id.clone()];
        delete_many(&ctx, &gone).await.expect("delete");
        for id in &gone {
            assert_eq!(
                get(&ctx, id).await.expect_err("gone").code,
                ErrorCode::NotFound
            );
        }
        let left: Vec<String> = list_recent(&ctx, 10)
            .await
            .expect("list")
            .into_iter()
            .map(|row| row.id)
            .collect();
        assert_eq!(left, [rows[1].id.clone()]);

        // An id no row has any more is not an error, and nothing is left to
        // delete for an empty list.
        delete_many(&ctx, &gone).await.expect("already gone");
        delete_many(&ctx, &[]).await.expect("nothing to delete");
    }

    #[test]
    fn decode_refuses_an_unknown_status_or_cause() {
        assert_eq!(
            decode(&record("staged", "site_write"))
                .expect("decode")
                .status,
            GenerationStatus::Staged,
        );

        let bad_status = decode(&record("archived", "site_write")).expect_err("unknown status");
        assert_eq!(bad_status.code, ErrorCode::Internal);
        assert!(bad_status.message.contains("archived"), "{bad_status}");

        let bad_cause = decode(&record("staged", "vibes")).expect_err("unknown cause");
        assert!(bad_cause.message.contains("vibes"), "{bad_cause}");
    }
}
