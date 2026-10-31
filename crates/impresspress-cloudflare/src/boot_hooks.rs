//! The hosted boot hooks, driven over Cloudflare's own database wrapper.
//!
//! The three `BootHooks` impls the Cloudflare funnels pick between —
//! `DeployBootHooks`, `RequestBootHooks` and `PreparedPlanBootHooks` — live in
//! `impresspress_core::builder` beside the funnels that construct them (see
//! that module's `request_path` docs for why each path gets which). What is
//! Cloudflare's is the KV row cache in front of D1 and the config-version
//! bump a write through it causes, so these tests stay here: they drive the
//! hooks' database halves through the real
//! [`kv_cached_db::KvCachedD1DatabaseService`](crate::kv_cached_db::KvCachedD1DatabaseService)
//! the request path runs with.

/// Tests for the two database-touching boot hooks' behaviour on Cloudflare.
///
/// `seed_after_admin_init` takes `&mut wafer_run::Wafer`, which no wasm unit
/// test can produce, so these exercise the hooks' database halves —
/// `RequestBootHooks::load` and `DeployBootHooks::seed_and_load` — through the
/// real [`kv_cached_db::KvCachedD1DatabaseService`] wrapper the request path
/// runs with, over a database that records every mutation.
///
/// The assertions are about writes that did or did not reach the database and
/// the KV config-version stamp, never about which function was called: a test
/// that asserts a call was not made passes just as happily when the call moves
/// somewhere else.
mod boot_hook_tests {
    use std::{cell::RefCell, collections::HashMap, sync::Arc};

    use impresspress_core::{
        builder::{DeployBootHooks, RequestBootHooks},
        cache_key,
        features::FeatureConfig,
        kv::KvBackend,
    };
    use wafer_block::db::{Filter, ListOptions};
    use wafer_core::interfaces::database::service::{
        AggregateSpec, Column, DatabaseError, DatabaseService, Record, RecordList, Table,
        UpsertSpec,
    };
    use wasm_bindgen_test::wasm_bindgen_test;

    use crate::{kv_cached_db, request_services};

    /// A `DatabaseService` over an EMPTY `block_settings` table — the state a
    /// first-ever deploy is in, and the one where the seed planner plans an
    /// `Insert` for every block. Records every mutation it is asked to make.
    #[derive(Default)]
    struct RecordingDb {
        writes: RefCell<Vec<String>>,
        /// Rows `list` answers with, per collection. Empty by default, which
        /// is the first-ever-deploy state every other test here wants.
        rows: RefCell<HashMap<String, Vec<Record>>>,
    }

    impl RecordingDb {
        fn note(&self, write: String) {
            self.writes.borrow_mut().push(write);
        }

        /// Put one row in `collection` for `list` to return.
        fn with_row(self, collection: &str, record: Record) -> Self {
            self.rows
                .borrow_mut()
                .entry(collection.to_string())
                .or_default()
                .push(record);
            self
        }
    }

    #[wafer_block::wafer_async_trait]
    impl DatabaseService for RecordingDb {
        async fn get(&self, _collection: &str, _id: &str) -> Result<Record, DatabaseError> {
            Err(DatabaseError::NotFound)
        }

        async fn list(
            &self,
            collection: &str,
            opts: &ListOptions,
        ) -> Result<RecordList, DatabaseError> {
            // Equality filters are honoured, which is not decoration: every
            // `variables::find_by_key` is a `list` with one `key` equality
            // filter, so a fixture that ignored them answered EVERY lookup
            // with the single seeded row. `seed_auto_generated` then believed
            // each declared secret already existed and wrote nothing — the
            // fixture quietly disabling most of the hook it exists to
            // exercise.
            let records: Vec<Record> = self
                .rows
                .borrow()
                .get(collection)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|record| {
                    opts.filters.iter().all(|filter| {
                        !matches!(filter.operator, wafer_block::db::FilterOp::Equal)
                            || record.data.get(&filter.field) == Some(&filter.value)
                    })
                })
                .collect();
            let total_count = i64::try_from(records.len()).unwrap_or(i64::MAX);
            Ok(RecordList {
                records,
                total_count,
                page: 1,
                page_size: 500,
            })
        }

        async fn create(
            &self,
            collection: &str,
            _data: HashMap<String, serde_json::Value>,
        ) -> Result<Record, DatabaseError> {
            self.note(format!("create {collection}"));
            Ok(Record {
                id: "created".to_string(),
                data: Default::default(),
            })
        }

        async fn update(
            &self,
            collection: &str,
            id: &str,
            _data: HashMap<String, serde_json::Value>,
        ) -> Result<Record, DatabaseError> {
            self.note(format!("update {collection}/{id}"));
            Ok(Record {
                id: id.to_string(),
                data: Default::default(),
            })
        }

        async fn delete(&self, collection: &str, id: &str) -> Result<(), DatabaseError> {
            self.note(format!("delete {collection}/{id}"));
            Ok(())
        }

        async fn count(
            &self,
            _collection: &str,
            _filters: &[Filter],
        ) -> Result<i64, DatabaseError> {
            Ok(0)
        }

        async fn sum(
            &self,
            _collection: &str,
            _field: &str,
            _filters: &[Filter],
        ) -> Result<f64, DatabaseError> {
            Ok(0.0)
        }

        async fn increment_field_where(
            &self,
            collection: &str,
            _col: &str,
            _delta: i64,
            _filters: &[Filter],
        ) -> Result<i64, DatabaseError> {
            self.note(format!("increment_field_where {collection}"));
            Ok(0)
        }

        async fn query_raw(
            &self,
            _query: &str,
            _args: &[serde_json::Value],
        ) -> Result<Vec<Record>, DatabaseError> {
            unreachable!("block_settings never goes through raw SQL")
        }

        async fn exec_raw(
            &self,
            query: &str,
            _args: &[serde_json::Value],
        ) -> Result<i64, DatabaseError> {
            self.note(format!("exec_raw {query}"));
            Ok(0)
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
            collection: &str,
            _spec: UpsertSpec,
        ) -> Result<Option<Record>, DatabaseError> {
            self.note(format!("upsert {collection}"));
            Ok(None)
        }

        async fn aggregate(
            &self,
            _collection: &str,
            _spec: AggregateSpec,
        ) -> Result<Vec<Record>, DatabaseError> {
            Ok(Vec::new())
        }

        async fn ensure_schema_table(&self, table: &Table) -> Result<(), DatabaseError> {
            self.note(format!("ensure_schema_table {}", table.name));
            Ok(())
        }

        async fn schema_table_exists(&self, _name: &str) -> Result<bool, DatabaseError> {
            Ok(true)
        }

        async fn schema_columns(&self, _table: &str) -> Result<Vec<String>, DatabaseError> {
            Ok(Vec::new())
        }

        async fn schema_drop_table(&self, name: &str) -> Result<(), DatabaseError> {
            self.note(format!("schema_drop_table {name}"));
            Ok(())
        }

        async fn schema_add_column(
            &self,
            table: &str,
            column: &Column,
        ) -> Result<(), DatabaseError> {
            self.note(format!("schema_add_column {table}.{}", column.name));
            Ok(())
        }

        fn statement_budget(
            &self,
        ) -> Result<wafer_core::interfaces::database::service::StatementBudget, DatabaseError>
        {
            Ok(wafer_core::interfaces::database::service::StatementBudget::Unbounded)
        }
    }

    /// KV backend that records every key it is asked to write, so the
    /// config-generation bump is observable.
    #[derive(Default)]
    struct RecordingKv {
        puts: RefCell<Vec<String>>,
    }

    #[async_trait::async_trait(?Send)]
    impl KvBackend for RecordingKv {
        async fn get(&self, _key: &str) -> Result<Option<String>, String> {
            Ok(None)
        }

        async fn put_with_ttl(
            &self,
            key: &str,
            _value: &str,
            _ttl_secs: u64,
        ) -> Result<(), String> {
            self.puts.borrow_mut().push(key.to_string());
            Ok(())
        }

        async fn put(&self, key: &str, _value: &str) -> Result<(), String> {
            self.puts.borrow_mut().push(key.to_string());
            Ok(())
        }

        async fn delete(&self, _key: &str) -> Result<(), String> {
            Ok(())
        }
    }

    /// The request path's exact database wiring: the KV row cache in front of
    /// D1 with `bump_on_write` ON, which is what turns any write to
    /// `block_settings` into a fleet-wide config-generation bump.
    fn request_path_db() -> (Arc<dyn DatabaseService>, Arc<RecordingDb>, Arc<RecordingKv>) {
        let inner = Arc::new(RecordingDb::default());
        let kv = Arc::new(RecordingKv::default());
        let service = kv_cached_db::KvCachedD1DatabaseService::with_mode(
            inner.clone() as Arc<dyn DatabaseService>,
            kv.clone() as Arc<dyn KvBackend>,
            kv_cached_db::CacheMode {
                read_through: true,
                bump_on_write: true,
            },
        );
        let service: Arc<dyn DatabaseService> = Arc::new(service);
        (service, inner, kv)
    }

    fn settings_handle() -> Arc<std::sync::RwLock<impresspress_core::features::BlockSettings>> {
        Arc::new(std::sync::RwLock::new(
            impresspress_core::features::BlockSettings::default(),
        ))
    }

    /// One block that ships enabled. Held here rather than taken from
    /// `blocks::block_enabled_defaults()` so the fixture proves the same thing
    /// in every feature build: this crate's default feature set has no
    /// `can_disable` block at all, so the real defaults list is empty there and
    /// a seeding call over it would write nothing for a reason that has nothing
    /// to do with the property under test.
    fn seed_defaults_fixture() -> Vec<(String, bool)> {
        vec![("impresspress/fixture".to_string(), true)]
    }

    /// **The deploy hook repairs a mis-flagged `sensitive` row, and the
    /// request hook does not.**
    ///
    /// The hosted target never runs `variables::seed_and_load` — this hook is
    /// the whole of its variables seeding — so the repair pass has to be named
    /// in it explicitly. Without that, a row an older build stored with the
    /// flag clear (a bootstrap credential is the likely one) keeps being
    /// served in the clear by `GET /b/admin/api/settings/{key}` on Cloudflare
    /// and stays eligible for the KV cache.
    ///
    /// Asserted as the `update` the repair issues against the variables table,
    /// not as "the function was called", so it keeps holding if the repair
    /// moves. The request-path half is the invariant control: that hook must
    /// stay write-free even for a repair.
    #[wasm_bindgen_test]
    async fn the_deploy_hook_repairs_a_mis_flagged_row_and_the_request_hook_does_not() {
        use impresspress_core::platform_state::variables;

        // A row an older build wrote: a declared `Password` var, flag clear.
        let unflagged = || {
            let mut data = HashMap::new();
            data.insert(
                "key".to_string(),
                serde_json::json!(
                    impresspress_core::blocks::auth::config::BOOTSTRAP_ADMIN_PASSWORD_KEY
                ),
            );
            data.insert("value".to_string(), serde_json::json!("hunter2"));
            data.insert("sensitive".to_string(), serde_json::json!(0));
            Record {
                id: "var_legacy".to_string(),
                data: data.into_iter().collect(),
            }
        };
        let repair_write = format!("update {}/var_legacy", variables::TABLE);

        let db = Arc::new(RecordingDb::default().with_row(variables::TABLE, unflagged()));
        let deploy = DeployBootHooks::seeding_towards(
            db.clone(),
            settings_handle(),
            request_services::config_proxy(),
            seed_defaults_fixture(),
        );
        deploy.seed_and_load().await.expect("deploy seed");
        assert!(
            db.writes.borrow().contains(&repair_write),
            "the deploy hook must repair a mis-flagged row; it wrote {:?}",
            db.writes.borrow(),
        );

        // Same fixture, request path: still write-free.
        let db = Arc::new(RecordingDb::default().with_row(variables::TABLE, unflagged()));
        let request = RequestBootHooks::new(
            db.clone(),
            settings_handle(),
            request_services::config_proxy(),
        );
        request.load().await.expect("request load");
        assert!(
            db.writes.borrow().is_empty(),
            "the request path must stay write-free even for a repair; it wrote {:?}",
            db.writes.borrow(),
        );
    }

    /// **The request path is physically write-free.** On the empty table a
    /// database that has never seen `/_deploy/init` is in — the state where
    /// the seed planner plans an `Insert` for every block — the request-path
    /// hook must issue none of them.
    ///
    /// Asserted as the absence of a database write and of a KV
    /// config-generation bump, not as the absence of a call. Those writes are
    /// what 500s a `serve --target cloudflare` session on a missing table,
    /// what makes every other isolate take a full dynamic rebuild, and what
    /// two concurrent isolates race into a unique-constraint failure; a test
    /// that only checked "the seeder was not called" would keep passing the
    /// moment the write arrived by another route.
    ///
    /// The second half is the anti-vacuity control: the SAME fixture, asked to
    /// seed, records writes. So "no writes" above is a fact about the request
    /// path, not about a fixture that cannot see one.
    ///
    /// Note which build catches a regression here. This crate's DEFAULT
    /// feature set enables no `can_disable` block, so
    /// `blocks::block_enabled_defaults()` is empty and a seeding hook wired
    /// back onto the request path would write nothing in that build and keep
    /// this green. The `--features full` lane is where it fails, which is why
    /// CI now runs this suite twice.
    #[wasm_bindgen_test]
    async fn the_request_path_hook_writes_nothing_to_the_database() {
        let (db, inner, kv) = request_path_db();
        let hooks = RequestBootHooks::new(
            db.clone(),
            settings_handle(),
            request_services::config_proxy(),
        );

        let settings = hooks
            .load()
            .await
            .expect("an empty block_settings table is not an error");

        assert!(
            inner.writes.borrow().is_empty(),
            "the request path must not write to the database during boot; it issued {:?}",
            inner.writes.borrow(),
        );
        assert!(
            !kv.puts
                .borrow()
                .iter()
                .any(|key| key == cache_key::CONFIG_VERSION_KEY),
            "a request-path boot must not bump the config generation: every other \
             isolate would take a full dynamic rebuild. KV writes: {:?}",
            kv.puts.borrow(),
        );
        // It still answers with usable settings: a block with no row reads as
        // enabled, which is why not seeding here is safe.
        assert!(
            settings.is_block_enabled(impresspress_core::blocks::admin::ADMIN_BLOCK_ID),
            "a block with no row must read as enabled",
        );

        // The control. Same database, same wrapper, the deploy funnel's hook.
        let deploy = DeployBootHooks::seeding_towards(
            db,
            settings_handle(),
            request_services::config_proxy(),
            seed_defaults_fixture(),
        );
        deploy
            .seed_and_load()
            .await
            .expect("seeding an empty table succeeds");
        let seeded = format!(
            "create {}",
            impresspress_core::platform_state::block_settings::TABLE
        );
        assert!(
            inner.writes.borrow().contains(&seeded),
            "the fixture must be able to observe the structural seed write, or \
             the assertion above proves nothing; it saw {:?}",
            inner.writes.borrow(),
        );
        assert!(
            kv.puts
                .borrow()
                .iter()
                .any(|key| key == cache_key::CONFIG_VERSION_KEY),
            "and it must be able to observe the config-generation bump that \
             write causes: {:?}",
            kv.puts.borrow(),
        );
    }

    /// **After the deploy funnel's admin `Init` and seed step, the cached
    /// whole `block_settings` table holds D1's rows, not the rows an older
    /// version cached.** That cached table, `cfg:v1:block_settings:__all__`,
    /// is what every version's cold build reads its migration state from;
    /// were it left holding the old rows — `blessed_hash` among their columns
    /// after admin migration 009 rebuilt the table without it, and admin's
    /// old record — a cold build would read them for up to the row cache's
    /// 24 h TTL.
    ///
    /// The run is the funnel's, in its order (`builder::boot` steps 3 and 4):
    /// the real admin block's `Init` applies its list, 009 included, over the
    /// KV row cache in deploy mode (reading through, no per-write stamp); then
    /// `DeployBootHooks::seed_and_load` reads and seeds `block_settings`
    /// through the same service, and `/_deploy/prepare`'s closing
    /// `block_settings::load` reads it once more. The database is one an
    /// older version migrated: admin's row records main's 2026-10-07 list
    /// from before the ledger, so its `Init` adopts 001-008 and runs 009, and
    /// KV already holds the rows as that version cached them, each with a
    /// `blessed_hash` the rebuilt table no longer has.
    ///
    /// Asserted on the value KV holds at the end, which is what a cold build
    /// reads, not on which step wrote it: the record's write drops the key
    /// and the read-through reads write it back from D1, and either alone
    /// would do.
    #[wasm_bindgen_test]
    async fn the_deploy_funnel_leaves_the_all_rows_key_holding_d1_s_rows() {
        use std::rc::Rc;

        use impresspress_core::{blocks::admin::ADMIN_BLOCK_ID, platform_state::block_settings};

        use crate::test_fakes::{
            admin_record_before_009, deploy_admin_runtime, settings_row, BlockSettingsD1, Log,
            MemoryKv,
        };

        const ALL_ROWS_KEY: &str = "cfg:v1:block_settings:__all__";

        let rows = vec![
            settings_row("bs_admin", ADMIN_BLOCK_ID, admin_record_before_009()),
            settings_row("bs_products", "impresspress/products", "products-list"),
            settings_row("bs_auth", "wafer-run/auth", "auth-list"),
        ];
        // What the older version cached: the same rows, each still carrying
        // the column 009 retires.
        let stale: Vec<Record> = rows
            .iter()
            .cloned()
            .map(|mut row| {
                row.data
                    .insert("blessed_hash".to_string(), serde_json::json!("blessed"));
                row
            })
            .collect();
        let mut cached = HashMap::new();
        cached.insert(
            ALL_ROWS_KEY.to_string(),
            serde_json::to_string(&stale).expect("serialize the cached rows"),
        );
        let log = Log::default();
        let kv = Arc::new(MemoryKv::new(Rc::clone(&log), cached));
        let d1 = Arc::new(BlockSettingsD1::new(Rc::clone(&log), rows));
        let db: Arc<dyn DatabaseService> =
            Arc::new(kv_cached_db::KvCachedD1DatabaseService::with_mode(
                d1.clone(),
                kv.clone() as Arc<dyn KvBackend>,
                kv_cached_db::CacheMode {
                    read_through: true,
                    bump_on_write: false,
                },
            ));

        let wafer = deploy_admin_runtime(db.clone(), admin_record_before_009()).await;
        wafer
            .init_block(ADMIN_BLOCK_ID)
            .await
            .expect("admin's Init applies its list");
        let deploy = DeployBootHooks::seeding_towards(
            db.clone(),
            settings_handle(),
            request_services::config_proxy(),
            seed_defaults_fixture(),
        );
        deploy.seed_and_load().await.expect("deploy seed");
        block_settings::load(&db)
            .await
            .expect("prepare's closing read");

        let cached = kv
            .entries
            .borrow()
            .get(ALL_ROWS_KEY)
            .cloned()
            .unwrap_or_else(|| panic!("KV holds the whole table again: {:#?}", log.borrow()));
        let cached: Vec<Record> =
            serde_json::from_str(&cached).expect("the cached table is a list of rows");
        let by_id = |rows: &[Record]| {
            let mut rows: Vec<(String, Vec<(String, serde_json::Value)>)> = rows
                .iter()
                .map(|row| {
                    let mut data: Vec<(String, serde_json::Value)> =
                        row.data.clone().into_iter().collect();
                    data.sort_by(|a, b| a.0.cmp(&b.0));
                    (row.id.clone(), data)
                })
                .collect();
            rows.sort_by(|a, b| a.0.cmp(&b.0));
            rows
        };
        assert_eq!(
            by_id(&cached),
            by_id(&d1.rows.borrow()),
            "the cached table is D1's: {:#?}",
            log.borrow()
        );
        assert!(
            cached
                .iter()
                .all(|row| !row.data.contains_key("blessed_hash")),
            "no cached row keeps blessed_hash: {cached:?}"
        );
    }
}
