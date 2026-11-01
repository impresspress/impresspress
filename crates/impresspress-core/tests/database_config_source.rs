//! [`DatabaseConfigSource`] over native SQLite, through the admin migrations
//! production applies: the source Cloudflare resolves block config with,
//! proven on the backend a native test and the D1 cost harness can run. And
//! the runtime's [`VariablesSnapshot`] it reads through, which the config
//! block shares: its memo, its failure memo and backoff, and the fail-closed
//! rule both of its readers follow.
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
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc, LazyLock,
    },
};

use impresspress_core::{
    blocks::{
        admin::{migrations::MIGRATIONS, ADMIN_BLOCK_ID},
        config::{get_many, VariablesConfigBlock},
    },
    config_generation::note_config_write,
    config_source::{DatabaseConfigSource, RetryMode, VariablesSnapshot},
    migration_helper::executable_statements,
    platform_state::variables::{self, NewVariable},
    test_support::{
        d1_shaped::{D1Shaped, Fault, Ledger},
        migrate_sqlite, TestContext,
    },
};
use wafer_block::ConfigVar;
use wafer_core::interfaces::database::service::{DatabaseService, RecordList};
use wafer_run::{ConfigError, ConfigSource, ErrorCode};

/// Held by every test for its whole body: see the module doc.
static GENERATION: LazyLock<futures::lock::Mutex<()>> =
    LazyLock::new(|| futures::lock::Mutex::new(()));

/// Counts the reads of the variables table that reach the database, and how
/// many of them carried a filter.
struct Counting {
    inner: Arc<dyn DatabaseService>,
    lists: AtomicUsize,
    filtered_lists: AtomicUsize,
    /// Fail every list of the variables table, and nothing else: the table
    /// is there, and asking the schema says so.
    fail_lists: std::sync::atomic::AtomicBool,
    /// Park the next unfiltered list of the variables table AFTER it has read
    /// its rows: say so on the first sender, then wait on the receiver. An
    /// in-flight read, held open for a write to land under it.
    park: std::sync::Mutex<Option<Park>>,
}

type Park = (
    futures::channel::oneshot::Sender<()>,
    futures::channel::oneshot::Receiver<()>,
);

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
                if self.fail_lists.load(Ordering::SeqCst) {
                    return Err(
                        wafer_core::interfaces::database::service::DatabaseError::Internal(
                            "injected list failure".to_string(),
                        ),
                    );
                }
                if opts.filters.is_empty() && opts.filter_tree.is_none() {
                    let park = self.park.lock().unwrap_or_else(|p| p.into_inner()).take();
                    if let Some((read, release)) = park {
                        let listed = self.inner.list(collection, opts).await;
                        read.send(()).expect("the test waits for the read");
                        release.await.expect("the test releases the read");
                        return listed;
                    }
                }
            }
            self.inner.list(collection, opts).await
        }
    }
}

/// A fresh in-memory SQLite database, behind a [`Counting`] decorator.
fn database() -> (Arc<Counting>, Arc<dyn DatabaseService>) {
    counted(Arc::new(
        wafer_block_sqlite::service::SQLiteDatabaseService::open_in_memory()
            .expect("open in-memory sqlite"),
    ))
}

fn counted(inner: Arc<dyn DatabaseService>) -> (Arc<Counting>, Arc<dyn DatabaseService>) {
    let counting = Arc::new(Counting {
        inner,
        lists: AtomicUsize::new(0),
        filtered_lists: AtomicUsize::new(0),
        fail_lists: std::sync::atomic::AtomicBool::new(false),
        park: std::sync::Mutex::new(None),
    });
    let db: Arc<dyn DatabaseService> = counting.clone();
    (counting, db)
}

/// A database at today's schema behind [`D1Shaped`] — whose fault switch
/// fails the variables table as D1 would — behind a [`Counting`] decorator,
/// which counts a failed list too.
async fn shaped() -> (Arc<Counting>, Arc<D1Shaped>, Arc<dyn DatabaseService>) {
    let shaped = Arc::new(D1Shaped::new(
        wafer_block_sqlite::service::SQLiteDatabaseService::open_in_memory()
            .expect("open in-memory sqlite"),
        Arc::new(Ledger::default()),
    ));
    let (counting, db) = counted(shaped.clone());
    migrate(&db).await;
    (counting, shaped, db)
}

/// A clock a test moves by hand, in milliseconds.
#[derive(Clone, Default)]
struct Clock(Arc<AtomicU64>);

impl Clock {
    fn set(&self, ms: u64) {
        self.0.store(ms, Ordering::SeqCst);
    }

    /// A snapshot measuring its retry times on this clock.
    fn snapshot(&self, retry: RetryMode) -> Arc<VariablesSnapshot> {
        let now = Arc::clone(&self.0);
        Arc::new(VariablesSnapshot::with_clock(retry, move || {
            now.load(Ordering::SeqCst)
        }))
    }
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
    let source = DatabaseConfigSource::new(db, Arc::default());

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
    let source = DatabaseConfigSource::new(db, Arc::default());

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
/// initializes first, its `Init` runs `settings::seed_defaults` on a
/// provisioning build, and the Cloudflare deploy hook then runs
/// `seed_auto_generated` — all after the database
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
    let source = DatabaseConfigSource::new(db.clone(), Arc::default());

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

/// A write that lands while a read is in flight is not masked by that read.
///
/// The snapshot tags what it read with the config-write generation captured
/// BEFORE the read. Here the read has its rows when an admin write commits
/// and bumps the generation; it then returns and is stored under the old
/// generation, so the next load re-reads and sees the write. Captured after
/// the read instead, the pre-write rows would be stored under the new
/// generation and served until the next write or a restart — the admin's
/// "Settings saved" silently not served.
#[tokio::test]
async fn a_write_during_an_in_flight_read_is_not_masked_by_it() {
    let _generation = GENERATION.lock().await;
    let (counting, db) = migrated().await;
    seed(&db, "WAFER_RUN__AUTH__K", "before").await;
    let source = DatabaseConfigSource::new(db.clone(), Arc::default());
    let declared = [optional("WAFER_RUN__AUTH__K")];
    let (read_tx, read_rx) = futures::channel::oneshot::channel();
    let (release_tx, release_rx) = futures::channel::oneshot::channel();
    *counting.park.lock().expect("park") = Some((read_tx, release_rx));

    let in_flight = source.load_for_block("wafer-run/auth", &declared);
    let write = async {
        read_rx.await.expect("the read has its rows");
        // The real write path, which records the config write itself.
        variables::set(&db, "WAFER_RUN__AUTH__K", "after", "", "", None)
            .await
            .expect("the write lands");
        release_tx.send(()).expect("the read is parked");
    };
    let (raced, ()) = futures::join!(in_flight, write);
    assert_eq!(
        raced.expect("resolves").get("WAFER_RUN__AUTH__K"),
        Some("before"),
        "precondition: the read had its rows before the write"
    );

    let next = source
        .load_for_block("wafer-run/auth", &declared)
        .await
        .expect("resolves");
    assert_eq!(
        next.get("WAFER_RUN__AUTH__K"),
        Some("after"),
        "a write that raced a read must not be masked by it"
    );
    assert_eq!(
        counting.lists() - counting.filtered_lists.load(Ordering::SeqCst),
        2,
        "the raced read, then one re-read"
    );
}

/// The re-read is driven by writes, not by every call. An established
/// database seeds nothing, so the whole pass must still cost one query.
#[tokio::test]
async fn no_config_write_means_no_reread() {
    let _generation = GENERATION.lock().await;
    let (counting, db) = migrated().await;
    seed(&db, "WAFER_RUN__AUTH__K", "v").await;
    let source = DatabaseConfigSource::new(db, Arc::default());
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
    let source = DatabaseConfigSource::new(db, Arc::default());

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
    let source = DatabaseConfigSource::new(db.clone(), Arc::default());

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

/// A table holding only shared rows groups to nothing too, but it is
/// memoized: the pre-002 rule is keyed on the record's shape, and a shared
/// row carries the `block` column (NULL). Keyed on the grouping instead, every
/// config-declaring block's load re-read such a table — (1 + M) × N rows per
/// build, M config-declaring blocks — and so did every `CONFIG_GET`.
#[tokio::test]
async fn an_all_shared_table_is_read_once_for_every_block() {
    let _generation = GENERATION.lock().await;
    let (counting, db) = migrated().await;
    seed(&db, "WAFER_RUN_SHARED__APP_NAME", "shared-value").await;
    let source = DatabaseConfigSource::new(db, Arc::default());
    for block in ["wafer-run/auth", "impresspress/email", "impresspress/files"] {
        source
            .load_for_block(block, &[optional("WAFER_RUN__AUTH__A")])
            .await
            .expect("resolves");
    }
    assert_eq!(
        counting.lists(),
        1,
        "shared rows carry a NULL `block`: the table has the column, so the read is remembered"
    );
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
    let source = DatabaseConfigSource::new(db.clone(), Arc::default());
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

/// A table that does not exist yet — a fresh database, before admin's
/// migrations — is `Transient` for the source, never the default, carrying the
/// database's own text; and it is not remembered, so once the migrations have
/// run (raw SQL, recording no config write) the next load reads the table.
/// A required key the deploy hook seeds after admin's `Init` would otherwise
/// be a permanent `MissingRequired`.
#[tokio::test]
async fn a_table_not_created_yet_is_transient_and_not_remembered() {
    let _generation = GENERATION.lock().await;
    let (counting, db) = database();
    let source = DatabaseConfigSource::new(db.clone(), Arc::default());
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
    let typed = cause
        .downcast_ref::<variables::UnfilteredReadError>()
        .expect("the read error stays typed behind the Transient");
    assert!(
        matches!(typed, variables::UnfilteredReadError::Absent(_)),
        "{typed:?}"
    );
    assert_eq!(
        typed.database_error().map(|e| e.code()),
        Some(ErrorCode::Internal)
    );

    migrate(&db).await;
    seed(&db, "WAFER_RUN__AUTH__K", "stored").await;
    let config = source
        .load_for_block("wafer-run/auth", &declared)
        .await
        .expect("the next load reads the created table");
    assert_eq!(config.get("WAFER_RUN__AUTH__K"), Some("stored"));
    assert_eq!(counting.lists(), 2);
}

/// The config block's side of a table that does not exist yet: nothing can be
/// stored, so `CONFIG_GET` reads on to the boot map — what admin's own `Init`
/// needs on a fresh database, where it reads its backend before its
/// migrations create the table. Not a failed read, so not `Unavailable`.
#[tokio::test]
async fn before_the_table_exists_config_get_reads_on_to_the_boot_map() {
    let _generation = GENERATION.lock().await;
    let (_counting, db) = database();
    let snapshot = Arc::new(VariablesSnapshot::new(RetryMode::Backoff));
    let ctx = context_with_config_block(
        &db,
        &snapshot,
        &[("WAFER_RUN_SHARED__DATABASE__BACKEND", "sqlite")],
    )
    .await;

    assert_eq!(
        wafer_core::clients::config::get_default(
            &ctx,
            "WAFER_RUN_SHARED__DATABASE__BACKEND",
            "default"
        )
        .await
        .expect("no table is no stored config"),
        "sqlite"
    );
    assert_eq!(
        wafer_core::clients::config::get_optional(&ctx, "WAFER_RUN_SHARED__UNSET")
            .await
            .expect("no table is no stored config"),
        None
    );
}

/// A list that fails while the table IS there is a failed read, not the cold
/// start: the schema is asked only after a failed list, and only a definite
/// "absent" is one. Remembered, and `Unavailable` to the config block.
#[tokio::test]
async fn a_failed_list_of_a_present_table_is_a_failure_not_the_cold_start() {
    let _generation = GENERATION.lock().await;
    let (counting, db) = migrated().await;
    seed(&db, "WAFER_RUN_SHARED__APP_NAME", "stored").await;
    let snapshot = Arc::new(VariablesSnapshot::new(RetryMode::Backoff));
    let ctx = context_with_config_block(
        &db,
        &snapshot,
        &[("WAFER_RUN_SHARED__APP_NAME", "from-boot")],
    )
    .await;
    counting.fail_lists.store(true, Ordering::SeqCst);

    let err = wafer_core::clients::config::get_default(&ctx, "WAFER_RUN_SHARED__APP_NAME", "")
        .await
        .expect_err("a failed list of a present table is unavailable");
    assert_eq!(err.code, ErrorCode::Unavailable, "{err}");
    assert!(err.message.contains("injected list failure"), "{err}");

    counting.fail_lists.store(false, Ordering::SeqCst);
    wafer_core::clients::config::get_default(&ctx, "WAFER_RUN_SHARED__APP_NAME", "")
        .await
        .expect_err("and remembered until its retry");
    assert_eq!(counting.lists(), 1);
}

/// The failure memo: a failed read is remembered and replayed to every load
/// with NO query until its retry time — 1 s after the first failure, doubling
/// — and then read again. A healthy database in the meantime does not end it
/// early (only a config write does, below): the snapshot does not know the
/// database recovered until it reads.
#[tokio::test]
async fn a_failed_read_is_remembered_until_its_retry_then_read_again() {
    let _generation = GENERATION.lock().await;
    let (counting, shaped, db) = shaped().await;
    seed(&db, "WAFER_RUN__AUTH__K", "stored").await;
    let clock = Clock::default();
    let source = DatabaseConfigSource::new(db, clock.snapshot(RetryMode::Backoff));
    let declared = [ConfigVar::new("WAFER_RUN__AUTH__K", "doc", "the-default")];
    let load = || source.load_for_block("wafer-run/auth", &declared);

    shaped.fail_table(variables::TABLE, Fault::Unavailable);
    let err = load().await.expect_err("the outage fails the load");
    assert!(
        matches!(&err, ConfigError::Transient { source, .. } if source.to_string().contains("injected outage")),
        "{err:?}"
    );
    assert_eq!(counting.lists(), 1);

    // Before the retry: the same failure, and no query — for every block.
    clock.set(999);
    shaped.clear_fault();
    for block in ["wafer-run/auth", "impresspress/email"] {
        let err = source
            .load_for_block(block, &declared)
            .await
            .expect_err("a remembered failure is replayed, never the default");
        assert!(
            matches!(&err, ConfigError::Transient { block: b, source } if b == block && source.to_string().contains("injected outage")),
            "{err:?}"
        );
    }
    assert_eq!(counting.lists(), 1, "no query before the retry time");

    // The second consecutive failure doubles the wait: 2 s from now.
    shaped.fail_table(variables::TABLE, Fault::Unavailable);
    clock.set(1_000);
    load().await.expect_err("the retry reads, and fails again");
    assert_eq!(counting.lists(), 2, "one read at the retry time");
    shaped.clear_fault();
    clock.set(2_999);
    load().await.expect_err("still remembered");
    assert_eq!(counting.lists(), 2);

    clock.set(3_000);
    let config = load().await.expect("the retry reads a healthy table");
    assert_eq!(config.get("WAFER_RUN__AUTH__K"), Some("stored"));
    assert_eq!(counting.lists(), 3);
    load().await.expect("and remembers it");
    assert_eq!(counting.lists(), 3);
}

/// A config write invalidates a remembered failure exactly as it invalidates a
/// loaded snapshot: the next load reads at once, retry time or not.
#[tokio::test]
async fn a_config_write_invalidates_a_remembered_failure() {
    let _generation = GENERATION.lock().await;
    let (counting, shaped, db) = shaped().await;
    let clock = Clock::default();
    let source = DatabaseConfigSource::new(db.clone(), clock.snapshot(RetryMode::Backoff));
    let declared = [optional("WAFER_RUN__AUTH__K")];

    shaped.fail_table(variables::TABLE, Fault::Unavailable);
    source
        .load_for_block("wafer-run/auth", &declared)
        .await
        .expect_err("the outage fails the load");
    shaped.clear_fault();

    // The real write path, which records the config write itself.
    variables::set(&db, "WAFER_RUN__AUTH__K", "written", "", "", None)
        .await
        .expect("the write lands");
    let config = source
        .load_for_block("wafer-run/auth", &declared)
        .await
        .expect("the write invalidated the failure");
    assert_eq!(config.get("WAFER_RUN__AUTH__K"), Some("written"));
    assert_eq!(
        counting.lists(),
        3,
        "the failed read, the write's own lookup, and one re-read at once"
    );
}

/// `RetryMode::Rebuild`: the snapshot never retries on its own — the runtime
/// that owns it retries by rebuilding, which brings a new snapshot. Only a
/// config write reads again.
#[tokio::test]
async fn rebuild_mode_never_retries_on_its_own() {
    let _generation = GENERATION.lock().await;
    let (counting, shaped, db) = shaped().await;
    let clock = Clock::default();
    let source = DatabaseConfigSource::new(db, clock.snapshot(RetryMode::Rebuild));
    let declared = [optional("WAFER_RUN__AUTH__K")];

    shaped.fail_table(variables::TABLE, Fault::Unavailable);
    source
        .load_for_block("wafer-run/auth", &declared)
        .await
        .expect_err("the outage fails the load");
    shaped.clear_fault();
    clock.set(u64::MAX / 2);
    source
        .load_for_block("wafer-run/auth", &declared)
        .await
        .expect_err("no self-retry, however long it has been");
    assert_eq!(counting.lists(), 1);

    note_config_write();
    source
        .load_for_block("wafer-run/auth", &declared)
        .await
        .expect("a config write still reads again");
    assert_eq!(counting.lists(), 2);
}

/// The config block and the config source share their runtime's snapshot:
/// one read serves every block's `Init` AND every `CONFIG_GET` after it.
#[tokio::test]
async fn the_config_block_and_the_source_share_one_read() {
    let _generation = GENERATION.lock().await;
    let (counting, _shaped, db) = shaped().await;
    seed(&db, "WAFER_RUN__AUTH__K", "block-row").await;
    seed(&db, "WAFER_RUN_SHARED__APP_NAME", "shared-row").await;
    let snapshot = Arc::new(VariablesSnapshot::new(RetryMode::Backoff));
    let source = DatabaseConfigSource::new(db.clone(), snapshot.clone());
    let ctx = context_with_config_block(&db, &snapshot, &[]).await;

    source
        .load_for_block("wafer-run/auth", &[optional("WAFER_RUN__AUTH__K")])
        .await
        .expect("resolves");
    for _ in 0..3 {
        assert_eq!(
            wafer_core::clients::config::get_default(&ctx, "WAFER_RUN_SHARED__APP_NAME", "")
                .await
                .expect("config read"),
            "shared-row"
        );
    }
    assert_eq!(counting.lists(), 1, "one read for the source and the block");
}

/// THE fail-closed rule, through both readers of a REMEMBERED failure — with
/// no query: `load_for_block` is `Transient`, and `CONFIG_GET` of a table key
/// answers `Unavailable` — not the boot map's value for a key it holds, not
/// `NotFound` for a key it lacks (which `config::get_default` would turn into
/// the declared default) — and `get_default` returns that error. So does
/// `CONFIG_GET_MANY`.
#[tokio::test]
async fn a_remembered_failure_is_unavailable_never_a_default_or_the_boot_map() {
    let _generation = GENERATION.lock().await;
    let (counting, shaped, db) = shaped().await;
    seed(&db, "WAFER_RUN_SHARED__APP_NAME", "stored").await;
    let clock = Clock::default();
    let snapshot = clock.snapshot(RetryMode::Backoff);
    let source = DatabaseConfigSource::new(db.clone(), snapshot.clone());
    let ctx = context_with_config_block(
        &db,
        &snapshot,
        &[("WAFER_RUN_SHARED__APP_NAME", "from-boot")],
    )
    .await;

    shaped.fail_table(variables::TABLE, Fault::Unavailable);
    let err = source
        .load_for_block(
            "wafer-run/auth",
            &[ConfigVar::new("WAFER_RUN__AUTH__K", "doc", "the-default")],
        )
        .await
        .expect_err("the outage fails the load");
    assert!(matches!(err, ConfigError::Transient { .. }), "{err:?}");
    assert_eq!(counting.lists(), 1);
    shaped.clear_fault();

    let in_boot_map =
        wafer_core::clients::config::get_default(&ctx, "WAFER_RUN_SHARED__APP_NAME", "default")
            .await
            .expect_err("never the boot map's value");
    assert_eq!(in_boot_map.code, ErrorCode::Unavailable, "{in_boot_map}");
    assert!(
        in_boot_map.message.contains("injected outage"),
        "{in_boot_map}"
    );
    let not_in_boot_map =
        wafer_core::clients::config::get_default(&ctx, "WAFER_RUN_SHARED__UNSET", "default")
            .await
            .expect_err("never NotFound, so never the default");
    assert_eq!(not_in_boot_map.code, ErrorCode::Unavailable);
    let optional = wafer_core::clients::config::get_optional(&ctx, "WAFER_RUN_SHARED__UNSET")
        .await
        .expect_err("never `None`");
    assert_eq!(optional.code, ErrorCode::Unavailable);
    let many = get_many(&ctx, &["WAFER_RUN_SHARED__APP_NAME"])
        .await
        .expect_err("a form is not filled either");
    assert_eq!(many.code, ErrorCode::Unavailable);

    assert_eq!(
        counting.lists(),
        1,
        "every reader got the remembered failure with no query"
    );
}

/// A test context whose `wafer-run/config` is the production config block over
/// `db`, reading through `snapshot`, with `boot` as its boot map. Runs as the
/// admin block, which may read every config key.
async fn context_with_config_block(
    db: &Arc<dyn DatabaseService>,
    snapshot: &Arc<VariablesSnapshot>,
    boot: &[(&str, &str)],
) -> TestContext {
    let boot_map: Arc<dyn wafer_core::interfaces::config::service::ConfigService> =
        impresspress_core::builder::fill_config_service(
            Arc::new(wafer_core::service_blocks::config::EnvConfigService::new()),
            boot.iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                .collect(),
        );
    let mut ctx = TestContext::new().await.running_as(ADMIN_BLOCK_ID);
    ctx.register_block(
        "wafer-run/config",
        Arc::new(VariablesConfigBlock::new(
            boot_map,
            db.clone(),
            snapshot.clone(),
        )),
    );
    ctx
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
    let source = DatabaseConfigSource::with_overlay(db, overlay, Arc::default());

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
