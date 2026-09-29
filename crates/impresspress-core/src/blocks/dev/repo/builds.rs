//! Staged compiler artifacts (`impresspress__dev__builds`).
//!
//! One row per compile that produced bytes, whether or not those bytes turned
//! out to be a usable block. The row keeps the source hash, the artifact hash,
//! the reported `BlockInfo` and the compiler diagnostics, so a refusal can be
//! explained after the fact without re-running the toolchain.

use std::collections::{BTreeMap, BTreeSet};

use wafer_block::db::{Filter, FilterOp, ListOptions, SortField};
use wafer_core::clients::database as db;
use wafer_run::{context::Context, ErrorCode, WaferError};

use crate::util::RecordExt;

pub const TABLE: &str = "impresspress__dev__builds";

/// Upper bound on one `list_recent` page.
const MAX_LIST_LIMIT: u32 = 200;

/// Whether a staged artifact was accepted.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum BuildStatus {
    /// Stored, not yet validated.
    Staged,
    /// Loaded, initialized, started and answered a probe.
    Valid,
    /// Refused; `diagnostics_json` says why.
    Invalid,
}

impl BuildStatus {
    /// Whether a row in this status says its compile has not reached a
    /// generation yet — and so whether the artifact it names is protected by
    /// the row alone.
    ///
    /// Only [`Self::Staged`] does. Staging inserts the row *before* it stores
    /// the bytes and leaves it staged until the activation it asks for has
    /// minted a generation, so the row covers exactly the interval in which
    /// nothing else names the artifact ([`super::super::gc`]). `Valid` and
    /// `Invalid` are both terminal: by then either a generation names the
    /// artifact or the compile is over and it is garbage.
    ///
    /// Exhaustive on purpose: a new status has to state which side it is on.
    pub fn is_in_flight(self) -> bool {
        match self {
            Self::Staged => true,
            Self::Valid | Self::Invalid => false,
        }
    }

    /// Canonical string form (matches the serde representation and the
    /// column's `CHECK` constraint).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Staged => "staged",
            Self::Valid => "valid",
            Self::Invalid => "invalid",
        }
    }

    /// Inverse of [`Self::as_str`]; `None` for an unrecognized value.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "staged" => Some(Self::Staged),
            "valid" => Some(Self::Valid),
            "invalid" => Some(Self::Invalid),
            _ => None,
        }
    }
}

/// One build row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildRow {
    /// Build id.
    pub id: String,
    /// Registered block name the artifact was compiled for (`site/{name}`).
    pub block_name: String,
    /// SHA-256 of the source manifest the compile ran against, hex-encoded.
    pub source_manifest_sha256: String,
    /// SHA-256 of the produced artifact, hex-encoded.
    pub artifact_sha256: String,
    /// JSON of the `BlockInfo` the guest reported, or `"null"` if it never
    /// got that far.
    pub block_info_json: String,
    /// JSON array of compiler/validator diagnostics.
    pub diagnostics_json: String,
    /// Pinned toolchain revision that produced the artifact.
    pub compiler_version: String,
    /// Stored size of the artifact, in bytes.
    ///
    /// Here rather than read back from the object store, because this table
    /// is the index of what the store holds — the collector deletes a row
    /// with the artifact it names — and `dev_status` reports the store's size
    /// on every poll.
    pub artifact_bytes: u64,
    /// Whether the artifact was accepted.
    pub status: BuildStatus,
    /// RFC 3339 creation time.
    pub created_at: String,
}

/// The caller-supplied half of a new build row; the repo mints the id,
/// timestamp and initial status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewBuild {
    /// Registered block name the artifact was compiled for.
    pub block_name: String,
    /// SHA-256 of the source manifest the compile ran against.
    pub source_manifest_sha256: String,
    /// SHA-256 of the produced artifact.
    pub artifact_sha256: String,
    /// JSON of the reported `BlockInfo`.
    pub block_info_json: String,
    /// JSON array of compiler diagnostics.
    pub diagnostics_json: String,
    /// Pinned toolchain revision.
    pub compiler_version: String,
    /// Stored size of the artifact, in bytes.
    pub artifact_bytes: u64,
}

/// Append a build in [`BuildStatus::Staged`], returning the stored row.
pub async fn insert(ctx: &dyn Context, new: &NewBuild) -> Result<BuildRow, WaferError> {
    let row = BuildRow {
        id: super::new_id(),
        block_name: new.block_name.clone(),
        source_manifest_sha256: new.source_manifest_sha256.clone(),
        artifact_sha256: new.artifact_sha256.clone(),
        block_info_json: new.block_info_json.clone(),
        diagnostics_json: new.diagnostics_json.clone(),
        compiler_version: new.compiler_version.clone(),
        artifact_bytes: new.artifact_bytes,
        status: BuildStatus::Staged,
        created_at: super::now(),
    };

    db::create(
        ctx,
        TABLE,
        crate::util::json_map(serde_json::json!({
            "id": row.id,
            "block_name": row.block_name,
            "source_manifest_sha256": row.source_manifest_sha256,
            "artifact_sha256": row.artifact_sha256,
            "block_info_json": row.block_info_json,
            "diagnostics_json": row.diagnostics_json,
            "compiler_version": row.compiler_version,
            "artifact_bytes": row.artifact_bytes,
            "status": row.status.as_str(),
            "created_at": row.created_at,
        })),
    )
    .await?;

    Ok(row)
}

/// Read one build by id.
pub async fn get(ctx: &dyn Context, id: &str) -> Result<BuildRow, WaferError> {
    decode(&db::get(ctx, TABLE, id).await?)
}

/// Move a build to `status`, optionally replacing its diagnostics and the
/// `BlockInfo` the guest reported.
///
/// Both extras are `Option` writes: `None` leaves the column alone rather
/// than clearing the compiler output that explains the row, or the info a
/// later validation reads back. A row is inserted before the guest has run,
/// so `block_info_json` is `"null"` until this call fills it in.
pub async fn set_status(
    ctx: &dyn Context,
    id: &str,
    status: BuildStatus,
    diagnostics_json: Option<&str>,
    block_info_json: Option<&str>,
) -> Result<(), WaferError> {
    let mut data = crate::util::json_map(serde_json::json!({
        "status": status.as_str(),
    }));
    if let Some(diagnostics_json) = diagnostics_json {
        data.insert(
            "diagnostics_json".into(),
            serde_json::json!(diagnostics_json),
        );
    }
    if let Some(block_info_json) = block_info_json {
        data.insert("block_info_json".into(), serde_json::json!(block_info_json));
    }
    db::update(ctx, TABLE, id, data).await?;
    Ok(())
}

/// The newest [`BuildStatus::Valid`] build for `artifact_sha256`, if any.
///
/// This is how a block already in the active set gets its `BlockInfo` back:
/// a generation's block manifest carries the artifact hash and the routes but
/// not the endpoints, and the duplicate-agent-tool rule needs the endpoints.
/// Filtered in the query rather than by paging `list_recent` because the
/// answer must not depend on how many builds have happened since.
pub async fn latest_valid_for_artifact(
    ctx: &dyn Context,
    artifact_sha256: &str,
) -> Result<Option<BuildRow>, WaferError> {
    let list = db::list(
        ctx,
        TABLE,
        &ListOptions {
            filters: vec![
                Filter {
                    field: "artifact_sha256".into(),
                    operator: FilterOp::Equal,
                    value: serde_json::json!(artifact_sha256),
                },
                status_is(BuildStatus::Valid),
            ],
            // With `limit: 1` the tiebreaker is not cosmetic: two valid builds
            // of one artifact stamped in the same millisecond would otherwise
            // make "the latest" an arbitrary choice between them, and the
            // duplicate-agent-tool rule reads the `BlockInfo` off whichever
            // row it lands on.
            sort: newest_first(),
            limit: Some(1),
            skip_count: true,
            ..Default::default()
        },
    )
    .await?;
    list.records.first().map(decode).transpose()
}

/// The `limit` newest builds, newest first.
///
/// `id` breaks a `created_at` tie (the table's primary key, which the
/// database appends to a sorted list over a table that has one), for the
/// reason
/// [`super::generations::list_recent`] states: the timestamp is
/// millisecond-resolution on wasm32, and two rows sharing one would otherwise
/// come back in whatever order the backend chose.
pub async fn list_recent(ctx: &dyn Context, limit: u32) -> Result<Vec<BuildRow>, WaferError> {
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

/// Every build whose compile has not reached a generation yet, newest first.
///
/// These are the garbage collector's artifact roots outside the ledger:
/// staging inserts the row before it stores the bytes and leaves it
/// [`BuildStatus::Staged`] until the activation it asks for has minted a
/// generation, so a staged row means "these bytes are on their way to a
/// manifest, do not collect them".
///
/// A *status*, not an age window. A time-based rule ("younger than the oldest
/// retained generation") looks equivalent and is not: a browser compile takes
/// tens of seconds, and twenty site writes during one would push its build
/// past the boundary and collect the artifact out from under it.
///
/// Capped at [`MAX_LIST_LIMIT`], which is safe because the set is bounded:
/// staged rows are terminal only while a compile is running, and
/// [`retire_in_flight`] retires the ones a crash left behind on the next boot.
pub async fn list_in_flight(ctx: &dyn Context) -> Result<Vec<BuildRow>, WaferError> {
    let list = db::list(
        ctx,
        TABLE,
        &ListOptions {
            filters: vec![status_is(BuildStatus::Staged)],
            sort: newest_first(),
            limit: Some(MAX_LIST_LIMIT),
            skip_count: true,
            ..Default::default()
        },
    )
    .await?;
    list.records.iter().map(decode).collect()
}

/// Whether any in-flight build names `artifact_sha256`.
///
/// The collector's last look before it deletes. Its root set is read once, and
/// a stage that inserted its row after that read would not be in it — this
/// re-asks the question against the row that would have to exist, immediately
/// before the object goes. One query per *deleted* artifact, which is a rare
/// event.
pub async fn is_in_flight_for_artifact(
    ctx: &dyn Context,
    artifact_sha256: &str,
) -> Result<bool, WaferError> {
    let list = db::list(
        ctx,
        TABLE,
        &ListOptions {
            filters: vec![
                Filter {
                    field: "artifact_sha256".into(),
                    operator: FilterOp::Equal,
                    value: serde_json::json!(artifact_sha256),
                },
                status_is(BuildStatus::Staged),
            ],
            limit: Some(1),
            skip_count: true,
            ..Default::default()
        },
    )
    .await?;
    Ok(!list.records.is_empty())
}

/// What one boot's pass over the in-flight builds did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RetiredBuilds {
    /// Rows accepted, because a manifest the journal vouches for names the
    /// artifact they staged.
    pub promoted: usize,
    /// Rows closed, because nothing does.
    pub retired: usize,
}

/// Settle every in-flight build against the manifests the journal vouches for.
///
/// For boot. A staged row means "a compile is running", and nothing is running
/// on a process that has just started — so each one is the wreckage of a stage
/// the last process did not finish, and each one has to be settled one way or
/// the other. Left staged they would pin their artifacts against the workspace
/// quota for the life of the instance, because the collector reads a staged row
/// as a promise that a generation is coming.
///
/// Which way depends on `vouched_artifacts`: the artifact hashes named by the
/// generation that is serving and by the one the journal is converging on. A
/// staged row whose artifact is in that set is a compile that **got there** —
/// the process died between its activation committing and the row being
/// accepted, which is one `set_status` apart — so it is promoted to
/// [`BuildStatus::Valid`], keeping the `BlockInfo` the stage recorded before
/// the activation. That is not cosmetic: the row is where
/// [`latest_valid_for_artifact`] finds the endpoints of a block in the active
/// set, and without it every later stage of *another* block is refused for a
/// collision check it cannot run.
///
/// Everything else is closed as [`BuildStatus::Invalid`] with
/// `diagnostics_json`, which is also what unpins its artifact.
pub async fn retire_in_flight(
    ctx: &dyn Context,
    vouched_artifacts: &BTreeSet<String>,
    diagnostics_json: &str,
) -> Result<RetiredBuilds, WaferError> {
    let mut settled = RetiredBuilds::default();
    for row in list_in_flight(ctx).await? {
        if vouched_artifacts.contains(&row.artifact_sha256) {
            set_status(ctx, &row.id, BuildStatus::Valid, None, None).await?;
            settled.promoted += 1;
        } else {
            set_status(
                ctx,
                &row.id,
                BuildStatus::Invalid,
                Some(diagnostics_json),
                None,
            )
            .await?;
            settled.retired += 1;
        }
    }
    Ok(settled)
}

/// Every artifact this table indexes, as `sha256 -> stored bytes`.
///
/// The size of the artifact store, answered from the ledger rather than by
/// walking the folder: `dev_status` is polled every ~300 ms while a tool call
/// is outstanding, and a storage `list` is `O(folder)` on the OPFS backend the
/// sandbox actually runs on. The table tracks the store because both ends are
/// maintained together — a row is written before its bytes are stored, and the
/// collector deletes a row with the artifact it names.
///
/// Deduplicated by hash: two compiles that produced identical bytes are two
/// rows and one stored object. Only the two columns it needs are selected, so
/// the poll does not drag the `BlockInfo` and diagnostics JSON of every build
/// across with it, and it pages until the table is exhausted rather than
/// stopping at [`MAX_LIST_LIMIT`] — an under-reported total would look exactly
/// like a collector that had run.
pub async fn artifact_index(ctx: &dyn Context) -> Result<BTreeMap<String, u64>, WaferError> {
    let mut index = BTreeMap::new();
    let mut offset = 0i64;
    loop {
        let list = db::list(
            ctx,
            TABLE,
            &ListOptions {
                columns: Some(vec!["artifact_sha256".into(), "artifact_bytes".into()]),
                sort: vec![SortField {
                    field: "artifact_sha256".into(),
                    desc: false,
                }],
                limit: Some(MAX_LIST_LIMIT),
                offset,
                skip_count: true,
                ..Default::default()
            },
        )
        .await?;
        let count = list.records.len() as i64;
        for record in &list.records {
            index.insert(
                record.str_field("artifact_sha256").to_string(),
                record.u64_field("artifact_bytes"),
            );
        }
        if count < i64::from(MAX_LIST_LIMIT) {
            return Ok(index);
        }
        offset += count;
    }
}

/// A settled build row, as the collector needs it: which row, naming which
/// artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettledRow {
    /// The row's id.
    pub id: String,
    /// The artifact it names.
    pub artifact_sha256: String,
}

/// Every row that is not [`BuildStatus::Staged`], as `(id, artifact)`.
///
/// For the collector's stale-row pass (`super::super::gc`), which needs to
/// know which rows promise an artifact is already stored. Only the two columns
/// it reads are selected, and it pages until the table is exhausted: a row
/// past a page boundary would be a stale row nothing ever drops.
pub async fn list_settled(ctx: &dyn Context) -> Result<Vec<SettledRow>, WaferError> {
    let mut rows = Vec::new();
    let mut offset = 0i64;
    loop {
        let list = db::list(
            ctx,
            TABLE,
            &ListOptions {
                columns: Some(vec!["id".into(), "artifact_sha256".into()]),
                filters: vec![Filter {
                    field: "status".into(),
                    operator: FilterOp::NotEqual,
                    value: serde_json::json!(BuildStatus::Staged.as_str()),
                }],
                sort: vec![SortField {
                    field: "id".into(),
                    desc: false,
                }],
                limit: Some(MAX_LIST_LIMIT),
                offset,
                skip_count: true,
                ..Default::default()
            },
        )
        .await?;
        let count = list.records.len() as i64;
        rows.extend(list.records.iter().map(|record| SettledRow {
            id: record.id.clone(),
            artifact_sha256: record.str_field("artifact_sha256").to_string(),
        }));
        if count < i64::from(MAX_LIST_LIMIT) {
            return Ok(rows);
        }
        offset += count;
    }
}

/// Delete one build row.
///
/// Two callers, and both are undoing something: staging drops the row it
/// inserted when the artifact it describes could not be stored, and the
/// collector drops settled rows whose artifact is no longer stored.
pub async fn delete(ctx: &dyn Context, id: &str) -> Result<(), WaferError> {
    db::delete(ctx, TABLE, id).await
}

/// A `status = …` filter.
fn status_is(status: BuildStatus) -> Filter {
    Filter {
        field: "status".into(),
        operator: FilterOp::Equal,
        value: serde_json::json!(status.as_str()),
    }
}

/// Delete every build row naming `artifact_sha256`.
///
/// For the garbage collector, and only after the artifact itself is gone: a
/// row that says an artifact was accepted, for bytes the store no longer
/// holds, is a claim [`latest_valid_for_artifact`] would hand back to the
/// duplicate-tool check as if the block were still loadable.
pub async fn delete_for_artifact(
    ctx: &dyn Context,
    artifact_sha256: &str,
) -> Result<(), WaferError> {
    db::delete_by_filters(
        ctx,
        TABLE,
        vec![Filter {
            field: "artifact_sha256".into(),
            operator: FilterOp::Equal,
            value: serde_json::json!(artifact_sha256),
        }],
    )
    .await
}

/// Newest first — the ordering every listing here uses. The database breaks
/// a `created_at` tie on the primary key, `id`, descending — a sorted `list`
/// ends its `ORDER BY` with the table's key, and this table has one
/// (`id TEXT PRIMARY KEY`, `001_dev_schema.sqlite.sql`) — for the reason
/// [`list_recent`] states.
fn newest_first() -> Vec<SortField> {
    vec![SortField {
        field: "created_at".into(),
        desc: true,
    }]
}

/// Decode a stored row. An unrecognized `status` is an error, never a default.
fn decode(record: &db::Record) -> Result<BuildRow, WaferError> {
    let status_text = record.str_field("status");
    Ok(BuildRow {
        id: record.str_field("id").to_string(),
        block_name: record.str_field("block_name").to_string(),
        source_manifest_sha256: record.str_field("source_manifest_sha256").to_string(),
        artifact_sha256: record.str_field("artifact_sha256").to_string(),
        block_info_json: super::json_text(record, "block_info_json"),
        diagnostics_json: super::json_text(record, "diagnostics_json"),
        compiler_version: record.str_field("compiler_version").to_string(),
        artifact_bytes: record.u64_field("artifact_bytes"),
        status: BuildStatus::parse(status_text).ok_or_else(|| {
            WaferError::new(
                ErrorCode::Internal,
                format!("{TABLE}: unknown status `{status_text}`"),
            )
        })?,
        created_at: record.str_field("created_at").to_string(),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::{blocks::dev::test_support::FakeControl, test_support::TestContext};

    /// JSON-shaped columns in canonical form — same round-trip hazard as the
    /// generation manifests (see `repo::json_text`).
    const BLOCK_INFO: &str = r#"{"name":"site/newsletter","version":"0.1.0"}"#;
    const DIAGNOSTICS: &str = r#"[{"level":"warning","message":"unused import"}]"#;

    fn new_build(block_name: &str) -> NewBuild {
        NewBuild {
            block_name: block_name.to_string(),
            source_manifest_sha256: "src".to_string(),
            artifact_sha256: "art".to_string(),
            block_info_json: BLOCK_INFO.to_string(),
            diagnostics_json: DIAGNOSTICS.to_string(),
            compiler_version: "rubrc@pinned".to_string(),
            artifact_bytes: 128,
        }
    }

    #[tokio::test]
    async fn insert_then_get_round_trips_every_column() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        let inserted = insert(&ctx, &new_build("site/newsletter"))
            .await
            .expect("insert");
        assert_eq!(inserted.status, BuildStatus::Staged);

        let read_back = get(&ctx, &inserted.id).await.expect("get");
        assert_eq!(read_back, inserted);
        assert_eq!(read_back.block_info_json, BLOCK_INFO);
        assert_eq!(read_back.diagnostics_json, DIAGNOSTICS);
        assert_eq!(read_back.compiler_version, "rubrc@pinned");
        assert_eq!(read_back.artifact_bytes, 128);
    }

    #[tokio::test]
    async fn set_status_replaces_diagnostics_only_when_given_them() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        let row = insert(&ctx, &new_build("site/newsletter"))
            .await
            .expect("insert");

        // Accepting a build must not wipe the warnings the compile produced.
        set_status(&ctx, &row.id, BuildStatus::Valid, None, None)
            .await
            .expect("accept");
        let valid = get(&ctx, &row.id).await.expect("get");
        assert_eq!(valid.status, BuildStatus::Valid);
        assert_eq!(valid.diagnostics_json, DIAGNOSTICS);

        let refusal = r#"[{"level":"error","message":"probe trapped"}]"#;
        set_status(&ctx, &row.id, BuildStatus::Invalid, Some(refusal), None)
            .await
            .expect("refuse");
        let invalid = get(&ctx, &row.id).await.expect("get");
        assert_eq!(invalid.status, BuildStatus::Invalid);
        assert_eq!(invalid.diagnostics_json, refusal);
    }

    #[tokio::test]
    async fn list_recent_is_newest_first_and_honours_the_limit() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        let mut rows = Vec::new();
        for name in ["site/a", "site/b", "site/c"] {
            rows.push(insert(&ctx, &new_build(name)).await.expect("insert"));
        }
        assert!(
            rows[0].created_at < rows[1].created_at && rows[1].created_at < rows[2].created_at,
            "insert must produce strictly increasing created_at: {:?}",
            rows.iter().map(|r| &r.created_at).collect::<Vec<_>>(),
        );

        let listed = list_recent(&ctx, 10).await.expect("list");
        assert_eq!(
            listed
                .iter()
                .map(|r| r.block_name.as_str())
                .collect::<Vec<_>>(),
            vec!["site/c", "site/b", "site/a"],
        );
        assert_eq!(list_recent(&ctx, 1).await.expect("list").len(), 1);
    }

    #[tokio::test]
    async fn the_reported_block_info_is_written_by_the_call_that_accepts_the_build() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        let row = insert(&ctx, &new_build("site/newsletter"))
            .await
            .expect("insert");
        // Nothing is filed under the artifact until a build is accepted.
        assert_eq!(
            latest_valid_for_artifact(&ctx, "art")
                .await
                .expect("lookup"),
            None,
        );

        let reported = r#"{"name":"site/newsletter","version":"0.2.0"}"#;
        set_status(&ctx, &row.id, BuildStatus::Valid, None, Some(reported))
            .await
            .expect("accept");
        let found = latest_valid_for_artifact(&ctx, "art")
            .await
            .expect("lookup")
            .expect("a valid build");
        assert_eq!(found.id, row.id);
        assert_eq!(found.block_info_json, reported);
        // A different artifact is a different question.
        assert_eq!(
            latest_valid_for_artifact(&ctx, "other")
                .await
                .expect("lookup"),
            None,
        );
    }

    #[test]
    fn only_a_staged_build_is_in_flight() {
        assert!(BuildStatus::Staged.is_in_flight());
        assert!(!BuildStatus::Valid.is_in_flight());
        assert!(!BuildStatus::Invalid.is_in_flight());
    }

    /// The collector's roots outside the ledger are exactly the staged rows —
    /// a status, never an age. A build that has been accepted or refused is
    /// over, and its artifact's fate belongs to the generations.
    #[tokio::test]
    async fn list_in_flight_is_the_staged_rows_and_nothing_else() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        let mut rows = Vec::new();
        for name in ["site/a", "site/b", "site/c"] {
            rows.push(insert(&ctx, &new_build(name)).await.expect("insert"));
        }
        assert_eq!(list_in_flight(&ctx).await.expect("list").len(), 3);

        set_status(&ctx, &rows[0].id, BuildStatus::Valid, None, None)
            .await
            .expect("accept");
        set_status(&ctx, &rows[1].id, BuildStatus::Invalid, None, None)
            .await
            .expect("refuse");
        assert_eq!(
            list_in_flight(&ctx)
                .await
                .expect("list")
                .iter()
                .map(|r| r.block_name.as_str())
                .collect::<Vec<_>>(),
            vec!["site/c"],
        );

        assert!(is_in_flight_for_artifact(&ctx, "art").await.expect("ask"));
        assert!(!is_in_flight_for_artifact(&ctx, "other").await.expect("ask"));
        set_status(&ctx, &rows[2].id, BuildStatus::Valid, None, None)
            .await
            .expect("accept");
        assert!(!is_in_flight_for_artifact(&ctx, "art").await.expect("ask"));
    }

    /// A staged row on a process that has just started is the wreckage of an
    /// unfinished compile, and left alone it pins its artifact forever — but
    /// only the rows no live manifest vouches for. One whose artifact IS named
    /// by a manifest got where it was going; the process merely died one
    /// `set_status` short of saying so.
    #[tokio::test]
    async fn retire_in_flight_settles_each_staged_row_against_the_vouched_set() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        let orphan = insert(&ctx, &new_build("site/a")).await.expect("insert");
        let mut landed = new_build("site/b");
        landed.artifact_sha256 = "vouched".to_string();
        let landed = insert(&ctx, &landed).await.expect("insert");
        let accepted = insert(&ctx, &new_build("site/c")).await.expect("insert");
        set_status(&ctx, &accepted.id, BuildStatus::Valid, None, None)
            .await
            .expect("accept");

        let reason = r#"[{"level":"error","message":"abandoned at boot"}]"#;
        let vouched = BTreeSet::from(["vouched".to_string()]);
        assert_eq!(
            retire_in_flight(&ctx, &vouched, reason)
                .await
                .expect("retire"),
            RetiredBuilds {
                promoted: 1,
                retired: 1
            },
        );

        let retired = get(&ctx, &orphan.id).await.expect("get");
        assert_eq!(retired.status, BuildStatus::Invalid);
        assert_eq!(retired.diagnostics_json, reason);

        // Promoted, with the `BlockInfo` the stage recorded before its
        // activation — which is the whole reason to promote rather than close.
        let promoted = get(&ctx, &landed.id).await.expect("get");
        assert_eq!(promoted.status, BuildStatus::Valid);
        assert_eq!(promoted.block_info_json, BLOCK_INFO);
        assert_eq!(promoted.diagnostics_json, DIAGNOSTICS);
        assert_eq!(
            latest_valid_for_artifact(&ctx, "vouched")
                .await
                .expect("lookup")
                .map(|row| row.id),
            Some(promoted.id),
        );

        // An already-accepted build is untouched, diagnostics included.
        let untouched = get(&ctx, &accepted.id).await.expect("get");
        assert_eq!(untouched.status, BuildStatus::Valid);
        assert_eq!(untouched.diagnostics_json, DIAGNOSTICS);
        // Idempotent: nothing is left in flight.
        assert_eq!(
            retire_in_flight(&ctx, &vouched, reason)
                .await
                .expect("retire"),
            RetiredBuilds::default(),
        );
    }

    /// `dev_status` reports the artifact store from this index, so it has to
    /// count stored *objects* rather than rows, and it must not stop at a
    /// page boundary.
    #[tokio::test]
    async fn the_artifact_index_deduplicates_by_hash_and_pages_to_the_end() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        // Two rows, one artifact: a re-stage of identical bytes.
        insert(&ctx, &new_build("site/a")).await.expect("insert");
        insert(&ctx, &new_build("site/a")).await.expect("insert");
        let mut other = new_build("site/b");
        other.artifact_sha256 = "other".to_string();
        other.artifact_bytes = 7;
        insert(&ctx, &other).await.expect("insert");

        let index = artifact_index(&ctx).await.expect("index");
        assert_eq!(index.len(), 2, "two stored objects, three rows");
        assert_eq!(index.get("art"), Some(&128));
        assert_eq!(index.get("other"), Some(&7));
        assert_eq!(index.values().sum::<u64>(), 135);
    }

    /// The page cap is not the answer's ceiling: 250 rows against a 200-row
    /// page still counts every artifact.
    #[tokio::test]
    async fn the_artifact_index_is_not_bounded_by_one_page() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        for i in 0..250 {
            let mut build = new_build("site/a");
            build.artifact_sha256 = format!("art-{i:04}");
            build.artifact_bytes = 1;
            insert(&ctx, &build).await.expect("insert");
        }
        let index = artifact_index(&ctx).await.expect("index");
        assert_eq!(index.len(), 250);
        assert_eq!(index.values().sum::<u64>(), 250);
    }

    #[tokio::test]
    async fn delete_removes_one_row() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        let row = insert(&ctx, &new_build("site/a")).await.expect("insert");
        delete(&ctx, &row.id).await.expect("delete");
        assert_eq!(
            get(&ctx, &row.id).await.expect_err("gone").code,
            ErrorCode::NotFound
        );
    }

    #[tokio::test]
    async fn delete_for_artifact_removes_every_row_naming_it() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        // Two rows for one artifact — a re-stage of identical bytes — plus one
        // for different bytes, which must survive.
        let first = insert(&ctx, &new_build("site/a")).await.expect("insert");
        let second = insert(&ctx, &new_build("site/b")).await.expect("insert");
        let mut other = new_build("site/c");
        other.artifact_sha256 = "kept".to_string();
        let other = insert(&ctx, &other).await.expect("insert");

        delete_for_artifact(&ctx, "art").await.expect("delete");
        for gone in [&first, &second] {
            assert_eq!(
                get(&ctx, &gone.id).await.expect_err("gone").code,
                ErrorCode::NotFound
            );
        }
        assert_eq!(get(&ctx, &other.id).await.expect("kept").id, other.id);

        // Idempotent: collecting an artifact whose rows are already gone is
        // not an error.
        delete_for_artifact(&ctx, "art")
            .await
            .expect("delete again");
    }

    #[test]
    fn decode_refuses_an_unknown_status() {
        let mut data = HashMap::new();
        data.insert("id".to_string(), serde_json::json!("b1"));
        data.insert("block_name".to_string(), serde_json::json!("site/x"));
        data.insert(
            "source_manifest_sha256".to_string(),
            serde_json::json!("src"),
        );
        data.insert("artifact_sha256".to_string(), serde_json::json!("art"));
        data.insert("block_info_json".to_string(), serde_json::json!("null"));
        data.insert("diagnostics_json".to_string(), serde_json::json!("[]"));
        data.insert("compiler_version".to_string(), serde_json::json!("v"));
        data.insert("artifact_bytes".to_string(), serde_json::json!(0));
        data.insert("status".to_string(), serde_json::json!("probably_fine"));
        data.insert("created_at".to_string(), serde_json::json!("1970-01-01"));
        let record = db::Record {
            id: "b1".to_string(),
            data,
        };

        let err = decode(&record).expect_err("unknown status must fail");
        assert_eq!(err.code, ErrorCode::Internal);
        assert!(err.message.contains("probably_fine"), "{err}");
    }
}
