//! Row-level access over `impresspress__files__buckets`.
//!
//! Buckets are user-created storage containers (one row per bucket). The
//! table is the single source of truth for bucket existence / ownership /
//! visibility — both the admin and user listing paths read it, and
//! [`find_owned`] is *the* ownership lookup every access-control caller
//! derives from (`storage::bucket_owned_by` → `require_bucket_access`,
//! the SSR portal's owner check, and the share-creation path).

use wafer_block::db::{Filter, FilterOp, ListOptions, SortField};
use wafer_core::clients::database::{self as db, Record};
use wafer_run::{context::Context, WaferError};

use super::Page;
use crate::{
    db_read::{self, Bound, CappedList},
    util::RecordExt,
};

/// Buckets table — user-created storage containers (one row per bucket).
pub const TABLE: &str = "impresspress__files__buckets";

/// One bucket row, decoded.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct BucketRow {
    pub id: String,
    /// Bucket name, and the blob-namespace folder name in `wafer-run/storage`.
    ///
    /// Unique across the table, enforced by the
    /// `impresspress__files__buckets_name_uniq` index (migration 002) rather
    /// than by any caller: because the name IS the folder, two rows for one
    /// name are two owners of one folder, and [`find_owned`] would hand the
    /// second one read/write/delete access to the first one's objects. The
    /// index is what makes [`insert`] the atomic claim on a name.
    pub name: String,
    /// Whether objects in the bucket are readable by anonymous URL.
    ///
    /// **B13.** The column is `INTEGER` on SQLite (migration 001) and
    /// `BOOLEAN` on Postgres, and every writer writes a JSON bool into it, so
    /// it reads back as `Number(0|1)`, `Bool`, or — from a TEXT-typed
    /// backend — `String("true")`. [`BucketRow::from_record`] is the only
    /// place in the block that turns any of those into this `bool`; the user
    /// and admin bucket pages used to do it themselves, one with `as_bool()`
    /// and one with `str_field(..) == "true"`, and so disagreed about the
    /// same row.
    pub public: bool,
    /// User id of the creator — the ownership key every access check filters
    /// on (see [`find_owned`]).
    pub created_by: String,
    /// RFC 3339 creation instant.
    pub created_at: String,
    pub updated_at: String,
}

impl BucketRow {
    /// The one decode of a bucket row, `public` included.
    pub fn from_record(rec: &Record) -> Self {
        Self {
            id: rec.id.clone(),
            name: rec.str_field("name").to_string(),
            public: rec.bool_field("public"),
            created_by: rec.str_field("created_by").to_string(),
            created_at: rec.str_field("created_at").to_string(),
            updated_at: rec.str_field("updated_at").to_string(),
        }
    }
}

fn created_by_filter(user_id: &str) -> Filter {
    Filter {
        field: "created_by".to_string(),
        operator: FilterOp::Equal,
        value: serde_json::Value::String(user_id.to_string()),
    }
}

/// Look up the bucket named `name` owned by `user_id`. Returns `Ok(None)`
/// when no such row exists (unknown bucket OR a bucket owned by someone
/// else — callers cannot distinguish the two, by design).
///
/// This is the single bucket-ownership predicate for the files block;
/// `storage::bucket_owned_by` projects it to a bool, and the admin-bypass
/// policy split lives in `storage::require_bucket_access` (see its docs).
pub async fn find_owned(
    ctx: &dyn Context,
    name: &str,
    user_id: &str,
) -> Result<Option<BucketRow>, WaferError> {
    let filters = vec![
        Filter {
            field: "name".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::Value::String(name.to_string()),
        },
        created_by_filter(user_id),
    ];
    let records = db_read::list_bounded(
        ctx,
        TABLE,
        filters,
        Bound::UniqueKey("buckets.name is declared UNIQUE"),
    )
    .await?;
    Ok(records.first().map(BucketRow::from_record))
}

/// List bucket rows visible to `owner`: `Some(user_id)` restricts to that
/// user's buckets (`created_by` filter), `None` returns every bucket (the
/// admin view). Unsorted — mirrors the JSON API listing.
///
/// Capped, and it says so: buckets are created self-service, so the admin
/// view (`owner = None`) lists a set that grows with the deployment.
pub async fn list_visible(
    ctx: &dyn Context,
    owner: Option<&str>,
) -> Result<CappedList<BucketRow>, WaferError> {
    let filters = match owner {
        Some(user_id) => vec![created_by_filter(user_id)],
        None => Vec::new(),
    };
    Ok(db_read::list_capped(ctx, TABLE, filters)
        .await?
        .map(|record| BucketRow::from_record(&record)))
}

/// List `user_id`'s buckets sorted by `name` ascending (the SSR bucket-list
/// page order).
pub async fn list_owned_sorted(
    ctx: &dyn Context,
    user_id: &str,
) -> Result<CappedList<BucketRow>, WaferError> {
    let records = db_read::list_capped_sorted(
        ctx,
        TABLE,
        vec![created_by_filter(user_id)],
        vec![SortField {
            field: "name".to_string(),
            desc: false,
        }],
    )
    .await?;
    Ok(records.map(|record| BucketRow::from_record(&record)))
}

/// Most recently created buckets, newest first (admin listing).
pub async fn list_recent(ctx: &dyn Context, limit: u32) -> Result<Page<BucketRow>, WaferError> {
    let opts = ListOptions {
        sort: vec![SortField {
            field: "created_at".to_string(),
            desc: true,
        }],
        limit: Some(limit),
        ..Default::default()
    };
    Ok(Page::decode(
        db::list(ctx, TABLE, &opts).await?,
        BucketRow::from_record,
    ))
}

/// Insert a bucket row (`created_at` stamped with
/// [`crate::util::now_rfc3339`]) and return it.
///
/// This is the claim on the bucket name: `name` is UNIQUE, so the insert is
/// what decides which of two users creating the same name gets the folder.
/// The caller creates the storage folder only after it succeeds.
pub async fn insert(
    ctx: &dyn Context,
    name: &str,
    public: bool,
    created_by: &str,
) -> Result<BucketRow, WaferError> {
    let data = crate::util::json_map(serde_json::json!({
        "name": name,
        "public": public,
        "created_by": created_by,
        "created_at": crate::util::now_rfc3339(),
    }));
    db::create(ctx, TABLE, data).await.map(|r| {
        // The insert echo carries the row as the caller wrote it; the decode
        // is the same one a later read goes through, so `public` means the
        // same thing on both paths.
        BucketRow::from_record(&r)
    })
}

/// Delete one bucket row by id.
///
/// The rollback [`super::super::storage::handle_create_bucket`] runs when the
/// storage folder for a row it just inserted could not be created. By id, not
/// by name: [`delete_by_name`] is only safe while the unique index exists, and
/// a deployment that takes the code half without `--run-migrations` has the
/// row but not the index. There, a second user's create on a taken name still
/// inserts, and a name-scoped rollback would delete the first owner's row
/// along with it.
pub async fn delete(ctx: &dyn Context, id: &str) -> Result<(), WaferError> {
    db::delete(ctx, TABLE, id).await
}

/// Delete the bucket row named `name`.
///
/// One row at most: `name` is UNIQUE (the
/// `impresspress__files__buckets_name_uniq` index from migration 002), which
/// is what lets the bucket-delete path drop the row by name without an owner
/// filter after its own access check has passed.
pub async fn delete_by_name(ctx: &dyn Context, name: &str) -> Result<(), WaferError> {
    db::delete_by_field(
        ctx,
        TABLE,
        "name",
        serde_json::Value::String(name.to_string()),
    )
    .await
}

/// Total number of bucket rows (admin stats).
pub async fn count_all(ctx: &dyn Context) -> Result<i64, WaferError> {
    db::count(ctx, TABLE, &[]).await
}

/// Test-fixture seeding: insert a raw row map exactly as given (no stamped
/// columns), so tests control the precise row shape.
#[cfg(test)]
pub async fn seed(
    ctx: &dyn Context,
    data: std::collections::HashMap<String, serde_json::Value>,
) -> Result<BucketRow, WaferError> {
    db::create(ctx, TABLE, data)
        .await
        .map(|r| BucketRow::from_record(&r))
}

/// Test helper: every row of `TABLE`, undecoded — each column exactly as
/// stored, for asserting that something left the table alone.
#[cfg(test)]
pub async fn raw_rows(
    ctx: &dyn Context,
) -> Result<Vec<wafer_core::clients::database::Record>, WaferError> {
    crate::db_read::list_every(ctx, TABLE, vec![]).await
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::test_support::TestContext;

    fn record_with_public(value: serde_json::Value) -> Record {
        Record {
            id: "b1".to_string(),
            data: [
                ("name".to_string(), json!("photos")),
                ("public".to_string(), value),
                ("created_by".to_string(), json!("alice")),
                ("created_at".to_string(), json!("2026-05-06T10:00:00Z")),
                ("updated_at".to_string(), json!("2026-05-06T10:00:01Z")),
            ]
            .into_iter()
            .collect(),
        }
    }

    /// B13. The three JSON shapes `public` comes back as — `Number` from
    /// SQLite's `INTEGER` column, `Bool` from Postgres's `BOOLEAN`, and the
    /// string a TEXT-typed backend hands back — all mean the same thing, and
    /// this one decode is where they become the same `bool`. Before this,
    /// the user bucket page accepted only `Bool` (`as_bool()`) and the admin
    /// page only `String("true")` (`str_field(..) == "true"`), so on SQLite
    /// both read a public bucket as private and on Postgres they disagreed.
    #[test]
    fn from_record_decodes_every_shape_public_arrives_in() {
        for shape in [json!(1), json!(true), json!("true")] {
            assert!(
                BucketRow::from_record(&record_with_public(shape.clone())).public,
                "`public` as {shape} must decode to true"
            );
        }
        for shape in [json!(0), json!(false), json!("false")] {
            assert!(
                !BucketRow::from_record(&record_with_public(shape.clone())).public,
                "`public` as {shape} must decode to false"
            );
        }
    }

    /// The rest of the row, so a column added to the struct without a decode
    /// cannot pass unnoticed.
    #[test]
    fn from_record_decodes_the_whole_row() {
        let row = BucketRow::from_record(&record_with_public(json!(1)));
        assert_eq!(
            row,
            BucketRow {
                id: "b1".to_string(),
                name: "photos".to_string(),
                public: true,
                created_by: "alice".to_string(),
                created_at: "2026-05-06T10:00:00Z".to_string(),
                updated_at: "2026-05-06T10:00:01Z".to_string(),
            }
        );
    }

    /// A round trip through the real SQLite fixture: what `insert` wrote as
    /// a JSON bool reads back as the same `bool`, whatever the column type
    /// turned it into in between.
    #[tokio::test]
    async fn public_survives_the_round_trip_through_the_database() {
        let ctx = TestContext::with_files().await;
        insert(&ctx, "photos", true, "alice").await.expect("seed");
        insert(&ctx, "docs", false, "alice").await.expect("seed");

        let photos = find_owned(&ctx, "photos", "alice")
            .await
            .expect("find_owned")
            .expect("row");
        let docs = find_owned(&ctx, "docs", "alice")
            .await
            .expect("find_owned")
            .expect("row");
        assert!(photos.public, "a bucket created public must read public");
        assert!(!docs.public, "a bucket created private must read private");
    }

    /// The ownership predicate matches on BOTH `name` and `created_by`:
    /// a hit requires the exact (bucket, owner) pair, cross-user lookups
    /// and unknown buckets both come back `None`.
    #[tokio::test]
    async fn find_owned_matches_only_the_name_owner_pair() {
        let ctx = TestContext::with_files().await;
        insert(&ctx, "photos", false, "alice").await.expect("seed");
        insert(&ctx, "docs", true, "bob").await.expect("seed");

        let hit = find_owned(&ctx, "photos", "alice")
            .await
            .expect("find_owned")
            .expect("alice owns photos");
        assert_eq!(hit.name, "photos");
        assert_eq!(hit.created_by, "alice");

        // Someone else's bucket → None (cross-user isolation).
        assert!(find_owned(&ctx, "photos", "bob")
            .await
            .expect("find_owned")
            .is_none());
        // Unknown bucket → None.
        assert!(find_owned(&ctx, "missing", "alice")
            .await
            .expect("find_owned")
            .is_none());
    }
}
