//! [`DatabaseConfigSource`] over native SQLite, through the admin migrations
//! production applies: the source Cloudflare resolves block config with,
//! proven on the backend a native test and the D1 cost harness can run.
//!
//! These moved here from `impresspress-cloudflare`'s `config_source.rs` wasm
//! tests, where the source ran over an in-memory list double. Over a real
//! table they also pin what the double could not: the `block` column
//! migration 002 adds and backfills, the unique `key`, and the query the
//! executor actually sends. The Cloudflare crate keeps the D1 side —
//! `runtime_build`'s tests pin the statements this source sends to D1.
//!
//! Its own test binary because the memo is keyed on the process-wide
//! config-write generation (`impresspress_core::config_generation`): in the
//! library's binary, any test writing the variables table moves it, so an
//! exact read count would flake. Here only these tests run, and each holds
//! [`GENERATION`] for its whole body.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, LazyLock,
    },
};

use impresspress_core::{
    blocks::admin::{migrations::MIGRATIONS, ADMIN_BLOCK_ID},
    config_generation::note_config_write,
    config_source::DatabaseConfigSource,
    migration_helper::executable_statements,
    platform_state::variables::{self, NewVariable},
    test_support::migrate_sqlite,
};
use wafer_block::ConfigVar;
use wafer_core::interfaces::database::service::{DatabaseService, RecordList};
use wafer_run::{ConfigError, ConfigSource};

/// Held by every test for its whole body: see the module doc.
static GENERATION: LazyLock<futures::lock::Mutex<()>> =
    LazyLock::new(|| futures::lock::Mutex::new(()));

/// Counts the reads of the variables table that reach the database, and how
/// many of them carried a filter.
struct Counting {
    inner: Arc<dyn DatabaseService>,
    lists: AtomicUsize,
    filtered_lists: AtomicUsize,
}

impl Counting {
    fn inner_service(&self) -> &dyn DatabaseService {
        self.inner.as_ref()
    }

    fn lists(&self) -> usize {
        self.lists.load(Ordering::SeqCst)
    }
}

wafer_core::forward_database_service! {
    impl DatabaseService for Counting {
        forward_to inner_service();

        ops {
            get: forward,
            list: custom,
            create: forward,
            create_many: forward,
            update: forward,
            delete: forward,
            count: forward,
            sum: forward,
            query_raw: forward,
            exec_raw: forward,
            exec_raw_transaction: forward,
            delete_where: forward,
            delete_where_count: forward,
            take_where: forward,
            update_where: forward,
            update_where_count: forward,
            increment_field_where: forward,
            upsert: forward,
            aggregate: forward,
            batch: forward,
            insert_guarded: forward,
            update_guarded: forward,
            ensure_schema_table: forward,
            ensure_schema_tables: forward,
            schema_table_exists: forward,
            schema_columns: forward,
            schema_drop_table: forward,
            schema_add_column: forward,
            statement_budget: forward,
        }

        async fn list(
            &self,
            collection: &str,
            opts: &wafer_block::db::ListOptions,
        ) -> Result<RecordList, wafer_core::interfaces::database::service::DatabaseError> {
            if collection == variables::TABLE {
                self.lists.fetch_add(1, Ordering::SeqCst);
                if !opts.filters.is_empty() || opts.filter_tree.is_some() {
                    self.filtered_lists.fetch_add(1, Ordering::SeqCst);
                }
            }
            self.inner.list(collection, opts).await
        }
    }
}

/// A fresh in-memory SQLite database, behind a [`Counting`] decorator.
fn database() -> (Arc<Counting>, Arc<dyn DatabaseService>) {
    let counting = Arc::new(Counting {
        inner: Arc::new(
            wafer_block_sqlite::service::SQLiteDatabaseService::open_in_memory()
                .expect("open in-memory sqlite"),
        ),
        lists: AtomicUsize::new(0),
        filtered_lists: AtomicUsize::new(0),
    });
    let db: Arc<dyn DatabaseService> = counting.clone();
    (counting, db)
}

/// Admin's migrations applied through the real runner.
async fn migrate(db: &Arc<dyn DatabaseService>) {
    migrate_sqlite(db, ADMIN_BLOCK_ID, MIGRATIONS.sqlite)
        .await
        .expect("apply admin migrations");
}

/// Admin migration file `index`'s statements, sent as raw SQL — no ledger,
/// no write path, so no config write is recorded. For staging a database
/// older than the ledger can describe: admin's own ledger row needs a column
/// migration 003 adds.
async fn run_raw(db: &Arc<dyn DatabaseService>, index: usize) {
    let (name, sql) = MIGRATIONS.sqlite[index];
    for statement in executable_statements(sql) {
        db.exec_raw(&statement, &[])
            .await
            .unwrap_or_else(|e| panic!("{name}: {e}"));
    }
}

/// A database at today's schema.
async fn migrated() -> (Arc<Counting>, Arc<dyn DatabaseService>) {
    let (counting, db) = database();
    migrate(&db).await;
    (counting, db)
}

/// Insert a row as the seeders do, with the `block` its key derives
/// (`None` for a shared or ad hoc key). Fixture setup straight through the
/// service: no config write is recorded, so a test decides when one is.
async fn seed(db: &Arc<dyn DatabaseService>, key: &str, value: &str) {
    let row = NewVariable {
        key: key.to_string(),
        value: value.to_string(),
        name: String::new(),
        description: String::new(),
        warning: String::new(),
        sensitive: false,
        updated_by: String::new(),
        block: variables::block_for_key(key),
    }
    .into_row();
    db.create(variables::TABLE, row.to_data())
        .await
        .expect("seed a variables row");
}

fn optional(key: &str) -> ConfigVar {
    ConfigVar::new(key, "doc", "").optional()
}

/// THE read-amplification fix: initializing every block must cost ONE
/// query, not one per block. Production ran 22 configured blocks, each
/// paying its own KV-cached per-block lookup on every cold hydration.
#[tokio::test]
async fn every_block_is_served_by_one_unfiltered_read() {
    let _generation = GENERATION.lock().await;
    let (counting, db) = migrated().await;
    seed(&db, "WAFER_RUN__AUTH__A", "auth-value").await;
    seed(&db, "IMPRESSPRESS__EMAIL__B", "email-value").await;
    seed(&db, "WAFER_RUN_SHARED__APP_NAME", "shared-value").await;
    let source = DatabaseConfigSource::new(db);

    let auth = source
        .load_for_block("wafer-run/auth", &[optional("WAFER_RUN__AUTH__A")])
        .await
        .expect("auth resolves");
    assert_eq!(auth.get("WAFER_RUN__AUTH__A"), Some("auth-value"));
    let email = source
        .load_for_block("impresspress/email", &[optional("IMPRESSPRESS__EMAIL__B")])
        .await
        .expect("email resolves");
    assert_eq!(email.get("IMPRESSPRESS__EMAIL__B"), Some("email-value"));

    assert_eq!(
        counting.lists(),
        1,
        "the snapshot must be read once and reused for every block"
    );
    assert_eq!(
        counting.filtered_lists.load(Ordering::SeqCst),
        0,
        "a per-block filter would be a cacheable shape and reintroduce one KV read per block"
    );
}

/// Rows stay scoped to their own block. The snapshot holds every row at once,
/// so a grouping bug would silently let one block read another's value —
/// which the old per-block `WHERE` clause made impossible. A shared row
/// (`block` NULL) belongs to no block's config.
#[tokio::test]
async fn a_block_sees_only_its_own_rows() {
    let _generation = GENERATION.lock().await;
    let (_counting, db) = migrated().await;
    seed(&db, "WAFER_RUN__AUTH__A", "auth-value").await;
    seed(&db, "WAFER_RUN_SHARED__APP_NAME", "shared-value").await;
    let source = DatabaseConfigSource::new(db);

    let declared = [
        optional("WAFER_RUN__AUTH__A"),
        optional("WAFER_RUN_SHARED__APP_NAME"),
    ];
    let email = source
        .load_for_block("impresspress/email", &declared)
        .await
        .expect("email resolves");
    assert_eq!(
        email.get("WAFER_RUN__AUTH__A"),
        None,
        "auth's row is not email's config"
    );
    assert_eq!(
        email.get("WAFER_RUN_SHARED__APP_NAME"),
        None,
        "a NULL-block row belongs to no block"
    );

    let auth = source
        .load_for_block("wafer-run/auth", &declared)
        .await
        .expect("auth resolves");
    assert_eq!(auth.get("WAFER_RUN__AUTH__A"), Some("auth-value"));
    assert_eq!(auth.get("WAFER_RUN_SHARED__APP_NAME"), None);
}

/// THE boot-ordering hazard, reproduced.
///
/// A runtime build reads config and writes it in the same pass: admin
/// initializes first, its `Init` runs `settings::seed_defaults`, and the
/// Cloudflare hook then runs `seed_auto_generated` — all after the database
/// service block (which declares a config key of its own, so the no-keys
/// short-circuit does not skip it) has already been lazily initialized by
/// admin's own seeding query, filling the snapshot.
///
/// Caching for the source's lifetime would make every row seeded after that
/// point invisible to every block initialized later. For a required key with
/// no default that is a permanent `InitError` cached for the block slot's
/// lifetime, not merely a stale read.
#[tokio::test]
async fn config_seeded_mid_boot_is_visible_to_blocks_initialized_after_it() {
    let _generation = GENERATION.lock().await;
    let (counting, db) = migrated().await;
    let source = DatabaseConfigSource::new(db.clone());

    let early = source
        .load_for_block("wafer-run/auth", &[optional("WAFER_RUN__AUTH__SEEDED")])
        .await
        .expect("early resolves");
    assert_eq!(
        early.get("WAFER_RUN__AUTH__SEEDED"),
        None,
        "nothing seeded yet"
    );

    // Seeding happens: the row lands, and the writer records it.
    seed(&db, "WAFER_RUN__AUTH__SEEDED", "seeded-value").await;
    note_config_write();

    let late = source
        .load_for_block("wafer-run/auth", &[optional("WAFER_RUN__AUTH__SEEDED")])
        .await
        .expect("late resolves");
    assert_eq!(
        late.get("WAFER_RUN__AUTH__SEEDED"),
        Some("seeded-value"),
        "a block initialized after seeding must see the seeded row"
    );
    assert_eq!(
        counting.lists(),
        2,
        "exactly one re-read: the write invalidated the snapshot once"
    );
}

/// The re-read is driven by writes, not by every call. An established
/// database seeds nothing, so the whole pass must still cost one query.
#[tokio::test]
async fn no_config_write_means_no_reread() {
    let _generation = GENERATION.lock().await;
    let (counting, db) = migrated().await;
    seed(&db, "WAFER_RUN__AUTH__K", "v").await;
    let source = DatabaseConfigSource::new(db);
    for _ in 0..5 {
        source
            .load_for_block("wafer-run/auth", &[optional("WAFER_RUN__AUTH__K")])
            .await
            .expect("resolves");
    }
    assert_eq!(counting.lists(), 1);
}

/// A block declaring no config keys must not touch the database at all.
/// `wafer-run` calls `load_for_block` for EVERY block regardless of whether
/// it declares any keys, so without this the first config-less block still
/// triggered the read.
#[tokio::test]
async fn a_block_with_no_declared_keys_issues_no_query() {
    let _generation = GENERATION.lock().await;
    let (counting, db) = migrated().await;
    let source = DatabaseConfigSource::new(db);

    let config = source
        .load_for_block("wafer-run/cors", &[])
        .await
        .expect("nothing to resolve");
    assert!(config.into_inner().is_empty());
    assert_eq!(
        counting.lists(),
        0,
        "a block declaring no keys has nothing to resolve, so the snapshot must not be read on its behalf"
    );
}

/// A pre-migration-002 table must not memoize its own emptiness.
///
/// Before that migration the variables table has no `block` column, so every
/// row is dropped by the grouping and the snapshot resolves to nothing. The
/// migration then ALTERs and backfills through `migration_helper`, which is
/// raw SQL: it never reaches a write path, so it records no config write and
/// cannot invalidate anything. The old per-block query tolerated this by
/// returning empty PER CALL, so blocks initialized after the migration
/// re-queried and saw the backfilled rows; memoizing would make the emptiness
/// permanent for the whole boot.
///
/// Here over a real admin 001 table, and a real 002 `ALTER` and backfill.
#[tokio::test]
async fn rows_without_a_block_column_are_not_memoized_as_empty() {
    let _generation = GENERATION.lock().await;
    let (counting, db) = database();
    run_raw(&db, 0).await;
    // A row as the pre-002 seeder wrote it: no `block` column to write.
    let mut row = NewVariable {
        key: "WAFER_RUN__AUTH__A".to_string(),
        value: "backfilled".to_string(),
        name: String::new(),
        description: String::new(),
        warning: String::new(),
        sensitive: false,
        updated_by: String::new(),
        block: None,
    }
    .into_row()
    .to_data();
    row.remove("block");
    db.create(variables::TABLE, row)
        .await
        .expect("seed a pre-002 row");
    let source = DatabaseConfigSource::new(db.clone());

    let before = source
        .load_for_block("wafer-run/auth", &[optional("WAFER_RUN__AUTH__A")])
        .await
        .expect("resolves on defaults");
    assert_eq!(before.get("WAFER_RUN__AUTH__A"), None);
    assert_eq!(counting.lists(), 1);

    // Migration 002 lands out of band, as raw SQL that records nothing —
    // which is how the runner sends it too.
    run_raw(&db, 1).await;

    let after = source
        .load_for_block("wafer-run/auth", &[optional("WAFER_RUN__AUTH__A")])
        .await
        .expect("resolves");
    assert_eq!(
        after.get("WAFER_RUN__AUTH__A"),
        Some("backfilled"),
        "a snapshot that grouped to nothing must be re-read, not trusted"
    );
    assert_eq!(counting.lists(), 2);
}

/// A table holding only shared rows groups to nothing too, and is not
/// memoized either: the pre-002 rule above cannot tell the two apart, so every
/// config-declaring block's load re-reads such a table — one read per block,
/// the cost a rule keyed on the record's shape would remove.
#[tokio::test]
async fn an_all_shared_table_is_reread_for_every_block() {
    let _generation = GENERATION.lock().await;
    let (counting, db) = migrated().await;
    seed(&db, "WAFER_RUN_SHARED__APP_NAME", "shared-value").await;
    let source = DatabaseConfigSource::new(db);
    for block in ["wafer-run/auth", "impresspress/email", "impresspress/files"] {
        source
            .load_for_block(block, &[optional("WAFER_RUN__AUTH__A")])
            .await
            .expect("resolves");
    }
    assert_eq!(counting.lists(), 3, "one read per config-declaring block");
}

/// Truncation must FAIL, not just log.
///
/// The old per-block query gave each block its own 10k budget; one
/// unfiltered read shares a single budget across the whole table. If it ever
/// fills, rows are dropped arbitrarily — and because `skip_count` is set
/// there is no `total_count` to notice it by. Continuing would build a
/// runtime from config known to be incomplete. Over real rows: one short of
/// the limit resolves, the limit itself refuses.
#[tokio::test]
async fn a_full_page_fails_the_load_instead_of_serving_partial_config() {
    let _generation = GENERATION.lock().await;
    let limit = impresspress_core::cache_key::full_table_list_opts()
        .limit
        .expect("the full-table read is bounded") as usize;
    let (_counting, db) = migrated().await;
    seed(&db, "WAFER_RUN__AUTH__K", "v").await;
    let filler: Vec<_> = (1..limit - 1)
        .map(|n| {
            NewVariable {
                key: format!("FILLER_{n}"),
                value: String::new(),
                name: String::new(),
                description: String::new(),
                warning: String::new(),
                sensitive: false,
                updated_by: String::new(),
                block: None,
            }
            .into_row()
            .to_data()
        })
        .collect();
    for chunk in filler.chunks(500) {
        db.create_many(variables::TABLE, chunk.to_vec())
            .await
            .expect("seed filler rows");
    }
    let source = DatabaseConfigSource::new(db.clone());
    let declared = [optional("WAFER_RUN__AUTH__K")];

    let below = source
        .load_for_block("wafer-run/auth", &declared)
        .await
        .expect("one row short of the limit is a complete page");
    assert_eq!(below.get("WAFER_RUN__AUTH__K"), Some("v"));

    seed(&db, "FILLER_LAST", "").await;
    note_config_write();
    let err = source
        .load_for_block("wafer-run/auth", &declared)
        .await
        .expect_err("incomplete config must not resolve");
    let ConfigError::Transient { block, source } = &err else {
        panic!("must be Transient so the block slot does not cache it permanently: {err:?}");
    };
    assert_eq!(block, "wafer-run/auth");
    assert!(
        source.to_string().contains("may be truncated"),
        "the refusal says why: {source}"
    );
}

/// A failed read of the table is `Transient` — never defaults — and is not
/// remembered: the next load reads again. Over a real failure, the table not
/// existing yet.
#[tokio::test]
async fn a_failed_read_is_transient_and_not_remembered() {
    let _generation = GENERATION.lock().await;
    let (counting, db) = database();
    let source = DatabaseConfigSource::new(db.clone());
    let declared = [ConfigVar::new("WAFER_RUN__AUTH__K", "doc", "the-default")];

    let err = source
        .load_for_block("wafer-run/auth", &declared)
        .await
        .expect_err("an unreadable table must not resolve to the default");
    let ConfigError::Transient {
        block,
        source: cause,
    } = &err
    else {
        panic!("a failed read is Transient: {err:?}");
    };
    assert_eq!(block, "wafer-run/auth");
    assert!(
        cause.to_string().contains("no such table"),
        "the database's own text reaches the error: {cause}"
    );

    migrate(&db).await;
    seed(&db, "WAFER_RUN__AUTH__K", "stored").await;
    let config = source
        .load_for_block("wafer-run/auth", &declared)
        .await
        .expect("the next load reads again");
    assert_eq!(config.get("WAFER_RUN__AUTH__K"), Some("stored"));
    assert_eq!(counting.lists(), 2);
}

/// The resolution ladder, end to end over stored rows: the overlay beats the
/// row, an empty value in either is no value, then the declared default, then
/// `MissingRequired` for a required key and omission for an optional one.
#[tokio::test]
async fn the_overlay_wins_then_the_row_then_the_default() {
    let _generation = GENERATION.lock().await;
    let (_counting, db) = migrated().await;
    for (key, value) in [
        ("WAFER_RUN__AUTH__OVERLAID", "from-db"),
        ("WAFER_RUN__AUTH__OVERLAY_EMPTY", "from-db"),
        ("WAFER_RUN__AUTH__ROW_EMPTY", ""),
        ("WAFER_RUN__AUTH__STORED", "from-db"),
    ] {
        seed(&db, key, value).await;
    }
    let overlay = HashMap::from([
        (
            "WAFER_RUN__AUTH__OVERLAID".to_string(),
            "from-overlay".to_string(),
        ),
        ("WAFER_RUN__AUTH__OVERLAY_EMPTY".to_string(), String::new()),
        (
            "WAFER_RUN__AUTH__SECRET".to_string(),
            "from-overlay".to_string(),
        ),
    ]);
    let source = DatabaseConfigSource::with_overlay(db, overlay);

    let config = source
        .load_for_block(
            "wafer-run/auth",
            &[
                ConfigVar::new("WAFER_RUN__AUTH__OVERLAID", "doc", "default"),
                ConfigVar::new("WAFER_RUN__AUTH__OVERLAY_EMPTY", "doc", "default"),
                ConfigVar::new("WAFER_RUN__AUTH__ROW_EMPTY", "doc", "default"),
                ConfigVar::new("WAFER_RUN__AUTH__STORED", "doc", "default"),
                ConfigVar::new("WAFER_RUN__AUTH__UNSTORED", "doc", "default"),
                // Required, no row, no default: the overlay supplies it.
                ConfigVar::new("WAFER_RUN__AUTH__SECRET", "doc", ""),
                optional("WAFER_RUN__AUTH__ABSENT"),
            ],
        )
        .await
        .expect("every required key is supplied");
    assert_eq!(
        config.get("WAFER_RUN__AUTH__OVERLAID"),
        Some("from-overlay")
    );
    assert_eq!(
        config.get("WAFER_RUN__AUTH__OVERLAY_EMPTY"),
        Some("from-db"),
        "an empty overlay value falls through to the row"
    );
    assert_eq!(
        config.get("WAFER_RUN__AUTH__ROW_EMPTY"),
        Some("default"),
        "an empty row value falls through to the default"
    );
    assert_eq!(config.get("WAFER_RUN__AUTH__STORED"), Some("from-db"));
    assert_eq!(config.get("WAFER_RUN__AUTH__UNSTORED"), Some("default"));
    assert_eq!(config.get("WAFER_RUN__AUTH__SECRET"), Some("from-overlay"));
    assert_eq!(config.get("WAFER_RUN__AUTH__ABSENT"), None);

    let err = source
        .load_for_block(
            "wafer-run/auth",
            &[ConfigVar::new("WAFER_RUN__AUTH__MISSING", "doc", "")],
        )
        .await
        .expect_err("a required key nothing supplies");
    assert!(
        matches!(err, ConfigError::MissingRequired { ref key, .. } if key == "WAFER_RUN__AUTH__MISSING"),
        "{err:?}"
    );
}
