//! `impresspress__admin__wrap_grants`: admin-created WRAP grants — the rows
//! the permissions page manages, loaded at every runtime build on top of the
//! grants the blocks declare in code.
//!
//! Two callers, one codec (spec 2.1.2). The boot flavour ([`load`]) runs
//! over [`DatabaseService`] before WRAP exists — it is what the runtime's
//! grant list is built from, so it can hardly run under it; the runtime
//! flavour ([`list`], [`create`], [`delete`]) is the admin block's
//! permissions surface under WRAP over [`Context`]. Both go through
//! [`WrapGrantRow::from_record`] / [`WrapGrantRow::to_data`], and
//! [`WrapGrantRow::into_resource_grant`] is the one place a stored row
//! becomes a [`ResourceGrant`].

use std::{collections::HashMap, sync::Arc};

use serde_json::{json, Value};
use wafer_block::{db::ListOptions, GrantWrite};
use wafer_core::{
    clients::database::{self as db, RecordData},
    interfaces::database::service::{DatabaseError, DatabaseService},
};
use wafer_run::{context::Context, ErrorCode, ResourceGrant, ResourceType, WaferError};

use crate::{
    db_read::{self, Bound},
    util::RecordExt,
};

pub const TABLE: &str = "impresspress__admin__wrap_grants";

/// One row of the wrap_grants table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrapGrantRow {
    pub id: String,
    /// The block id being granted access, or `*` for every block.
    pub grantee: String,
    /// The table, storage path or other resource pattern being granted.
    pub resource: String,
    /// Stored as two integer flags: `write` (migration 001) set for a
    /// read-write grant, `append` (migration 005) set for an append-only one,
    /// neither for read-only. See [`encode_access`].
    pub write: GrantWrite,
    /// The stored wire value of the grant's [`ResourceType`] (its lowercase
    /// `Display` form: `db`, `config`, …); empty is the all-types wildcard.
    pub resource_type: String,
    pub description: String,
    pub created_at: String,
    pub updated_at: String,
}

impl WrapGrantRow {
    /// Decode one row. `grantee`, `resource` and `write` are required (all
    /// `NOT NULL`); a row without them is not a grant and is refused rather
    /// than defaulted, so a malformed row can never widen access. The access
    /// columns are decoded by [`decode_access`]; a value it does not
    /// recognise, or a row that sets both flags, refuses the row too.
    pub fn from_record(id: &str, data: &RecordData) -> Result<Self, String> {
        let grantee = data
            .opt_str_field("grantee")
            .ok_or_else(|| format!("{TABLE} row `{id}` has no grantee"))?;
        let resource = data
            .opt_str_field("resource")
            .ok_or_else(|| format!("{TABLE} row `{id}` has no resource"))?;
        let write = decode_access(data).map_err(|e| format!("{TABLE} row `{id}` {e}"))?;
        Ok(Self {
            id: id.to_string(),
            grantee,
            resource,
            write,
            resource_type: data.str_field("resource_type").to_string(),
            description: data.str_field("description").to_string(),
            created_at: data.str_field("created_at").to_string(),
            updated_at: data.str_field("updated_at").to_string(),
        })
    }

    /// The column map this row inserts as.
    pub fn to_data(&self) -> HashMap<String, Value> {
        let mut data = HashMap::new();
        data.insert("id".to_string(), json!(self.id));
        data.insert("grantee".to_string(), json!(self.grantee));
        data.insert("resource".to_string(), json!(self.resource));
        let (write, append) = encode_access(self.write);
        data.insert(WRITE_COLUMN.to_string(), json!(write));
        data.insert(APPEND_COLUMN.to_string(), json!(append));
        data.insert("resource_type".to_string(), json!(self.resource_type));
        data.insert("description".to_string(), json!(self.description));
        data.insert("created_at".to_string(), json!(self.created_at));
        data.insert("updated_at".to_string(), json!(self.updated_at));
        data
    }

    /// The runtime grant this row declares. An empty `resource_type` is the
    /// intentional all-types wildcard; a non-empty unrecognized value is a
    /// typo'd grant and is an error (fail-closed) rather than widened to the
    /// wildcard. So is a grant the runtime would refuse to install
    /// ([`ResourceGrant::check_shape`]: an append grant not typed `db`) —
    /// `Wafer::add_wrap_grants` rejects the whole set it is handed when one
    /// grant fails that check, so one bad row must not reach it.
    pub fn into_resource_grant(self) -> Result<ResourceGrant, String> {
        let resource_type = ResourceType::parse_stored(Some(&self.resource_type))
            .map_err(|e| format!("{TABLE} row `{}`: {e}", self.id))?;
        let grant = ResourceGrant {
            grantee: self.grantee,
            resource: self.resource,
            write: self.write,
            resource_type,
        };
        grant
            .check_shape()
            .map_err(|e| format!("{TABLE} row `{}`: {e}", self.id))?;
        Ok(grant)
    }
}

/// Set for a read-write grant.
const WRITE_COLUMN: &str = "write";
/// Set for an append-only grant (migration 005).
const APPEND_COLUMN: &str = "append";

/// The `(write, append)` flags a grant is stored as.
///
/// Append-only has a column of its own rather than a third `write` value
/// because a binary built before append grants existed reads `write` as a
/// flag, any non-zero value meaning read-write. A row stored this way reads as
/// read-only to such a binary, which ignores `append`, so rolling back past
/// this release narrows an append grant instead of widening it.
fn encode_access(write: GrantWrite) -> (i64, i64) {
    match write {
        GrantWrite::None => (0, 0),
        GrantWrite::Full => (1, 0),
        GrantWrite::Append => (0, 1),
    }
}

/// The stored access columns as a [`GrantWrite`]. `write` is required;
/// `append` reads as unset when it is absent or `NULL`, which is how a row
/// looks where migration 005 has not run yet (`/_deploy/init` or
/// `/_deploy/prepare` building its runtime before the funnel migrates) — and
/// treating it as unset never grants more than `write` says.
/// A row that sets both flags is refused: it names two different accesses,
/// and neither is a safe guess.
fn decode_access(data: &RecordData) -> Result<GrantWrite, String> {
    let Some(write) = data.get(WRITE_COLUMN) else {
        return Err("has no write column".to_string());
    };
    let write =
        decode_flag(write).ok_or_else(|| format!("has an unrecognised write value {write}"))?;
    let append = match data.get(APPEND_COLUMN) {
        None | Some(Value::Null) => false,
        Some(append) => decode_flag(append)
            .ok_or_else(|| format!("has an unrecognised append value {append}"))?,
    };
    match (write, append) {
        (false, false) => Ok(GrantWrite::None),
        (true, false) => Ok(GrantWrite::Full),
        (false, true) => Ok(GrantWrite::Append),
        (true, true) => Err("sets both write and append".to_string()),
    }
}

/// One stored access flag: the integer `0`/`1` SQLite and Postgres store, a
/// bool, or either spelled as a string by a hand-built fixture. `None` for
/// anything else — `write = 2` included — so the caller refuses the row
/// rather than guess which access it meant.
fn decode_flag(value: &Value) -> Option<bool> {
    match value {
        Value::Number(n) => match n.as_i64()? {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        },
        Value::Bool(b) => Some(*b),
        Value::String(s) => match s.as_str() {
            "0" | "false" => Some(false),
            "1" | "true" => Some(true),
            _ => None,
        },
        _ => None,
    }
}

/// A grant to insert, as the permissions page's form supplies it.
#[derive(Debug, Clone)]
pub struct NewWrapGrant {
    pub grantee: String,
    pub resource: String,
    pub write: GrantWrite,
    pub resource_type: String,
    pub description: String,
}

impl NewWrapGrant {
    /// The row this becomes: a synthesised `wg_<uuid>` id and both
    /// timestamps set to now.
    pub fn into_row(self) -> WrapGrantRow {
        let now = crate::util::now_rfc3339();
        WrapGrantRow {
            id: format!("wg_{}", uuid::Uuid::new_v4()),
            grantee: self.grantee,
            resource: self.resource,
            write: self.write,
            resource_type: self.resource_type,
            description: self.description,
            created_at: now.clone(),
            updated_at: now,
        }
    }
}

fn decode_error(e: String) -> WaferError {
    WaferError::new(ErrorCode::Internal, e)
}

// ---------------------------------------------------------------------------
// Boot flavour: over `DatabaseService`, before WRAP.
// ---------------------------------------------------------------------------

/// Load the admin-created grants for a runtime build.
///
/// DB-service reader shared by every target (the native CLI, the Cloudflare
/// adapter's per-isolate cache and its `/_deploy/*` funnel, the browser), so
/// dynamic grants are injected the same way everywhere. A row that does not
/// decode or names an unknown resource type is warned about and dropped,
/// never widened.
///
/// # A failed read is an error, never "no grants"
///
/// A runtime built without the admin-created grants denies every access they
/// exist to allow, and nothing about it looks broken: it serves, it is cached
/// as healthy, and the blocks those grants were for fail one request at a
/// time. So a read that could not answer is the caller's `Err` to act on —
/// the boot funnel aborts or reports it by policy (`builder::boot`), native
/// refuses to start, and `/_deploy/prepare` refuses to seal a plan.
///
/// # The missing table is this function's own problem
///
/// A first-ever deploy reads this table BEFORE anything has created it:
/// `/_deploy/init` and `/_deploy/prepare` build their runtime, and only then
/// do the blocks' `Init`s migrate. So "table absent" is an ordinary cold
/// start here and answers no grants. The executor has no table-existence
/// guard — a `list` of a missing table runs and the backend answers "no such
/// table" — so the tolerance is this function's, through
/// [`DatabaseService::schema_table_exists`].
///
/// It lists first and asks only after the list failed, the same order as
/// [`crate::platform_state::block_settings::load`]: on a healthy database
/// the table is there, the list succeeds and the probe never runs. On D1
/// that probe is a `sqlite_master` scan of every schema row up to this
/// table, which admin's migration creates last — about thirty rows on every
/// database-built runtime when it ran first. Only a definite "absent" turns
/// a failed list into no grants; a probe that also failed, or that found the
/// table, is no evidence the failure was the cold start, so the list's own
/// error is returned.
pub async fn load(db: &Arc<dyn DatabaseService>) -> Result<Vec<ResourceGrant>, DatabaseError> {
    let opts = ListOptions {
        limit: Some(10_000),
        skip_count: true,
        ..Default::default()
    };
    let rows = match db.list(TABLE, &opts).await {
        Ok(list) => list.records,
        Err(e) => {
            if let Ok(false) = db.schema_table_exists(TABLE).await {
                tracing::debug!(
                    "wrap_grants table absent (fresh boot before migrations); no dynamic grants"
                );
                return Ok(Vec::new());
            }
            tracing::error!(
                error = %e,
                "wrap_grants list failed (operational error, not a missing table); \
                 refusing to build a runtime without its admin-created grants"
            );
            return Err(e);
        }
    };
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            WrapGrantRow::from_record(&r.id, &r.data)
                .and_then(WrapGrantRow::into_resource_grant)
                .map_err(|e| tracing::warn!(error = %e, "wrap_grants row dropped"))
                .ok()
        })
        .collect())
}

/// The resource one test fixture grants over: a real table name on the wire,
/// which is why it is spelled here and not in the tests that use it.
#[cfg(test)]
pub(crate) const FIXTURE_RESOURCE: &str = "impresspress__files__objects";

/// Test-only: insert one admin-created grant over [`FIXTURE_RESOURCE`], the
/// row [`load`] is then expected to return. The caller applies the admin DDL
/// first.
///
/// It lives in this module rather than in each test that needs a seeded grant
/// because this module owns both the table name and that resource literal, and
/// `tests/repo_door.rs` is the gate that keeps it that way: the same three
/// lines written in `builder/boot.rs` would have to spell [`TABLE`] there,
/// which is exactly the bypass the door exists to catch.
#[cfg(test)]
pub(crate) async fn seed_fixture_grant(db: &Arc<dyn DatabaseService>) {
    let row = NewWrapGrant {
        grantee: "impresspress/files".to_string(),
        resource: FIXTURE_RESOURCE.to_string(),
        write: GrantWrite::Full,
        resource_type: "db".to_string(),
        description: String::new(),
    }
    .into_row();
    db.create(TABLE, row.to_data())
        .await
        .expect("seed fixture grant");
}

/// Test-only: a migrated database holding the [`seed_fixture_grant`] row,
/// whose every statement naming [`TABLE`] then fails as a D1 outage does
/// ([`Fault::Unavailable`](crate::test_support::d1_shaped::Fault)) — the
/// failed read [`load`]'s callers must treat as an error.
///
/// Here rather than in each caller's tests for the reason
/// [`seed_fixture_grant`] is: the fault switch names the table.
#[cfg(test)]
pub(crate) async fn fixture_with_failing_reads() -> Arc<dyn DatabaseService> {
    use crate::test_support::d1_shaped::{D1Shaped, Fault, Ledger};
    let shaped = Arc::new(D1Shaped::new(
        wafer_block_sqlite::service::SQLiteDatabaseService::open_in_memory()
            .expect("open in-memory sqlite"),
        Arc::new(Ledger::default()),
    ));
    let db: Arc<dyn DatabaseService> = shaped.clone();
    crate::test_support::migrate_sqlite(
        &db,
        crate::blocks::admin::ADMIN_BLOCK_ID,
        crate::blocks::admin::migrations::SQLITE_MIGRATIONS,
    )
    .await
    .expect("apply admin migrations");
    seed_fixture_grant(&db).await;
    load(&db)
        .await
        .expect("the fixture database reads before its fault is switched on");
    shaped.fail_table(TABLE, Fault::Unavailable);
    db
}

// ---------------------------------------------------------------------------
// Runtime flavour: over `Context`, under WRAP.
// ---------------------------------------------------------------------------

/// Every row, for the permissions page. A row that does not decode is an
/// error here rather than silently omitted: the page renders these as the
/// custom grants in force, and a listing that hides a row it cannot read
/// would misstate what the runtime loaded.
pub async fn list(ctx: &dyn Context) -> Result<Vec<WrapGrantRow>, WaferError> {
    let records = db_read::list_bounded(
        ctx,
        TABLE,
        vec![],
        Bound::Curated("custom WRAP grants are created by an operator in the admin UI"),
    )
    .await?;
    records
        .iter()
        .map(|r| WrapGrantRow::from_record(&r.id, &r.data).map_err(decode_error))
        .collect()
}

/// Insert a new grant and return it as stored.
pub async fn create(ctx: &dyn Context, new: NewWrapGrant) -> Result<WrapGrantRow, WaferError> {
    let row = new.into_row();
    let rec = db::create(ctx, TABLE, row.to_data()).await?;
    WrapGrantRow::from_record(&rec.id, &rec.data).map_err(decode_error)
}

/// Delete the grant with `id`. `NotFound` when there is none.
pub async fn delete(ctx: &dyn Context, id: &str) -> Result<(), WaferError> {
    db::delete(ctx, TABLE, id).await
}
#[cfg(test)]
mod tests {
    use wafer_run::ResourceType;

    use super::*;
    use crate::test_support::{FailingDbOpContext, TestContext};

    fn new_grant(resource_type: &str) -> NewWrapGrant {
        NewWrapGrant {
            grantee: "impresspress/files".to_string(),
            resource: "impresspress__foo__bar".to_string(),
            write: GrantWrite::Full,
            resource_type: resource_type.to_string(),
            description: "probe".to_string(),
        }
    }

    /// The codec: every column `create` writes comes back through `list`
    /// unchanged, `write` as a [`GrantWrite`] from the integer column.
    #[tokio::test]
    async fn create_and_list_round_trip_every_column() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let created = create(&ctx, new_grant("db")).await.expect("create");
        assert!(created.id.starts_with("wg_"), "{}", created.id);
        assert_eq!(created.grantee, "impresspress/files");
        assert_eq!(created.resource, "impresspress__foo__bar");
        assert_eq!(created.write, GrantWrite::Full);
        assert_eq!(created.resource_type, "db");
        assert_eq!(created.description, "probe");
        assert!(!created.created_at.is_empty());
        assert_eq!(created.created_at, created.updated_at);

        let rows = list(&ctx).await.expect("list");
        assert_eq!(rows, vec![created.clone()]);

        let again =
            WrapGrantRow::from_record(&created.id, &created.to_data().into_iter().collect())
                .expect("decode");
        assert_eq!(again, created);

        let grant = created
            .into_resource_grant()
            .expect("a stored `db` type parses");
        assert_eq!(grant.grantee, "impresspress/files");
        assert_eq!(grant.resource, "impresspress__foo__bar");
        assert_eq!(grant.write, GrantWrite::Full);
        assert_eq!(grant.resource_type, Some(ResourceType::Db));
    }

    /// An append-only grant survives the table and becomes an append-only
    /// runtime grant: stored as `write = 0, append = 1`, it cannot collapse
    /// into read-only or read-write on the way through.
    #[tokio::test]
    async fn an_append_grant_round_trips_through_the_table() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let created = create(
            &ctx,
            NewWrapGrant {
                write: GrantWrite::Append,
                ..new_grant("db")
            },
        )
        .await
        .expect("create");
        assert_eq!(created.to_data()[WRITE_COLUMN], serde_json::json!(0));
        assert_eq!(created.to_data()[APPEND_COLUMN], serde_json::json!(1));
        let rows = list(&ctx).await.expect("list");
        assert_eq!(rows[0].write, GrantWrite::Append);
        let grant = rows[0]
            .clone()
            .into_resource_grant()
            .expect("a db append grant");
        assert_eq!(grant.write, GrantWrite::Append);
    }

    /// A binary built before append grants existed decoded `write` with
    /// `RecordExt::bool_field` (any non-zero value is read-write) and has no
    /// notion of `append`. Rolling back to one must not widen an append-only
    /// grant, so the row as the database hands it back has to read as
    /// read-only through that decoder — and as append-only through this one.
    #[tokio::test]
    async fn an_append_row_reads_as_read_only_to_a_binary_without_the_column() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let created = create(
            &ctx,
            NewWrapGrant {
                write: GrantWrite::Append,
                ..new_grant("db")
            },
        )
        .await
        .expect("create");
        let stored = db::get(&ctx, TABLE, &created.id)
            .await
            .expect("read the stored row");
        assert!(
            !stored.data.bool_field(WRITE_COLUMN),
            "an older binary would read this append grant as read-write: {:?}",
            stored.data
        );
        let decoded = WrapGrantRow::from_record(&stored.id, &stored.data).expect("decode");
        assert_eq!(decoded.write, GrantWrite::Append);
    }

    /// An append grant not typed `db` is one the runtime refuses to install,
    /// and `Wafer::add_wrap_grants` refuses the WHOLE set it is handed when
    /// one fails — so the row is refused here, where [`load`] drops it alone.
    #[tokio::test]
    async fn an_append_grant_not_typed_db_is_refused_as_a_row() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        for resource_type in ["", "storage"] {
            let created = create(
                &ctx,
                NewWrapGrant {
                    write: GrantWrite::Append,
                    ..new_grant(resource_type)
                },
            )
            .await
            .expect("create");
            let err = created
                .into_resource_grant()
                .expect_err("append grant not typed db");
            assert!(err.contains("append"), "{err}");
        }
    }

    /// An empty `resource_type` is the intentional all-types wildcard; a
    /// non-empty unrecognized value is a typo'd grant and is refused rather
    /// than widened to the wildcard.
    #[tokio::test]
    async fn resource_type_parses_wildcard_and_refuses_typos() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let wildcard = create(&ctx, new_grant("")).await.expect("create");
        assert_eq!(
            wildcard
                .into_resource_grant()
                .expect("wildcard")
                .resource_type,
            None
        );
        let typo = create(&ctx, new_grant("databsae")).await.expect("create");
        assert!(typo.into_resource_grant().is_err());
    }

    #[tokio::test]
    async fn delete_removes_the_row_and_reports_a_missing_one() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let created = create(&ctx, new_grant("db")).await.expect("create");
        delete(&ctx, &created.id).await.expect("delete");
        assert!(list(&ctx).await.expect("list").is_empty());
        let err = delete(&ctx, &created.id)
            .await
            .expect_err("deleting a gone row is NotFound");
        assert_eq!(err.code, ErrorCode::NotFound);
    }

    /// The listing must not read as "no custom grants" on an outage: the
    /// permissions page renders whatever this returns.
    #[tokio::test]
    async fn list_surfaces_read_errors() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        create(&ctx, new_grant("db")).await.expect("create");
        let failing = FailingDbOpContext::new(ctx, vec![("database.list", TABLE)]);
        assert!(list(&failing).await.is_err());
    }

    fn row_with(columns: &[(&str, Value)]) -> RecordData {
        let mut data = RecordData::new();
        data.insert("grantee".to_string(), serde_json::json!("a/b"));
        data.insert("resource".to_string(), serde_json::json!("a__b__c"));
        for (column, value) in columns {
            data.insert((*column).to_string(), value.clone());
        }
        data
    }

    /// The access flags arrive as integers from the database, or as bools or
    /// strings from a hand-built fixture. `append` may be absent or `NULL`
    /// (a table migration 005 has not reached yet) and then reads as unset.
    #[test]
    fn access_decodes_from_every_backend_shape() {
        use serde_json::json;
        for (write, append, want) in [
            (json!(1), None, GrantWrite::Full),
            (json!(0), None, GrantWrite::None),
            (json!(true), None, GrantWrite::Full),
            (json!(false), None, GrantWrite::None),
            (json!("1"), None, GrantWrite::Full),
            (json!("true"), None, GrantWrite::Full),
            (json!("0"), None, GrantWrite::None),
            (json!(1), Some(json!(null)), GrantWrite::Full),
            (json!(0), Some(json!(null)), GrantWrite::None),
            (json!(1), Some(json!(0)), GrantWrite::Full),
            (json!(0), Some(json!(0)), GrantWrite::None),
            (json!(0), Some(json!(1)), GrantWrite::Append),
            (json!(false), Some(json!(true)), GrantWrite::Append),
            (json!("0"), Some(json!("1")), GrantWrite::Append),
        ] {
            let mut columns = vec![(WRITE_COLUMN, write.clone())];
            if let Some(append) = &append {
                columns.push((APPEND_COLUMN, append.clone()));
            }
            let row = WrapGrantRow::from_record("wg_1", &row_with(&columns)).expect("decode");
            assert_eq!(row.write, want, "write {write}, append {append:?}");
            assert_eq!(
                row.resource_type, "",
                "absent resource_type reads as the wildcard"
            );
        }
    }

    /// Every access round-trips through `to_data`, and the columns it writes
    /// are exactly the flag pair [`encode_access`] names.
    #[test]
    fn access_round_trips_through_to_data() {
        for (write, flags) in [
            (GrantWrite::None, (0, 0)),
            (GrantWrite::Full, (1, 0)),
            (GrantWrite::Append, (0, 1)),
        ] {
            let row = NewWrapGrant {
                write,
                ..new_grant("db")
            }
            .into_row();
            let data: RecordData = row.to_data().into_iter().collect();
            assert_eq!(
                (&data[WRITE_COLUMN], &data[APPEND_COLUMN]),
                (&serde_json::json!(flags.0), &serde_json::json!(flags.1)),
                "{write:?}"
            );
            assert_eq!(
                WrapGrantRow::from_record(&row.id, &data).expect("decode"),
                row
            );
        }
    }

    /// A row without the required columns is not a grant.
    #[test]
    fn a_row_missing_a_required_column_is_refused() {
        for missing in ["grantee", "resource", "write"] {
            let mut data = row_with(&[(WRITE_COLUMN, serde_json::json!(1))]);
            data.shift_remove(missing);
            let err = WrapGrantRow::from_record("wg_1", &data).expect_err(missing);
            assert!(err.contains(missing) && err.contains("wg_1"), "{err}");
        }
    }

    /// A flag value that names no access is refused, not guessed at —
    /// `write = 2`, the spelling append-only had before migration 005,
    /// included — and so is a row that sets both flags.
    #[test]
    fn an_unrecognised_or_contradictory_access_is_refused() {
        use serde_json::json;
        for (write, append, names) in [
            (json!(2), None, "write"),
            (json!("2"), None, "write"),
            (json!(3), None, "write"),
            (json!(-1), None, "write"),
            (json!("append"), None, "write"),
            (json!(null), None, "write"),
            (json!(0), Some(json!(2)), "append"),
            (json!(0), Some(json!("yes")), "append"),
            (json!(1), Some(json!(1)), "both"),
            (json!(true), Some(json!(true)), "both"),
        ] {
            let mut columns = vec![(WRITE_COLUMN, write.clone())];
            if let Some(append) = &append {
                columns.push((APPEND_COLUMN, append.clone()));
            }
            let err = WrapGrantRow::from_record("wg_1", &row_with(&columns))
                .expect_err("unrecognised access");
            assert!(
                err.contains(names) && err.contains("wg_1"),
                "write {write}, append {append:?}: {err}"
            );
        }
    }
}

#[cfg(test)]
mod migration_005_tests {
    //! What admin's `005_wrap_grants_append_column` does through the gated
    //! runner Cloudflare and the browser apply it with, on a deployment that
    //! already holds grants — the only database its `UPDATE` has anything to
    //! do on. It lives beside the codec because this module owns the table.

    use std::collections::HashMap;

    use serde_json::json;
    use wafer_block::GrantWrite;
    use wafer_core::clients::database as db;

    use super::{list, TABLE};
    use crate::{
        blocks::admin::migrations::{SQLITE_MIGRATIONS, WRAP_GRANTS_APPEND_COLUMN},
        migration_helper,
        test_support::TestContext,
        util::RecordExt,
    };

    const ADMIN: &str = "impresspress/admin";

    /// The migrations before 005, sliced out of the shipped list by name so
    /// an unwired 005 cannot pass as applied.
    fn before_005() -> &'static [(&'static str, &'static str)] {
        let at = SQLITE_MIGRATIONS
            .iter()
            .position(|(name, _)| *name == WRAP_GRANTS_APPEND_COLUMN)
            .expect("005 is wired into SQLITE_MIGRATIONS");
        &SQLITE_MIGRATIONS[..at]
    }

    /// A row as the table held it before 005: access in `write` alone.
    fn pre_005_row(id: &str, write: i64) -> HashMap<String, serde_json::Value> {
        let mut data = HashMap::new();
        for (column, value) in [
            ("id", json!(id)),
            ("grantee", json!("impresspress/legalpages")),
            ("resource", json!("impresspress__legalpages__docs")),
            ("write", json!(write)),
            ("resource_type", json!("db")),
            ("created_at", json!("2026-01-01T00:00:00Z")),
            ("updated_at", json!("2026-01-01T00:00:00Z")),
        ] {
            data.insert(column.to_string(), value);
        }
        data
    }

    #[tokio::test]
    async fn migration_005_moves_append_grants_off_the_write_column() {
        let mut ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        migration_helper::apply_pending(&ctx, ADMIN, before_005(), &[])
            .await
            .expect("001-004 apply");
        for (id, write) in [("wg_read", 0), ("wg_full", 1), ("wg_append", 2)] {
            // Straight to the table: `write = 2` is a spelling the only
            // writer (`wrap_grants::create`) no longer produces.
            db::create(&ctx, TABLE, pre_005_row(id, write))
                .await
                .expect("seed a pre-005 grant");
        }

        ctx.set_config(migration_helper::RUN_MIGRATIONS_KEY, "1");
        migration_helper::apply_pending(&ctx, ADMIN, SQLITE_MIGRATIONS, &[])
            .await
            .expect("005 applies to a database holding grants");

        for (id, write, append, access) in [
            ("wg_read", 0, 0, GrantWrite::None),
            ("wg_full", 1, 0, GrantWrite::Full),
            ("wg_append", 0, 1, GrantWrite::Append),
        ] {
            let stored = db::get(&ctx, TABLE, id).await.expect("read the grant");
            assert_eq!(
                (&stored.data["write"], &stored.data["append"]),
                (&json!(write), &json!(append)),
                "{id}"
            );
            // What a binary without the column would make of the row.
            assert_eq!(
                stored.data.bool_field("write"),
                access == GrantWrite::Full,
                "{id} as a binary without the append column reads it"
            );
        }
        let mut listed: Vec<(String, GrantWrite)> = list(&ctx)
            .await
            .expect("every migrated row decodes")
            .into_iter()
            .map(|row| (row.id, row.write))
            .collect();
        listed.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            listed,
            vec![
                ("wg_append".to_string(), GrantWrite::Append),
                ("wg_full".to_string(), GrantWrite::Full),
                ("wg_read".to_string(), GrantWrite::None),
            ]
        );
    }
}

/// The boot flavour, over [`DatabaseService`]: the tests `boot.rs` carried
/// for the grant loader, against the moved name.
#[cfg(test)]
mod boot_tests {
    use wafer_block::db::Filter;
    use wafer_core::interfaces::database::service::{
        AggregateSpec, Column, Record, RecordList, Table, UpsertSpec,
    };

    use super::*;

    /// Open a fresh in-memory SQLite [`DatabaseService`] with no migrations
    /// applied.
    async fn bare_db() -> Arc<dyn DatabaseService> {
        Arc::new(
            wafer_block_sqlite::service::SQLiteDatabaseService::open_in_memory()
                .expect("open in-memory sqlite"),
        )
    }

    async fn seed(
        db: &Arc<dyn DatabaseService>,
        grantee: &str,
        resource: &str,
        write: GrantWrite,
        resource_type: &str,
    ) {
        let row = NewWrapGrant {
            grantee: grantee.to_string(),
            resource: resource.to_string(),
            write,
            resource_type: resource_type.to_string(),
            description: String::new(),
        }
        .into_row();
        db.create(TABLE, row.to_data()).await.expect("seed grant");
    }

    #[tokio::test]
    async fn load_maps_rows_and_tolerates_missing_table() {
        let db = bare_db().await;

        // Missing table → empty, no error.
        assert!(load(&db)
            .await
            .expect("a missing table is a cold start")
            .is_empty());

        // Apply admin migrations (creates the wrap_grants table among the
        // other admin tables) through the real migration runner — the
        // migration-file-runner exception to the no-raw-SQL rule (CLAUDE.md),
        // reusing the real embedded schema.
        crate::test_support::migrate_sqlite(
            &db,
            crate::blocks::admin::ADMIN_BLOCK_ID,
            crate::blocks::admin::migrations::SQLITE_MIGRATIONS,
        )
        .await
        .expect("apply admin migrations");

        seed(
            &db,
            "impresspress/files",
            "impresspress__files__objects",
            GrantWrite::Full,
            "db",
        )
        .await;
        seed(&db, "wafer-run/auth", "bucket/x", GrantWrite::None, "").await;

        let grants = load(&db).await.expect("load");
        assert_eq!(grants.len(), 2);
        let g1 = grants
            .iter()
            .find(|g| g.grantee == "impresspress/files")
            .unwrap();
        assert_eq!(g1.write, GrantWrite::Full);
        assert_eq!(g1.resource_type, Some(wafer_run::ResourceType::Db));
        let g2 = grants
            .iter()
            .find(|g| g.grantee == "wafer-run/auth")
            .unwrap();
        assert_eq!(g2.write, GrantWrite::None);
        assert_eq!(g2.resource_type, None);

        // Unrecognized resource_type → the ROW is dropped (fail-closed),
        // never widened to the all-types wildcard.
        seed(
            &db,
            "impresspress/products",
            "impresspress__products__items",
            GrantWrite::Full,
            "databsae",
        )
        .await;
        // Empty-string resource_type → kept as an intentional wildcard.
        seed(&db, "impresspress/files", "bucket/y", GrantWrite::None, "").await;
        // An append grant not typed `db` → the ROW is dropped, so the rest of
        // the set still reaches `Wafer::add_wrap_grants`, which would refuse
        // all of it over this one.
        seed(
            &db,
            "impresspress/legalpages",
            "impresspress__legalpages__docs",
            GrantWrite::Append,
            "",
        )
        .await;

        let grants = load(&db).await.expect("load");
        assert_eq!(
            grants.len(),
            3,
            "typo'd resource_type and untyped append rows must be dropped"
        );
        assert!(grants.iter().all(|g| g.grantee != "impresspress/products"));
        assert!(grants
            .iter()
            .all(|g| g.grantee != "impresspress/legalpages"));
        let g4 = grants
            .iter()
            .find(|g| g.resource == "bucket/y")
            .expect("empty resource_type row kept");
        assert_eq!(g4.resource_type, None);
    }

    /// On a table 001-004 created, a row that spelled append-only `write = 2`
    /// comes out of 005 as `write = 0, append = 1` and still loads as an
    /// append grant; a second boot over the same list runs nothing and
    /// changes nothing.
    #[tokio::test]
    async fn migration_005_moves_a_write_2_row_onto_the_append_column() {
        use crate::blocks::admin::{
            migrations::{SQLITE_MIGRATIONS, WRAP_GRANTS_APPEND_COLUMN},
            ADMIN_BLOCK_ID,
        };

        let db = bare_db().await;
        let at = SQLITE_MIGRATIONS
            .iter()
            .position(|(name, _)| *name == WRAP_GRANTS_APPEND_COLUMN)
            .expect("005 is wired into SQLITE_MIGRATIONS");
        assert!(
            SQLITE_MIGRATIONS[at].1.contains("ADD COLUMN append"),
            "{WRAP_GRANTS_APPEND_COLUMN} is where the list says it is"
        );
        crate::test_support::migrate_sqlite(&db, ADMIN_BLOCK_ID, &SQLITE_MIGRATIONS[..at])
            .await
            .expect("001-004 apply");

        // Straight to the table in the pre-005 spelling, which the codec no
        // longer writes.
        let mut legacy = HashMap::new();
        for (column, value) in [
            ("id", json!("wg_legacy")),
            ("grantee", json!("impresspress/legalpages")),
            ("resource", json!("impresspress__legalpages__docs")),
            ("write", json!(2)),
            ("resource_type", json!("db")),
            ("created_at", json!("2026-01-01T00:00:00Z")),
            ("updated_at", json!("2026-01-01T00:00:00Z")),
        ] {
            legacy.insert(column.to_string(), value);
        }
        db.create(TABLE, legacy)
            .await
            .expect("seed a write = 2 row");
        assert!(
            load(&db).await.expect("load").is_empty(),
            "before 005 the codec refuses `write = 2`"
        );

        for boot in ["first", "second"] {
            crate::test_support::migrate_sqlite(&db, ADMIN_BLOCK_ID, SQLITE_MIGRATIONS)
                .await
                .unwrap_or_else(|e| panic!("{boot} boot: {e}"));
            let stored = db.get(TABLE, "wg_legacy").await.expect("read the row");
            assert_eq!(stored.data["write"], json!(0), "{boot} run");
            assert_eq!(stored.data["append"], json!(1), "{boot} run");
            let grants = load(&db).await.expect("load");
            assert_eq!(grants.len(), 1, "{boot} run");
            assert_eq!(grants[0].write, GrantWrite::Append, "{boot} run");
        }
    }

    /// A [`DatabaseService`] that answers [`load`]'s two questions from a
    /// script — `list` with no rows or the error `list` builds, the existence
    /// check with what `exists` builds — counts the existence checks it is
    /// asked, and is [`unreachable!`] everywhere else, so any other read
    /// `load` made would fail the test.
    struct ErroringDb {
        /// `None` lists no rows; `Some(f)` fails the list with `f()`.
        list: Option<fn() -> DatabaseError>,
        exists: fn() -> Result<bool, DatabaseError>,
        probes: std::sync::atomic::AtomicUsize,
    }

    impl ErroringDb {
        fn new(
            list: Option<fn() -> DatabaseError>,
            exists: fn() -> Result<bool, DatabaseError>,
        ) -> Arc<Self> {
            Arc::new(Self {
                list,
                exists,
                probes: Default::default(),
            })
        }

        fn probes(&self) -> usize {
            self.probes.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    fn outage() -> DatabaseError {
        DatabaseError::Unavailable("D1_ERROR: simulated wrap_grants outage".into())
    }

    fn probe_failed() -> Result<bool, DatabaseError> {
        Err(DatabaseError::Internal(
            "simulated wrap_grants existence-check failure".into(),
        ))
    }

    fn no_such_table() -> DatabaseError {
        DatabaseError::Internal(format!("no such table: {TABLE}"))
    }

    #[async_trait::async_trait]
    impl DatabaseService for ErroringDb {
        async fn catalog_tables(&self) -> Result<Vec<String>, DatabaseError> {
            unreachable!()
        }

        async fn catalog_columns(
            &self,
            _table: &str,
        ) -> Result<Vec<wafer_core::interfaces::database::service::CatalogColumn>, DatabaseError>
        {
            unreachable!()
        }

        async fn catalog_row_count(&self, _table: &str) -> Result<i64, DatabaseError> {
            unreachable!()
        }

        async fn schema_table_exists(&self, _name: &str) -> Result<bool, DatabaseError> {
            self.probes
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            (self.exists)()
        }

        async fn schema_columns(&self, _table: &str) -> Result<Vec<String>, DatabaseError> {
            unreachable!()
        }

        async fn get(&self, _collection: &str, _id: &str) -> Result<Record, DatabaseError> {
            unreachable!("load reads the table through `list` only")
        }

        async fn list(
            &self,
            _collection: &str,
            _opts: &ListOptions,
        ) -> Result<RecordList, DatabaseError> {
            match self.list {
                None => Ok(RecordList {
                    records: Vec::new(),
                    total_count: 0,
                    page: 1,
                    page_size: 10_000,
                }),
                Some(fail) => Err(fail()),
            }
        }

        async fn create(
            &self,
            _collection: &str,
            _data: HashMap<String, serde_json::Value>,
        ) -> Result<Record, DatabaseError> {
            unreachable!()
        }

        async fn update(
            &self,
            _collection: &str,
            _id: &str,
            _data: HashMap<String, serde_json::Value>,
        ) -> Result<Record, DatabaseError> {
            unreachable!()
        }

        async fn delete(&self, _collection: &str, _id: &str) -> Result<(), DatabaseError> {
            unreachable!()
        }

        async fn count(
            &self,
            _collection: &str,
            _filters: &[Filter],
        ) -> Result<i64, DatabaseError> {
            unreachable!()
        }

        async fn sum(
            &self,
            _collection: &str,
            _field: &str,
            _filters: &[Filter],
        ) -> Result<f64, DatabaseError> {
            unreachable!()
        }

        async fn increment_field_where(
            &self,
            _collection: &str,
            _col: &str,
            _delta: i64,
            _filters: &[Filter],
        ) -> Result<i64, DatabaseError> {
            unreachable!()
        }

        async fn query_raw(
            &self,
            _query: &str,
            _args: &[serde_json::Value],
        ) -> Result<Vec<Record>, DatabaseError> {
            unreachable!()
        }

        async fn exec_raw(
            &self,
            _query: &str,
            _args: &[serde_json::Value],
        ) -> Result<i64, DatabaseError> {
            unreachable!()
        }

        async fn exec_raw_transaction(
            &self,
            _statements: &[wafer_core::interfaces::database::service::RawStatement],
        ) -> Result<Vec<i64>, wafer_core::interfaces::database::service::TransactionError> {
            unreachable!()
        }

        async fn create_many(
            &self,
            _collection: &str,
            _rows: Vec<std::collections::HashMap<String, serde_json::Value>>,
        ) -> Result<i64, wafer_core::interfaces::database::service::DatabaseError> {
            unreachable!()
        }

        async fn take_where(
            &self,
            _collection: &str,
            _filters: &[Filter],
        ) -> Result<Vec<Record>, DatabaseError> {
            unreachable!()
        }

        async fn update_where(
            &self,
            _collection: &str,
            _filters: &[Filter],
            _data: HashMap<String, serde_json::Value>,
        ) -> Result<(), DatabaseError> {
            unreachable!()
        }

        async fn batch(
            &self,
            _ops: Vec<wafer_core::interfaces::database::service::WriteOp>,
        ) -> Result<
            Vec<wafer_core::interfaces::database::service::WriteOutcome>,
            wafer_core::interfaces::database::service::DatabaseError,
        > {
            unreachable!()
        }

        async fn insert_guarded(
            &self,
            _collection: &str,
            _data: std::collections::HashMap<String, serde_json::Value>,
            _guards: &[wafer_core::interfaces::database::service::CapGuard],
        ) -> Result<
            wafer_core::interfaces::database::service::GuardedInsert,
            wafer_core::interfaces::database::service::DatabaseError,
        > {
            unreachable!()
        }

        async fn update_guarded(
            &self,
            _collection: &str,
            _filters: &[wafer_block::db::Filter],
            _data: std::collections::HashMap<String, serde_json::Value>,
            _guards: &[wafer_core::interfaces::database::service::CapGuard],
        ) -> Result<
            wafer_core::interfaces::database::service::GuardedUpdate,
            wafer_core::interfaces::database::service::DatabaseError,
        > {
            unreachable!()
        }

        async fn upsert(
            &self,
            _collection: &str,
            _spec: UpsertSpec,
        ) -> Result<Option<Record>, DatabaseError> {
            unreachable!()
        }

        async fn aggregate(
            &self,
            _collection: &str,
            _spec: AggregateSpec,
        ) -> Result<Vec<Record>, DatabaseError> {
            unreachable!()
        }

        async fn ensure_schema_table(&self, _table: &Table) -> Result<(), DatabaseError> {
            unreachable!()
        }

        async fn schema_drop_table(&self, _name: &str) -> Result<(), DatabaseError> {
            unreachable!()
        }

        async fn schema_add_column(
            &self,
            _table: &str,
            _column: &Column,
        ) -> Result<(), DatabaseError> {
            unreachable!()
        }
    }

    /// A list that failed is an error, never "no grants": a runtime built
    /// without its admin-created grants serves, is cached as healthy, and
    /// denies every access they exist to allow. When the existence check
    /// cannot answer either, the error returned is the list's — the read
    /// that failed — not the probe's.
    #[tokio::test]
    async fn a_failed_read_is_an_error_not_an_empty_grant_set() {
        let scripted = ErroringDb::new(Some(outage), probe_failed);
        let db: Arc<dyn DatabaseService> = scripted.clone();
        let error = load(&db)
            .await
            .expect_err("a failed list must not answer no grants");
        assert!(
            matches!(error, DatabaseError::Unavailable(ref m) if m.contains("simulated wrap_grants outage")),
            "the list's own error comes back: {error:?}"
        );
        assert_eq!(scripted.probes(), 1, "the probe ran once, after the list");
    }

    /// A probe that finds the table says the failure was not the cold start,
    /// so the list's error stands.
    #[tokio::test]
    async fn a_failed_read_of_a_present_table_is_an_error() {
        let db: Arc<dyn DatabaseService> = ErroringDb::new(Some(outage), || Ok(true));
        let error = load(&db)
            .await
            .expect_err("the table is there, so the failed list is an outage");
        assert!(matches!(error, DatabaseError::Unavailable(_)), "{error:?}");
    }

    /// Only a definite "absent" makes a failed list the fresh-install cold
    /// start, which answers no grants.
    #[tokio::test]
    async fn a_failed_read_of_an_absent_table_is_the_cold_start() {
        let scripted = ErroringDb::new(Some(no_such_table), || Ok(false));
        let db: Arc<dyn DatabaseService> = scripted.clone();
        assert!(load(&db)
            .await
            .expect("an absent table is a fresh install, not a failure")
            .is_empty());
        assert_eq!(scripted.probes(), 1);
    }

    /// A list that succeeds never asks whether the table exists: on D1 that
    /// question is a `sqlite_master` scan, and every database-built runtime
    /// used to pay it before listing.
    #[tokio::test]
    async fn a_healthy_list_never_probes() {
        let scripted = ErroringDb::new(None, || {
            unreachable!("a list that answered needs no existence check")
        });
        let db: Arc<dyn DatabaseService> = scripted.clone();
        assert!(load(&db).await.expect("an empty table lists").is_empty());
        assert_eq!(scripted.probes(), 0);
    }
}
