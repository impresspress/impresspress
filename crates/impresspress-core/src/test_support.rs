//! Test infrastructure for impresspress-core integration tests.
//!
//! [`TestContext`] wires a real SQLite database (via the production
//! `DatabaseBlock` + `SQLiteDatabaseService`) into a minimal [`Context`]
//! implementation so unit and integration tests can exercise the full block
//! client stack without running a server process. In-memory by default
//! ([`TestContext::new`]); [`TestContext::new_on_disk`] is the same thing
//! over a file-backed service, for the tests whose subject is the read/write
//! connection split only that topology has.
//!
//! Every `call_block` goes through the runtime's own admission
//! (`wafer_run::runtime::call_gates::admit_call`: call depth, cancellation,
//! aliases, the caller's `requires` and capabilities, registration, the
//! target's interface), and every service op is WRAP-checked against the
//! caller the runtime would attribute it to and the grants the deployment
//! carries — a registration whose grants boot would reject is refused. A
//! constructor hands back an unframed context, which the runtime would treat
//! as an unattributed caller and refuse; a test runs code as a block with
//! [`TestContext::running_as`] and stages rows with [`TestContext::fixture`].
//! `parity_tests` holds the frame's answers to a sealed `Wafer`'s.
//!
//! [`source_scan`] is the other half: the shared walk and comment strippers
//! the crate's source gates are built on, so a gate states its root and its
//! exemptions instead of hand-rolling a fourth `read_dir` recursion.

mod dispatch;
#[cfg(test)]
pub mod htmx;
#[cfg(test)]
mod parity_tests;
pub mod source_scan;

use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Mutex},
};

use wafer_core::interfaces::crypto::service::{CryptoError, CryptoService};
use wafer_run::{
    context::Context,
    streams::output::{BufferedResponse, TerminalNotResponse},
    Block, BlockInfo, ErrorCode, InputStream, Message, OutputStream, ResourceGrant, WaferError,
};

use crate::routing::ExtraRoute;

/// Minimal test context backed by a real in-memory SQLite database.
///
/// Routes `"wafer-run/database"` calls to the production `DatabaseBlock`,
/// and every other name to the block [`Self::register_block`] put under it.
///
/// `Clone` is shallow — every interior field is already `Arc`/`Mutex`-shared
/// (or trivially copyable), so a clone produces another handle pointing at
/// the same database, blocks map, and config. Used by [`Context::clone_arc`]
/// so service objects (e.g. `AuthServiceImpl`) can stash an owning context
/// handle in a `OnceLock` past the lifetime of a `&TestContext` borrow.
#[derive(Clone)]
pub struct TestContext {
    /// The raw `DatabaseService` behind `database_block`, kept around so
    /// [`Self::break_writes`] can rewrap it in a fault-injecting decorator
    /// without losing the underlying in-memory SQLite data.
    db_service: Arc<dyn wafer_core::interfaces::database::service::DatabaseService>,
    database_block: Arc<dyn Block>,
    /// Config snapshot used by `config_get`. Immutable after construction so
    /// `config_get` can return `Option<&str>` without holding a lock.
    /// Populated via [`set_config`].
    config: Arc<HashMap<String, String>>,
    /// Whether `wafer-run/config` is the production
    /// [`crate::blocks::config::VariablesConfigBlock`] over this fixture's
    /// database — true once the `variables` table exists (admin migrations)
    /// or a test booted the config service. See [`Self::install_config_block`].
    config_store: bool,
    /// The blocks [`Self::register_block`] registered, by name.
    pub blocks: Arc<Mutex<HashMap<String, Arc<dyn Block>>>>,
    /// `BlockInfo` for every block registered via [`Self::register_block`],
    /// keyed by (and kept in sync with) `blocks`. Backs
    /// `Context::registered_blocks()` so handler code that gates behavior on
    /// "is block X registered?" (e.g. the vector block's backend-availability
    /// check, `blocks::vector::service::vector_backend_available`) sees the
    /// same signal in tests that a real `RuntimeContext` — whose
    /// `registered_blocks()` reflects its sealed startup snapshot — would
    /// produce in production. A plain `Vec` (not wrapped in the `blocks`
    /// mutex) is enough: `register_block` already takes `&mut self`, so no
    /// interior mutability is needed to update it, and `registered_blocks()`
    /// can return a borrowed slice directly instead of cloning through a
    /// lock guard.
    block_infos: Vec<wafer_run::BlockInfo>,
    /// Whose code runs on this context — what a block reached through
    /// [`Context::call_block`] sees as its caller. See [`dispatch::Frame`]:
    /// a constructor hands back an unframed context, [`Self::running_as`]
    /// and a callee's context run as a block, and [`Self::fixture`] is test
    /// setup.
    frame: dispatch::Frame,
    /// Who called into this frame: what [`Context::caller_id`] answers, and
    /// the identity a service's WRAP check authorizes — re-pointed on every
    /// hop, as `RuntimeContext::dispatch_call` re-points `caller_id`.
    called_by: dispatch::Caller,
    /// This frame's own `requires` allowlist — the gate production applies
    /// to a `call_block` itself, before the callee's handler makes any grant
    /// check.
    ///
    /// `Wafer::make_block_context` installs a block's declared `requires` on
    /// every context that block's code runs in, and
    /// `RuntimeContext::dispatch_call` refuses any target not named in it
    /// before it looks at capabilities or grants. `None` = undeclared =
    /// unrestricted, which is what `Wafer::resolve_block_requires_uncached`
    /// yields for a block with an empty or absent list, and what an unframed
    /// context and [`Self::fixture`] carry.
    caller_requires: Option<Vec<String>>,
    /// This frame's nesting depth: `0` for a top-level frame, one more for
    /// each `call_block` hop. A per-frame value, as in `RuntimeContext`, so
    /// sibling calls never accumulate.
    call_depth: u32,
    /// The cancellation flag every frame of one fixture shares, as every
    /// sub-context of a runtime dispatch shares its caller's. Set by
    /// [`Self::cancel`].
    cancelled: Arc<std::sync::atomic::AtomicBool>,
    /// Grants the deployment installs beside the ones blocks declare —
    /// `ImpresspressBuilder::wrap_grants`, and the operator grants boot loads
    /// from the database. Added by [`Self::add_deployment_grants`].
    deployment_grants: Vec<ResourceGrant>,
    /// Every grant a WRAP check sees, collected by a real `Wafer` on first
    /// use (see [`dispatch::collect_wrap_grants`]) and reset whenever a
    /// registration or [`Self::deployment_grants`] changes.
    wrap_grants: Arc<std::sync::OnceLock<Arc<Vec<ResourceGrant>>>>,
    /// Routes a fixture registered the way a downstream project would, via
    /// `ImpresspressBuilder::add_route`. Fed to [`Self::dispatch_resolved`]
    /// and [`Self::request`] so a test exercises the same router path —
    /// including its access gate — that the consumer's registration
    /// produces, instead of calling a block's `handle()` past the gate.
    extra_routes: Vec<ExtraRoute>,
    /// The object store behind the registered `wafer-run/storage` block, when
    /// a constructor installed one. Kept so [`Self::storage_get`] and
    /// [`Self::storage_ops`] can read the store *underneath* the namespacing
    /// wrapper — the only way a test can assert what another block's
    /// namespace actually holds.
    storage: Option<Arc<InMemoryStorageService>>,
    /// The shared state the registered `impresspress/dev` block was built
    /// over, when [`Self::with_dev`] installed one. Handed back by
    /// [`Self::dev_shared`] so a test can drive the activation queue directly
    /// rather than only through HTTP.
    #[cfg(feature = "block-dev")]
    dev_shared: Option<Arc<crate::blocks::dev::DevShared>>,
    /// Drop guard only — never read. Keeps the on-disk database file alive
    /// for exactly as long as some handle to it exists and deletes it
    /// afterwards; `None` for the in-memory constructors, which have no file.
    /// See [`TempDbFile`].
    ///
    /// Declared LAST on purpose: struct fields drop in declaration order, so
    /// `db_service` (and with it the write/reader connections and their
    /// worker threads) is already closed by the time the file is unlinked,
    /// which is what lets SQLite clean up its own `-wal`/`-shm` sidecars.
    _db_file: Option<Arc<TempDbFile>>,
}

/// A temporary on-disk SQLite database that deletes itself — and the WAL /
/// shared-memory sidecars `PRAGMA journal_mode=WAL` creates next to it — when
/// the last [`TestContext`] clone holding it is dropped.
///
/// Owned through an `Arc` inside `TestContext` rather than returned to the
/// caller as a guard, because `TestContext` is `Clone` (shallowly, onto the
/// same database) and `Context::clone_arc` hands clones to service objects
/// that outlive the `&TestContext` a test holds. A caller-held guard would
/// have to outlive all of those by hand; refcounting the file alongside the
/// service it backs makes that automatic.
struct TempDbFile(std::path::PathBuf);

impl Drop for TempDbFile {
    fn drop(&mut self) {
        let base = self.0.as_os_str().to_owned();
        for suffix in ["", "-wal", "-shm"] {
            let mut p = base.clone();
            p.push(suffix);
            let _ = std::fs::remove_file(std::path::PathBuf::from(p));
        }
    }
}

impl TestContext {
    /// Construct a `TestContext` with a fresh in-memory SQLite database.
    pub async fn new() -> Self {
        Self::over(
            Arc::new(
                wafer_block_sqlite::service::SQLiteDatabaseService::open_in_memory()
                    .expect("open in-memory sqlite"),
            ),
            None,
        )
    }

    /// Construct a `TestContext` over a fresh **file-backed** SQLite database.
    ///
    /// The difference from [`Self::new`] is not durability, it is topology:
    /// `SQLiteDatabaseService::open` opens dedicated `SQLITE_OPEN_READ_ONLY`
    /// reader connections alongside the single writable one and serves the
    /// `DbExec` read primitives from them, whereas `open_in_memory` opens zero
    /// readers (separate connections cannot see a private in-memory database)
    /// and therefore serves every operation, read and write alike, from the
    /// one write connection. That is the configuration every native
    /// deployment runs and the only one in which a write dispatched down the
    /// read path can be observed failing — under `open_in_memory` such a
    /// write simply succeeds on the write connection, which is how
    /// `DbExec::take_where` shipped dispatching its `DELETE … RETURNING`
    /// through `run_fetch` with a fully green test suite.
    ///
    /// Slower than [`Self::new`] (real file I/O, three connections, three
    /// worker threads), so it is for tests that specifically need the
    /// read/write split, not a default.
    ///
    /// The reader count is asserted here rather than in each test: `open`
    /// treats a reader connection that fails to open as non-fatal and
    /// degrades to serving reads from the write worker, which is precisely
    /// the in-memory topology — a fixture that degraded that way would hand
    /// every test built on it a green result for the wrong reason.
    pub async fn new_on_disk() -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let path = std::env::temp_dir().join(format!(
            "impresspress-test-{}-{nonce}.db",
            std::process::id()
        ));
        let svc = wafer_block_sqlite::service::SQLiteDatabaseService::open(
            path.to_str().expect("temp dir path is UTF-8"),
        )
        .expect("open file-backed sqlite");
        assert!(
            svc.reader_count() > 0,
            "the file-backed fixture must open read-only reader connections, \
             or it exercises the same single-connection topology as \
             TestContext::new"
        );
        Self::over(Arc::new(svc), Some(Arc::new(TempDbFile(path))))
    }

    /// The common wiring both constructors above share: put `svc` behind the
    /// production `DatabaseBlock` and hand back a context routing
    /// `wafer-run/database` at it.
    fn over(
        svc: Arc<dyn wafer_core::interfaces::database::service::DatabaseService>,
        db_file: Option<Arc<TempDbFile>>,
    ) -> Self {
        let database_block: Arc<dyn Block> = Arc::new(
            wafer_core::service_blocks::database::DatabaseBlock::new(svc.clone()),
        );

        let mut ctx = Self {
            db_service: svc,
            database_block,
            config: Arc::new(HashMap::new()),
            config_store: false,
            blocks: Arc::new(Mutex::new(HashMap::new())),
            block_infos: Vec::new(),
            frame: dispatch::Frame::Unframed,
            called_by: dispatch::Caller::Nobody,
            caller_requires: None,
            call_depth: 0,
            cancelled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            deployment_grants: Vec::new(),
            wrap_grants: Arc::default(),
            extra_routes: Vec::new(),
            storage: None,
            #[cfg(feature = "block-dev")]
            dev_shared: None,
            _db_file: db_file,
        };
        // Every target's builder registers a config block, so an unset key
        // answers the service's `NotFound` and a client read falls back to
        // its default. A fixture without one would answer "no such block",
        // which the config client returns as an error.
        ctx.install_env_config_block();
        ctx
    }

    /// Insert a single config entry into the snapshot.
    ///
    /// Makes a fresh `Arc<HashMap>` clone on each call — fine for tests,
    /// where the number of entries is small and mutations happen at setup time.
    ///
    /// Also (re)registers the `wafer-run/config` service block over the
    /// updated map, so block code that reads config through the typed client
    /// (`wafer_core::clients::config::get`/`get_default`) sees the same values
    /// as `config_get`.
    ///
    /// On a fixture whose `wafer-run/config` is the production block (see
    /// [`Self::install_config_block`]) the value joins that block's boot map
    /// instead, as an environment value would: a non-empty `variables` row
    /// for the same key still wins.
    pub fn set_config(&mut self, key: &str, value: &str) {
        let mut map = (*self.config).clone();
        map.insert(key.to_string(), value.to_string());
        self.config = Arc::new(map);

        if self.config_store {
            self.install_config_block();
            return;
        }
        self.install_env_config_block();
    }

    /// Serve `wafer-run/config` from the framework `ConfigBlock` over this
    /// fixture's config map.
    fn install_env_config_block(&mut self) {
        let svc = wafer_core::service_blocks::config::EnvConfigService::new();
        for (k, v) in self.config.iter() {
            wafer_core::interfaces::config::service::ConfigService::set(&svc, k, v);
        }
        let block: Arc<dyn Block> = Arc::new(wafer_core::service_blocks::config::ConfigBlock::new(
            Arc::new(svc),
        ));
        self.register_block("wafer-run/config", block);
    }

    /// This context runs `block`'s code, in the frame `Wafer::run_block`
    /// gives the block a listener or flow step dispatches to: no caller, and
    /// `block`'s own declared `requires` allowlist installed — `requires` plus `optional_requires`,
    /// read off the block this fixture registered under that name, else off
    /// the built-in block's declaration, else unrestricted for a name no
    /// block carries (a synthetic caller such as `test/ungranted`).
    ///
    /// A request the router admits reaches its block from
    /// `impresspress/router`, not from nowhere; a test about that path uses
    /// [`Self::dispatch_resolved`] or [`Self::request`], which route through
    /// the router's frame.
    ///
    /// Every block it reaches through `call_block` is called by `block`, and
    /// every service op those calls make is authorized as `block` against
    /// the deployment's grants — the same two gates production applies, with
    /// nothing a test can hand itself: grants are what the blocks declare
    /// and [`Self::add_deployment_grants`] installs.
    pub fn running_as(mut self, block: &str) -> Self {
        self.caller_requires = match self.registered(block) {
            Some(registered) => registered.info().call_allowlist(),
            None => dispatch::built_in_call_allowlist(block),
        };
        self.frame = dispatch::Frame::Block(block.to_string());
        self.called_by = dispatch::Caller::Nobody;
        self.call_depth = 0;
        self
    }

    /// The fixture's own frame over the same database, blocks and config —
    /// what a test stages and asserts through. Authorized as the admin block
    /// (see [`dispatch::Caller::Fixture`]), so it is reached only by asking
    /// for it: a constructor hands back an unframed context, which is
    /// refused every guarded resource, so no code under test runs with the
    /// fixture's authority unless a test says so here.
    ///
    /// A test whose subject runs as a block still has to seed rows and read
    /// back what landed, and those reads are not the block's: the block may
    /// hold no grant on the table the test inspects.
    pub fn fixture(&self) -> Self {
        let mut ctx = self.clone();
        ctx.frame = dispatch::Frame::Fixture;
        ctx.called_by = dispatch::Caller::Fixture;
        ctx.caller_requires = None;
        ctx.call_depth = 0;
        ctx
    }

    /// Install grants the deployment adds beside the ones its blocks declare
    /// — what `ImpresspressBuilder::wrap_grants` hands the runtime, or the
    /// operator grants boot loads from the database. Validated by
    /// `Wafer::add_wrap_grants`, which refuses a malformed one.
    pub fn add_deployment_grants(&mut self, grants: Vec<ResourceGrant>) {
        self.deployment_grants.extend(grants);
        self.wrap_grants = Arc::default();
    }

    /// Cancel every frame of this fixture, as the runtime cancels a dispatch
    /// that timed out or was aborted: [`Context::is_cancelled`] answers
    /// `true` and every later `call_block` is refused with `Cancelled`.
    pub fn cancel(&self) {
        self.cancelled
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// The block registered under `name`, the database block included.
    fn registered(&self, name: &str) -> Option<Arc<dyn Block>> {
        if name == "wafer-run/database" {
            return Some(self.database_block.clone());
        }
        self.blocks
            .lock()
            .expect("blocks mutex poisoned")
            .get(name)
            .cloned()
    }

    /// The grant set a WRAP check reads — see [`Self::wrap_grants`].
    fn deployment_wrap_grants(&self) -> Arc<Vec<ResourceGrant>> {
        self.wrap_grants
            .get_or_init(|| {
                let blocks: Vec<(String, Arc<dyn Block>)> = {
                    let guard = self.blocks.lock().expect("blocks mutex poisoned");
                    guard
                        .iter()
                        .map(|(name, block)| (name.clone(), block.clone()))
                        .chain([(
                            "wafer-run/database".to_string(),
                            self.database_block.clone(),
                        )])
                        .collect()
                };
                Arc::new(dispatch::collect_wrap_grants(
                    &blocks,
                    &self.deployment_grants,
                ))
            })
            .clone()
    }

    /// The sub-context `RuntimeContext::dispatch_call` hands the block
    /// registered under `name`: it runs as `name`, is called by this frame,
    /// carries the callee's own declared allowlist `requires` — what
    /// [`admit_call`](wafer_run::runtime::call_gates::admit_call) read off
    /// the callee — and sits one level deeper.
    fn for_callee(&self, name: &str, requires: Option<Arc<Vec<String>>>) -> Self {
        let mut ctx = self.clone();
        ctx.called_by = self.frame.as_caller();
        ctx.frame = dispatch::Frame::Block(name.to_string());
        ctx.caller_requires = requires.map(Arc::unwrap_or_clone);
        ctx.call_depth = self.call_depth + 1;
        ctx
    }

    /// The frame the *router* runs in: `impresspress/router`'s top-level
    /// context.
    ///
    /// `routing::route_to_block` is the impresspress router block's code,
    /// and that block declares no `requires`, so production reaches every
    /// routed target from an unrestricted frame — and the routed block sees
    /// the router as its caller.
    fn as_router(&self) -> Self {
        self.clone()
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID)
    }

    /// Apply one block's migrations into this fixture through the same gated
    /// path the runtime uses ([`crate::migration_helper::apply_migrations`]),
    /// sourcing the SQL from the block's single-source `SQLITE_MIGRATIONS` /
    /// `POSTGRES_MIGRATIONS` consts.
    ///
    /// Replaces the per-block `migrations::apply()` wrappers (deleted when the
    /// `impresspress_feature_block!` macro folded each block's `lifecycle(Init)`
    /// into `migration_helper::lifecycle_init`). Test-fixture setup is an
    /// explicit exception to the no-raw-migration-runner rule; it mirrors the
    /// production gate exactly so fixtures exercise the real schema.
    async fn apply_block_migrations(
        &self,
        block_name: &str,
        sqlite: &[(&str, &str)],
        postgres: &[&str],
    ) {
        let sqlite_sql: Vec<&str> = sqlite.iter().map(|(_, sql)| *sql).collect();
        // In the block's own frame, as its `lifecycle(Init)` runs them.
        crate::migration_helper::apply_migrations(
            &self.fixture().running_as(block_name),
            block_name,
            &sqlite_sql,
            postgres,
        )
        .await
        .unwrap_or_else(|e| panic!("apply {block_name} migrations in test fixture: {e}"));
    }

    /// Build a `TestContext` with admin + auth block migrations applied.
    ///
    /// Convenience constructor for tests that need the
    /// `wafer_run__auth__{users,orgs,sessions,provider_links,...}` schema
    /// in place — most repo and handler tests do.
    ///
    /// Admin migrations run first so that the
    /// `impresspress__admin__block_settings` tracking table exists before
    /// auth's `apply_if_blessed` upserts its `current_hash` row. In
    /// production that ordering comes from `builder::boot`, which calls
    /// `init_block` on admin explicitly before iterating the rest; here we
    /// enforce it explicitly.
    ///
    /// NOT from registration order, which never drove it: `block_names()` is
    /// sorted, and since `blocks::register_admin` took over from the
    /// zero-arg manifest, admin in fact registers *after* every manifest
    /// block.
    pub async fn with_auth() -> Self {
        Self::with_admin().await.with_auth_added().await
    }

    /// [`Self::with_auth`]'s schema over a **file-backed** database — the
    /// read/write-split topology described on [`Self::new_on_disk`], which
    /// `with_auth` (in-memory, zero readers) cannot produce.
    ///
    /// For auth tests whose subject is a statement that both reads and
    /// writes: `oauth_pkce::take` and `bootstrap_tokens::take_valid_by_hash`
    /// both consume their row with a single `DELETE … RETURNING`, and whether
    /// that statement runs on the write connection or a read-only one is
    /// invisible under `with_auth` and decisive here.
    pub async fn with_auth_on_disk() -> Self {
        Self::new_on_disk()
            .await
            .with_admin_added()
            .await
            .with_auth_added()
            .await
    }

    /// Add auth's migrations to an EXISTING fixture — the same
    /// `wafer_run__auth__*` schema [`Self::with_auth`] applies to a fresh
    /// context, layered instead on top of whatever `self` already carries.
    ///
    /// Mirrors [`Self::with_dev_added`]'s shape for the same reason: a caller
    /// that needs auth's tables *alongside* another block's own fixture —
    /// e.g. `TestContext::with_products().await.with_auth_added().await`, for
    /// a scenario that seeds a real owner account next to a product — has no
    /// way to reach `with_auth`'s migrations without also re-running
    /// `with_products`'s from scratch. Admin's migrations are idempotent
    /// (`CREATE TABLE IF NOT EXISTS`) but re-registering a block is not
    /// something either constructor needs to redo here.
    pub async fn with_auth_added(self) -> Self {
        self.apply_block_migrations(
            "wafer-run/auth",
            crate::blocks::auth::migrations::SQLITE_MIGRATIONS,
            crate::blocks::auth::migrations::POSTGRES_MIGRATIONS,
        )
        .await;
        self
    }

    /// Seed one `wafer_run__auth__users` row under a caller-chosen id.
    ///
    /// The ONE raw-SQL users fixture in this crate (CLAUDE.md's test-fixture
    /// exception). Ten test modules hand-wrote this same `INSERT` because
    /// `repo::users::insert` mints a UUID, while each of them needs the id
    /// its own authenticated `Message` — or a foreign key on another auth
    /// table — already names. Consolidating them keeps the users table's
    /// wire name and NOT NULL column set spelled in one place, which is what
    /// `tests/repo_door.rs` checks.
    ///
    /// `email` is `{user_id}@example.com` and `display_name` is `user_id`;
    /// the role is `"user"`, so a test that needs an admin sets it through
    /// the repo afterwards.
    pub async fn seed_auth_user(&self, user_id: &str) {
        self.seed_auth_user_verified(user_id, false).await;
    }

    /// [`Self::seed_auth_user`] with an explicit `email_verified` flag,
    /// written as the `0`/`1` INTEGER the migration declares.
    pub async fn seed_auth_user_verified(&self, user_id: &str, email_verified: bool) {
        wafer_core::clients::database::exec_raw(
            &self.fixture(),
            "INSERT INTO wafer_run__auth__users \
             (id, email, display_name, role, email_verified, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
            &[
                serde_json::json!(user_id),
                serde_json::json!(format!("{user_id}@example.com")),
                serde_json::json!(user_id),
                serde_json::json!("user"),
                serde_json::json!(i64::from(email_verified)),
                serde_json::json!("2026-01-01T00:00:00Z"),
                serde_json::json!("2026-01-01T00:00:00Z"),
            ],
        )
        .await
        .expect("seed auth user fixture");
    }

    /// Build a `TestContext` with admin migrations applied (only).
    ///
    /// Use this for tests that exercise a block's own `init()` / migration
    /// application directly — the prerequisite is that
    /// `impresspress__admin__block_settings` exists so `apply_if_blessed` can
    /// upsert its tracking row.
    pub async fn with_admin() -> Self {
        Self::new().await.with_admin_added().await
    }

    /// Add admin's migrations to an EXISTING fixture — [`Self::with_admin`]'s
    /// schema layered on whatever `self` already carries, the counterpart of
    /// [`Self::with_auth_added`]. Lets a constructor choose its database
    /// topology (in-memory or [`Self::new_on_disk`]) without restating the
    /// migration chain built on top of it.
    ///
    /// Also installs the production config block over the new `variables`
    /// table ([`Self::install_config_block`]), as every target's builder does.
    pub async fn with_admin_added(mut self) -> Self {
        self.apply_block_migrations(
            "impresspress/admin",
            crate::blocks::admin::migrations::SQLITE_MIGRATIONS,
            crate::blocks::admin::migrations::POSTGRES_MIGRATIONS,
        )
        .await;
        self.install_config_block();
        self
    }

    /// Build a `TestContext` with admin + auth + files migrations applied,
    /// **running as `impresspress/files`** — in the frame production gives
    /// that block (see [`Self::running_as`]).
    ///
    /// The frame is not decoration. A frame other than the files block's
    /// carries a different allowlist, or none, so it certifies calls the
    /// runtime refuses the files block. That is exactly how the share path
    /// shipped calling `wafer-run/crypto` without declaring it: every test that created a
    /// share passed, and the live server answered `PermissionDenied: block
    /// 'wafer-run/crypto' not in requires list`. The frame goes on the
    /// constructor every files-block test shares, so none of them can miss
    /// it.
    #[cfg(feature = "block-files")]
    pub async fn with_files() -> Self {
        let ctx = Self::with_auth().await;
        ctx.apply_block_migrations(
            "impresspress/files",
            crate::blocks::files::migrations::SQLITE_MIGRATIONS,
            crate::blocks::files::migrations::POSTGRES_MIGRATIONS,
        )
        .await;
        ctx.running_as(crate::blocks::files::FilesBlock::BLOCK_NAME)
    }

    /// Build a `TestContext` with admin + auth + userportal migrations
    /// applied, running as `impresspress/userportal` (see
    /// [`Self::running_as`]); stage and assert through [`Self::fixture`].
    #[cfg(feature = "block-userportal")]
    pub async fn with_userportal() -> Self {
        let ctx = Self::with_auth().await;
        ctx.apply_block_migrations(
            "impresspress/userportal",
            crate::blocks::userportal::migrations::SQLITE_MIGRATIONS,
            crate::blocks::userportal::migrations::POSTGRES_MIGRATIONS,
        )
        .await;
        ctx.running_as("impresspress/userportal")
    }

    /// Build a TestContext with admin, auth, and tickets migrations applied,
    /// running as `impresspress/tickets` (see [`Self::running_as`]); stage
    /// and assert through [`Self::fixture`].
    #[cfg(feature = "block-tickets")]
    pub async fn with_tickets() -> Self {
        let mut ctx = Self::with_auth().await;
        ctx.apply_block_migrations(
            "impresspress/tickets",
            crate::blocks::tickets::migrations::SQLITE_MIGRATIONS,
            crate::blocks::tickets::migrations::POSTGRES_MIGRATIONS,
        )
        .await;
        ctx.register_block(
            "impresspress/tickets",
            Arc::new(crate::blocks::tickets::TicketsBlock::new()),
        );
        ctx.running_as("impresspress/tickets")
    }

    /// Build a `TestContext` with admin + auth + vector migrations applied,
    /// running as `impresspress/vector` (see [`Self::running_as`]); stage and
    /// assert through [`Self::fixture`].
    #[cfg(feature = "block-vector")]
    pub async fn with_vector() -> Self {
        let ctx = Self::with_auth().await;
        ctx.apply_block_migrations(
            "impresspress/vector",
            crate::blocks::vector::migrations::SQLITE_MIGRATIONS,
            crate::blocks::vector::migrations::POSTGRES_MIGRATIONS,
        )
        .await;
        ctx.running_as("impresspress/vector")
    }

    /// Build a `TestContext` with admin + llm migrations applied, running as
    /// `impresspress/llm` (see [`Self::running_as`]); stage and assert
    /// through [`Self::fixture`].
    ///
    /// Admin first so the `impresspress__admin__block_settings` tracking
    /// table exists before llm's `apply_if_blessed` upserts its row (the
    /// production ordering). llm's schema does not depend on auth, so auth
    /// migrations are skipped.
    #[cfg(feature = "block-llm")]
    pub async fn with_llm() -> Self {
        let ctx = Self::with_admin().await;
        ctx.apply_block_migrations(
            "impresspress/llm",
            crate::blocks::llm::migrations::SQLITE_MIGRATIONS,
            crate::blocks::llm::migrations::POSTGRES_MIGRATIONS,
        )
        .await;
        ctx.running_as("impresspress/llm")
    }

    /// Build a `TestContext` with admin + products migrations applied,
    /// running as `impresspress/products` (see [`Self::running_as`]); stage
    /// and assert through [`Self::fixture`].
    ///
    /// Admin migrations run first so the `impresspress__admin__block_settings`
    /// tracking table exists before products' `apply_if_blessed` upserts its
    /// `current_hash` row (the production ordering, enforced explicitly here).
    /// Products' schema does not depend on auth, so auth migrations are
    /// skipped.
    #[cfg(feature = "block-products")]
    pub async fn with_products() -> Self {
        let mut ctx = Self::with_admin().await;
        ctx.apply_block_migrations(
            "impresspress/products",
            crate::blocks::products::migrations::SQLITE_MIGRATIONS,
            crate::blocks::products::migrations::POSTGRES_MIGRATIONS,
        )
        .await;
        // Register the real block so `registered_blocks()` reports it — the
        // context IS a products deployment (its migrations just ran). Nav
        // rendering gates the Products sidebar item on this signal.
        ctx.register_block(
            "impresspress/products",
            Arc::new(crate::blocks::products::ProductsBlock::new()),
        );
        ctx.running_as("impresspress/products")
    }

    /// Build a `TestContext` with admin + signal migrations applied, running
    /// as `impresspress/signal` (see [`Self::running_as`]).
    ///
    /// No auth: the signal block has no user and never reads one — every
    /// endpoint is public, so admin-only is enough to let its own
    /// `lifecycle(Init)`-equivalent (`apply_block_migrations`) upsert its
    /// `impresspress__admin__block_settings` tracking row, the same
    /// prerequisite `with_llm` and `with_products` rely on.
    #[cfg(feature = "block-signal")]
    pub async fn with_signal() -> Self {
        let ctx = Self::with_admin().await;
        ctx.apply_block_migrations(
            "impresspress/signal",
            crate::blocks::signal::migrations::SQLITE_MIGRATIONS,
            crate::blocks::signal::migrations::POSTGRES_MIGRATIONS,
        )
        .await;
        ctx.running_as("impresspress/signal")
    }

    /// Build a `TestContext` with admin + dev-sandbox migrations applied, the
    /// `impresspress/dev` block registered over `control`, and the `/b/dev`
    /// `Admin` extra route added the way `ImpresspressBuilder::add_route`
    /// would.
    ///
    /// WRAP enforcement is switched on with
    /// [`crate::blocks::dev::wrap_grants`], so a test exercises the real
    /// grant set: the block's own
    /// `impresspress__dev__*` tables self-admit under the own-namespace rule,
    /// and the published site under `wafer-run/web/site/*` is reachable only
    /// because that grant is present. Migrations run before enforcement
    /// starts, exactly as they do at boot.
    #[cfg(feature = "block-dev")]
    pub async fn with_dev(control: Arc<dyn crate::blocks::dev::RuntimeControl>) -> Self {
        Self::with_admin().await.with_dev_added(control).await
    }

    /// Add the `impresspress/dev` block to an EXISTING fixture — the dev
    /// migrations, block registration, storage wiring, `/b/dev` extra route
    /// and WRAP enforcement [`Self::with_dev`] applies to a fresh context,
    /// applied instead on top of whatever `self` already carries.
    ///
    /// For a test that needs `impresspress/dev` alongside another block's
    /// own fixture — e.g. `TestContext::with_products().await.with_dev_added(..)`
    /// for `/b/dev/api/tools.json`, which projects both blocks' endpoints
    /// into one manifest. [`Self::with_dev`] cannot do this: it always starts
    /// from a bare `with_admin()` context, so a caller that needs a second
    /// block already registered would have no way to add it before the dev
    /// block's WRAP enforcement switches on.
    ///
    /// Migrations, not registration order, are what production guarantees
    /// (admin's `block_settings` table before any block's `apply_if_blessed`
    /// upsert) — adding dev's migrations after another block's have already
    /// run is exactly what a deployment that enables the sandbox alongside an
    /// existing block set does, so this is not a fixture-only shortcut.
    #[cfg(feature = "block-dev")]
    pub async fn with_dev_added(
        self,
        control: Arc<dyn crate::blocks::dev::RuntimeControl>,
    ) -> Self {
        // The default shell is a plausible one rather than an empty one: a
        // fixture that had no shell at all could not tell "this test does not
        // exercise the export" apart from "the export produced a folder with
        // no runtime in it", and the second is precisely what
        // `blocks::dev::export` refuses.
        self.with_dev_added_and_shell(
            control,
            Arc::new(crate::blocks::dev::test_support::FakeShell::new()),
        )
        .await
    }

    /// [`Self::with_dev_added`] with an explicit [`crate::blocks::dev::ShellSource`].
    ///
    /// For the export tests, whose subject IS the shell: which files it
    /// carries, what its `sw.js` says, and what happens when it cannot be
    /// listed. Everything else about the fixture is identical — this is the
    /// one function that wires the dev block, and `with_dev_added` is a call
    /// to it with the default shell.
    #[cfg(feature = "block-dev")]
    pub async fn with_dev_added_and_shell(
        mut self,
        control: Arc<dyn crate::blocks::dev::RuntimeControl>,
        shell: Arc<dyn crate::blocks::dev::ShellSource>,
    ) -> Self {
        use crate::blocks::dev;

        self.apply_block_migrations(
            dev::BLOCK_NAME,
            dev::migrations::SQLITE_MIGRATIONS,
            dev::migrations::POSTGRES_MIGRATIONS,
        )
        .await;
        let shared = dev::DevShared::new(control, shell);
        self.dev_shared = Some(shared.clone());
        let block = Arc::new(dev::DevBlock::with_workspace(shared));
        self.register_block(dev::BLOCK_NAME, block);
        // The workspace store (blobs + `workspace.json`) lives in storage,
        // so the fixture needs a real object store behind the production
        // `wafer-run/storage` block — its handler is what turns the block's
        // own `blobs` / `""` folders into `impresspress/dev/…`, and what
        // refuses a cross-block reach the grants below do not cover.
        let store = Arc::new(InMemoryStorageService::new());
        self.storage = Some(store.clone());
        self.register_block("wafer-run/storage", crate::blocks::storage::create(store));
        self.add_extra_route(ExtraRoute::new(
            dev::ROUTE_PREFIX.to_string(),
            dev::BLOCK_NAME.to_string(),
            crate::routing::RouteAccess::Admin,
        ));
        // The grants the consumer hands `ImpresspressBuilder::wrap_grants`
        // alongside the block, and the block's own frame — its declared
        // `requires` included: `dev::export` reaches storage and the database
        // through `call_block`, and production refuses any target the block
        // did not declare before it looks at a single grant.
        self.add_deployment_grants(dev::wrap_grants());
        self.running_as(dev::BLOCK_NAME)
    }

    /// Register a route the way `ImpresspressBuilder::add_route` does, so
    /// [`Self::dispatch_resolved`] and [`Self::request`] route through it.
    pub fn add_extra_route(&mut self, route: ExtraRoute) {
        self.extra_routes.push(route);
    }

    /// The shared state the fixture's `impresspress/dev` block is built over.
    ///
    /// The same `Arc` the block holds — not a copy — so a test that drives
    /// `activation::request` through this handle contends for the very queue
    /// the HTTP handlers use.
    #[cfg(feature = "block-dev")]
    pub fn dev_shared(&self) -> Arc<crate::blocks::dev::DevShared> {
        self.dev_shared
            .clone()
            .expect("this fixture registered no dev block; use TestContext::with_dev")
    }

    /// Read an object out of the fixture's object store *underneath* the
    /// per-block namespacing wrapper.
    ///
    /// `block` is the namespace owner (`"wafer-run/web"`), `folder` its folder
    /// (`"site"`), `key` the object — i.e. exactly the three parts
    /// the storage handler resolves into `{block}/{folder}/{key}`.
    /// Going through the store directly is the point: a test asserting what
    /// the *published site* holds must not be able to satisfy itself by
    /// reading through the dev block's own grants.
    pub async fn storage_get(
        &self,
        block: &str,
        folder: &str,
        key: &str,
    ) -> Result<Vec<u8>, wafer_core::interfaces::storage::service::StorageError> {
        use wafer_core::interfaces::storage::service::StorageService as _;
        let (bytes, _info) = self
            .storage()
            .get(&store_folder(block, folder), key)
            .await?;
        Ok(bytes)
    }

    /// Park the fixture's object store on the next `get` of one object, and
    /// hand back the handle that releases it.
    ///
    /// `block`, `folder` and `key` are the same three parts
    /// [`Self::storage_get`] takes — the namespace owner, its folder and the
    /// object — because the hold is installed *underneath* the per-block
    /// namespacing wrapper. `store_folder` composes the first two the way the
    /// real path does, which matters for a block's namespace root: the dev
    /// block's `workspace.json` lives in folder `""`, and that resolves to
    /// `impresspress/dev` with no trailing separator.
    ///
    /// A hold whose key does not match parks nothing and fails silently, so
    /// assert [`HeldGet::was_reached`] in any test that installs one.
    ///
    /// This is the only way a fixture can put a suspension inside a handler:
    /// the store resolves everything on the first poll, so joined futures
    /// otherwise run to completion one at a time and no interleaving is
    /// observable at all.
    pub fn hold_next_storage_get(&self, block: &str, folder: &str, key: &str) -> Arc<HeldGet> {
        self.storage()
            .hold_next_get(&store_folder(block, folder), key)
    }

    /// Make the fixture's object store refuse the next `put`.
    ///
    /// The one failure a test cannot otherwise produce: a publish that fails
    /// after the runtime has already been swapped, which the activation queue
    /// has to unwind.
    pub fn fail_next_storage_put(&self, message: &str) {
        self.storage().fail_next_put(message);
    }

    /// Make the fixture's object store refuse the next `put` of one object,
    /// letting every other `put` through.
    ///
    /// `block`, `folder` and `key` are [`Self::storage_get`]'s three parts.
    /// For the failures that land after other writes on the same request —
    /// a file write whose blob stores and whose `workspace.json` save does
    /// not, or a rollback whose publish succeeds and whose workspace adoption
    /// does not — where [`Self::fail_next_storage_put`] would hit the first
    /// write instead.
    pub fn fail_next_storage_put_to(&self, block: &str, folder: &str, key: &str, message: &str) {
        self.storage()
            .fail_next_put_to(&store_folder(block, folder), key, message);
    }

    /// Make the fixture's object store refuse the next `delete` of one object,
    /// deleting nothing.
    pub fn fail_next_storage_delete_of(&self, block: &str, folder: &str, key: &str, message: &str) {
        self.storage()
            .fail_next_delete_of(&store_folder(block, folder), key, message);
    }

    /// Every read the fixture's store has served, oldest first, as
    /// `"get {folder}/{key}"` for a buffered read and
    /// `"get_streaming {folder}/{key}"` for a streaming one.
    ///
    /// The two are recorded apart because they cost apart on a real backend:
    /// a buffered `get` transfers the whole body before it answers, a
    /// streaming one answers with the metadata and transfers only what its
    /// consumer reads.
    pub fn storage_reads(&self) -> Vec<String> {
        self.storage().reads()
    }

    /// Every mutating storage operation the fixture's store has seen, oldest
    /// first, as `"{op} {folder}/{key}"`.
    ///
    /// Ordering is the assertion this exists for: the site publisher must
    /// write `index.html` after the assets it references, and only the order
    /// of the `put`s can show that.
    pub fn storage_ops(&self) -> Vec<String> {
        self.storage().ops()
    }

    /// The fixture's database as the platform service handle — the one the
    /// registered `wafer-run/database` block serves, decorators included.
    ///
    /// For a fake backend block that keeps its data in the same database, as
    /// a real backend reads its own store without going through the
    /// database block.
    pub fn database_service(
        &self,
    ) -> Arc<dyn wafer_core::interfaces::database::service::DatabaseService> {
        self.db_service.clone()
    }

    /// The fixture's object store as the platform service handle.
    ///
    /// For the code paths that hold a `StorageService` rather than a
    /// [`wafer_run::context::Context`] — the runtime rebuild reads guest
    /// artifacts before there is a runtime to route a `wafer-run/storage` call
    /// through. Deliberately the *same* store [`Self::storage_get`] reads, so
    /// a test can prove the two access paths address the same objects.
    pub fn storage_service(
        &self,
    ) -> Arc<dyn wafer_core::interfaces::storage::service::StorageService> {
        self.storage
            .clone()
            .expect("this fixture registered no storage service")
    }

    fn storage(&self) -> &InMemoryStorageService {
        self.storage
            .as_deref()
            .expect("this fixture registered no storage service")
    }

    /// Route `msg` through [`crate::routing::route_to_block`] using this
    /// context's registered `BlockInfo`s and extra routes, taking the
    /// identity already on `msg` as resolved.
    ///
    /// The layer BELOW the request preamble: nothing here reads a credential.
    /// The `auth.*` meta [`auth_msg`] / [`admin_msg`] inject is believed as
    /// it stands, the pipeline's JWT/API-key check never runs, and neither
    /// does the CSRF origin policy. What does run is the router's access
    /// gate, so a test of a block's behaviour behind the gate — or of the
    /// gate given a caller — belongs here. A test about who a request IS —
    /// a token, a cookie, an API key, a revocation, a cross-site form — uses
    /// [`Self::request`], which resolves the caller the way production does.
    pub async fn dispatch_resolved(&self, msg: Message) -> OutputStream {
        self.dispatch_resolved_with_input(msg, InputStream::empty())
            .await
    }

    /// [`Self::dispatch_resolved`] for a request that carries a body.
    ///
    /// A `POST`/`PATCH` handler reads its body off the `InputStream`, which
    /// `dispatch_resolved` hands it empty. Routing a write by calling the
    /// block's `handle()` directly would skip the router's access gate, so
    /// the body belongs on this path, not on a second one.
    pub async fn dispatch_resolved_with_input(
        &self,
        msg: Message,
        input: InputStream,
    ) -> OutputStream {
        // Routed from the router's frame, not the fixture's block identity —
        // see [`Self::as_router`].
        let router = self.as_router();
        crate::routing::route_to_block(
            &router,
            msg,
            input,
            &crate::features::AllEnabled,
            &self.block_infos,
            &self.extra_routes,
        )
        .await
    }

    /// [`Self::dispatch_resolved_with_input`] with `body` serialized as the
    /// JSON request body.
    pub async fn dispatch_resolved_json<T: serde::Serialize>(
        &self,
        msg: Message,
        body: &T,
    ) -> OutputStream {
        let bytes = serde_json::to_vec(body).expect("serialize test request body");
        self.dispatch_resolved_with_input(msg, InputStream::from_bytes(bytes))
            .await
    }

    /// Send `msg` through the production request path: the router block's
    /// own [`crate::blocks::router::ImpresspressRouterBlock`] `handle`, built
    /// over this fixture's `BlockInfo`s and extra routes the way
    /// `builder::registration` builds it.
    ///
    /// That is the request preamble up from the router: it picks the
    /// credential off the `Authorization` header or the `auth_token` cookie
    /// and records which, then
    /// [`crate::pipeline::handle_request`] verifies it — a JWT's signature,
    /// issuer, blocklist entry and `auth_version`; an API key's row, owner
    /// and roles — writes the `auth.*` meta from what verified, applies the
    /// CSRF origin policy to a cookie-authenticated write, routes, and logs.
    /// It is not the whole of what a deployment runs: in the `site-main` flow
    /// ([`crate::flows::site_main`]) the `security-headers`, `cors`,
    /// `readonly-guard` and `body-limit` steps run before the router and do
    /// not run here, so a header they add or a request they refuse is not
    /// something this path can show.
    ///
    /// `msg` carries what the wire carries — `req.*` and `http.header.*`
    /// meta, as [`anon_msg`] builds and [`Session::bearer`] /
    /// [`Session::cookie`] / [`api_key_header`] extend. A message that
    /// already carries `auth.*` meta is refused with a panic: the adapters
    /// never produce one, and a test that pre-resolved its caller is about
    /// the layer [`Self::dispatch_resolved`] drives.
    ///
    /// The router verifies against this fixture's
    /// `WAFER_RUN__AUTH__JWT_SECRET`, as the builder hands it the booted
    /// config's; [`Self::with_sign_in_added`] sets it to the secret the
    /// registered crypto block signs with.
    pub async fn request(&self, msg: Message) -> OutputStream {
        self.request_with_input(msg, InputStream::empty()).await
    }

    /// [`Self::request`] for a request that carries a body.
    pub async fn request_with_input(&self, msg: Message, input: InputStream) -> OutputStream {
        if let Some(injected) = msg.meta.iter().find(|m| m.key.starts_with("auth.")) {
            panic!(
                "TestContext::request sends what the wire carries, and {:?} is not \
                 something a client can send: the pipeline writes the auth.* meta. \
                 Authenticate with a Session or an API key, or use dispatch_resolved \
                 for a test about the layer below the preamble.",
                injected.key
            );
        }
        let jwt_secret = self
            .config
            .get(crate::blocks::auth::JWT_SECRET_KEY)
            .cloned()
            .unwrap_or_default();
        let router = crate::blocks::router::ImpresspressRouterBlock::with_extra_routes(
            Arc::new(std::sync::RwLock::new(jwt_secret)),
            Arc::new(crate::features::AllEnabled),
            self.block_infos.clone(),
            self.extra_routes.clone(),
        );
        router.handle(&self.as_router(), msg, input).await
    }

    /// [`Self::request_with_input`] with `body` serialized as the JSON
    /// request body.
    pub async fn request_json<T: serde::Serialize>(&self, msg: Message, body: &T) -> OutputStream {
        let bytes = serde_json::to_vec(body).expect("serialize test request body");
        self.request_with_input(msg, InputStream::from_bytes(bytes))
            .await
    }

    /// Make this fixture able to sign people in the way a deployment does:
    /// a real `wafer-run/crypto` block, `WAFER_RUN__AUTH__JWT_SECRET` set to
    /// the secret it signs with — one secret for signing, verifying and the
    /// CSRF form token, as `builder::registration` wires one — and the
    /// production `impresspress/auth-ui` block registered, so
    /// [`Self::sign_in`] and [`Session::create_api_key`] reach its real
    /// routes through [`Self::request`].
    ///
    /// PRECONDITION: auth's migrations have run ([`Self::with_auth`] or
    /// [`Self::with_auth_added`]).
    pub fn with_sign_in_added(mut self) -> Self {
        self.register_block(
            "wafer-run/crypto",
            Arc::new(wafer_core::service_blocks::crypto::CryptoBlock::new(
                Arc::new(real_crypto_service()),
            )),
        );
        self.set_config(crate::blocks::auth::JWT_SECRET_KEY, CRYPTO_BLOCK_JWT_SECRET);
        self.register_block(
            crate::blocks::auth_ui::AUTH_UI_BLOCK_ID,
            Arc::new(crate::blocks::auth_ui::AuthUiBlock::new()),
        );
        self
    }

    /// Seed an account that can sign in with `password`: a user row whose
    /// inline role is `role`, and a password credential hashed by the
    /// registered crypto block. Returns the user id.
    ///
    /// Staged as fixture data; the credential it yields is minted by
    /// [`Self::sign_in`], through the login route.
    pub async fn seed_account(&self, email: &str, password: &str, role: &str) -> String {
        let fixture = self.fixture();
        let user = seed_user(email).role(role).insert(&fixture).await;
        let hash = wafer_core::clients::crypto::hash(&fixture, password)
            .await
            .expect("hash the fixture password");
        crate::blocks::auth::repo::local_credentials::insert(&fixture, &user.id, &hash, false)
            .await
            .expect("seed the fixture password");
        user.id
    }

    /// Sign in through `POST /b/auth/api/login` over [`Self::request`] and
    /// keep what the response hands a client: the access token from the body
    /// and the `auth_token` cookie from its `Set-Cookie`.
    ///
    /// Needs [`Self::with_sign_in_added`] and an account from
    /// [`Self::seed_account`]; panics unless the login answers 200.
    pub async fn sign_in(&self, email: &str, password: &str) -> Session {
        let mut msg = anon_msg("create", "/b/auth/api/login");
        msg.set_meta(wafer_block::meta::META_REQ_CLIENT_IP, "203.0.113.50");
        let out = self
            .request_json(
                msg,
                &serde_json::json!({ "email": email, "password": password }),
            )
            .await;
        let parts = wafer_block::http_codec::collect_http_response(out).await;
        let body: serde_json::Value = serde_json::from_slice(&parts.body).unwrap_or_default();
        assert_eq!(parts.status, 200, "sign-in as {email} failed: {body}");
        let cookie = parts
            .headers
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case("set-cookie"))
            .find_map(|(_, value)| {
                value
                    .split(';')
                    .next()
                    .and_then(|pair| pair.trim().strip_prefix("auth_token="))
                    .map(str::to_string)
            })
            .expect("the login response sets the auth_token cookie");
        Session {
            user_id: body["user"]["id"]
                .as_str()
                .expect("the login response names the user")
                .to_string(),
            access_token: body["access_token"]
                .as_str()
                .expect("the login response carries an access token")
                .to_string(),
            cookie,
        }
    }

    /// Install a `wafer-run/config` service block seeded the way a real boot
    /// seeds one: run the production `seed_and_load` loader over the
    /// `variables` table, then fill an `EnvConfigService` through
    /// [`crate::builder::fill_config_service`] and publish the same map to
    /// the synchronous `config_get` snapshot.
    ///
    /// Unlike [`Self::set_config`], nothing is seeded by the test: the
    /// service holds precisely what the table held when this was called.
    /// Calling it a second time models a process restart against the same
    /// database.
    ///
    /// NOT a full boot. `impresspress_server::build_native_runtime` additionally publishes
    /// `BLOCK_SETTINGS_CONFIG_KEY` and `RUN_MIGRATIONS_KEY` to both surfaces,
    /// passes the declared-key filtered process environment into
    /// `seed_and_load` rather than `&[]`, installs a `ConfigSource` (the
    /// loaded variables over the process environment), and registers the
    /// config block through
    /// `ImpresspressBuilder::build()` rather than by hand. A test that needs
    /// any of those — block settings in particular — wants
    /// `impresspress/tests/boot_lifecycle.rs`'s `build_native_runtime`
    /// harness, which drives the factory the binary itself uses.
    ///
    /// CLOBBERS the synchronous snapshot rather than merging into it, which
    /// is deliberate: a restart genuinely drops in-memory state, and merging
    /// would let a pre-restart value survive and make a
    /// `config_get`-after-restart assertion lie. The cost is that anything
    /// [`Self::set_config`] put there is discarded — notably the
    /// `TEST_JWT_SECRET` recipe below — so seed through the `variables` table
    /// if a value must survive this call.
    ///
    /// PRECONDITION: admin migrations have run, so the `variables` table
    /// exists — `seed_and_load` documents the same requirement.
    pub async fn boot_config_service(&mut self) {
        self.boot_config_service_with(&[]).await;
    }

    /// Run the production boot-time `sensitive`-flag reconciliation
    /// ([`crate::platform_state::variables::repair_sensitive_flags`]) over this
    /// fixture's database — the pass native and the browser get inside
    /// `seed_and_load` and Cloudflare gets from its deploy hook.
    ///
    /// Exists so a test can drive it without a boot: it is deliberately
    /// un-gated, and a test that reached it only through `seed_and_load` could
    /// not tell that apart from `admin::settings::seed_defaults`' hash-gated
    /// path.
    pub async fn repair_sensitive_flags(&self) {
        crate::platform_state::variables::repair_sensitive_flags(&self.db_service).await;
    }

    /// Run the production boot seeder
    /// ([`crate::platform_state::variables::seed_and_load`]) over this
    /// fixture's database with `env_vars` — the batch `impresspress_server::build_native_runtime` builds
    /// from the process environment and hands it on native.
    ///
    /// The way to model "an operator set this in the deployment environment"
    /// without touching `std::env`, which is `unsafe` in Rust 2024 and races
    /// every other test in the binary. Writes rows only; use
    /// [`Self::boot_config_service`] as well if the test also needs the
    /// config service filled from them.
    ///
    /// PRECONDITION: admin migrations have run, so the `variables` table
    /// exists.
    pub async fn seed_env_vars(&self, env_vars: &[(&str, &str)]) {
        self.try_seed_env_vars(env_vars)
            .await
            .expect("seed env vars at boot");
    }

    /// [`Self::seed_env_vars`] without the `expect`, for a test that is about
    /// the seeder's behaviour WHEN THE DATABASE FAILS.
    ///
    /// Exists because [`Self::break_list_reads`] makes `seed_and_load` return
    /// `Err` from its final table read, so the panicking wrapper cannot reach
    /// the per-key branches that run before it — and the per-key
    /// read-failure branch ("cannot tell whether this row is pinned, so leave
    /// it alone") is exactly the kind that goes uncovered and then goes wrong.
    pub async fn try_seed_env_vars(&self, env_vars: &[(&str, &str)]) -> Result<(), String> {
        let owned: Vec<(String, String)> = env_vars
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        crate::platform_state::variables::seed_and_load(&self.db_service, &owned)
            .await
            .map(|_| ())
    }

    /// [`Self::boot_config_service`], then layer `adapter_values` onto BOTH
    /// config surfaces the way a target's boot hook does after seeding — the
    /// browser adapter's `RuntimeConfig::republish` of
    /// `__IMPRESSPRESS_RUNTIME_KIND__ = "browser"`, say.
    ///
    /// Exists so a test can put a value in the boot map that the variables
    /// table does NOT hold, which is the only way to check precedence between
    /// the two: a fixture that can only seed from the table cannot express an
    /// adapter-injected value at all.
    pub async fn boot_config_service_with(&mut self, adapter_values: &[(&str, &str)]) {
        let mut vars = crate::platform_state::variables::seed_and_load(&self.db_service, &[])
            .await
            .expect("seed and load variables at boot");
        for (key, value) in adapter_values {
            vars.insert((*key).to_string(), (*value).to_string());
        }

        self.config = Arc::new(vars);
        self.install_config_block();
    }

    /// Serve `wafer-run/config` from the production
    /// [`crate::blocks::config::VariablesConfigBlock`]: this fixture's
    /// `variables` table, over a boot map holding the synchronous snapshot.
    ///
    /// The block `builder::registration` registers, not wafer-core's: a
    /// fixture that wired up a different config block would certify a path
    /// production does not take, which is how the config-store defect
    /// survived a green suite in the first place. It reads through the same
    /// `DatabaseService` the database block does, as production's does, so
    /// [`Self::break_reads`] and [`Self::break_writes`] reach it too.
    fn install_config_block(&mut self) {
        let svc: Arc<dyn wafer_core::interfaces::config::service::ConfigService> =
            Arc::new(wafer_core::service_blocks::config::EnvConfigService::new());
        let svc = crate::builder::fill_config_service(svc, (*self.config).clone());
        let block: Arc<dyn Block> = Arc::new(crate::blocks::config::VariablesConfigBlock::new(
            svc,
            self.db_service.clone(),
        ));
        self.register_block("wafer-run/config", block);
        self.config_store = true;
    }

    /// Register a block under `name`. Calls to `ctx.call_block(name, ...)`
    /// will route to this block's `handle()`.
    ///
    /// Used to wire up cross-block call tests — e.g. the dashboard handler
    /// in the auth block calls `"impresspress/userportal"` for the buttons
    /// list; tests register a real or fake `UserPortalBlock` so the call
    /// resolves.
    ///
    /// Admitted as the runtime admits a block — see [`dispatch::admit`]: a
    /// block the runtime refuses to register (a name it reports differently,
    /// a declaration `BlockInfo::validate` rejects) panics here rather than
    /// running in a fixture production would never boot.
    pub fn register_block(&mut self, name: &str, block: Arc<dyn Block>) {
        let info = dispatch::admit(name, block.clone());
        // Keep `block_infos` a deduplicated mirror of `blocks`'s keys — a
        // block re-registered under the same name (e.g. `set_config`
        // calling `register_block("wafer-run/config", ..)` again) replaces
        // its old entry rather than appending a duplicate.
        self.block_infos.retain(|b| b.name != name);
        self.block_infos.push(info);

        self.blocks
            .lock()
            .expect("blocks mutex poisoned")
            .insert(name.to_string(), block);
        self.wrap_grants = Arc::default();
    }

    /// Put a `BlockInfo` into `Context::registered_blocks()` without a block
    /// behind it.
    ///
    /// [`Self::register_block`]'s snapshot half, for the one registration a
    /// fixture cannot express through it: a *dynamic* sandbox block. Those
    /// are compiled guests — the fixture drives them through
    /// `blocks::dev::test_support::FakeControl`, which has no `dyn Block` to
    /// hand over — yet the real runtime registers each live one via
    /// `ImpresspressBuilder::extra_block` on every rebuild, so its `BlockInfo`
    /// IS in the sealed snapshot that `registered_blocks()` returns. A test
    /// that wants to reason about what a *rebuilt* runtime looks like has to
    /// be able to say so; without this it can only ever see the built-ins,
    /// which is precisely how the sandbox shipped a rule that refused a block
    /// its own agent tool names on recompile.
    ///
    /// Same de-duplication as `register_block`, and the same admission: the
    /// runtime would refuse to register a block reporting `info`, so the
    /// fixture refuses to snapshot it.
    pub fn register_block_info(&mut self, name: &str, info: wafer_run::BlockInfo) {
        let info = dispatch::admit(name, Arc::new(dispatch::Declared(info)));
        self.block_infos.retain(|b| b.name != name);
        self.block_infos.push(info);
    }

    /// Make every `config.set` fail with an `Internal` error while reads keep
    /// answering from the registered config block — the shape a config store
    /// that cannot be written takes. Used to prove a save handler reports the
    /// failure instead of a success.
    pub fn refuse_config_writes(&mut self) {
        self.refuse_config_op(
            wafer_block::common::ServiceOp::CONFIG_SET,
            None,
            WaferError::new(ErrorCode::Internal, "simulated config write failure"),
        );
    }

    /// Make every `config.get` answer `error` while writes keep reaching the
    /// registered config block — a config read the caller is refused. Used to
    /// prove a handler sends the refusal through its classifier instead of
    /// answering with a default or echoing the refusal's own text.
    pub fn refuse_config_reads(&mut self, error: WaferError) {
        self.refuse_config_op(wafer_block::common::ServiceOp::CONFIG_GET, None, error);
    }

    /// [`Self::refuse_config_reads`] for the one key `key`: every other read
    /// reaches the registered config block. Reproduces a caller that holds a
    /// grant for most keys but not this one.
    pub fn refuse_config_reads_of(&mut self, key: &str, error: WaferError) {
        self.refuse_config_op(
            wafer_block::common::ServiceOp::CONFIG_GET,
            Some(key.to_string()),
            error,
        );
    }

    fn refuse_config_op(&mut self, op: &'static str, key: Option<String>, error: WaferError) {
        let inner = self
            .blocks
            .lock()
            .expect("blocks mutex poisoned")
            .get("wafer-run/config")
            .cloned()
            .expect("every fixture registers a config block");
        self.register_block(
            "wafer-run/config",
            Arc::new(RefusingConfigOp {
                inner,
                op,
                key,
                error,
            }),
        );
    }

    /// Replace the database backing this context with one whose mutating
    /// operations (`create`/`update`/`delete`/`upsert`) always fail with a
    /// simulated operational error, while every read (`get`/`list`/`count`/
    /// schema checks/…) still delegates to the real in-memory SQLite data.
    ///
    /// Used to test that a mutation handler surfaces — rather than
    /// discards — a genuine persistence failure (e.g. it must not write a
    /// success audit-log row or report success to the caller). Reads still
    /// working means a handler's "read current state, then try to persist a
    /// change" shape (block-settings toggle, etc.) exercises the exact
    /// branch under test instead of failing earlier for an unrelated reason.
    pub fn break_writes(self) -> Self {
        self.wrap_database_service(|inner| Arc::new(FailingWritesDb { inner }))
    }

    /// Put a decorator between every database call this context makes and
    /// the in-memory SQLite behind it: `wrap` receives the current service
    /// and returns the one the database block — and so every `db::*` call a
    /// block under test makes — goes through from now on. What
    /// [`Self::break_writes`] and [`Self::break_reads`] are built on, and how
    /// a test observes which database operations a code path issues.
    pub fn wrap_database_service(
        mut self,
        wrap: impl FnOnce(
            Arc<dyn wafer_core::interfaces::database::service::DatabaseService>,
        )
            -> Arc<dyn wafer_core::interfaces::database::service::DatabaseService>,
    ) -> Self {
        let wrapped = wrap(self.db_service.clone());
        self.db_service = wrapped.clone();
        self.database_block = Arc::new(wafer_core::service_blocks::database::DatabaseBlock::new(
            wrapped,
        ));
        if self.config_store {
            self.install_config_block();
        }
        self
    }

    /// Replace the database backing this context with one whose read
    /// operations (`get`/`list`/`count`/`sum`/`query_raw`/`aggregate`) always
    /// fail with a simulated operational error, while schema checks and
    /// mutating operations still delegate to the real in-memory SQLite data.
    ///
    /// The mirror image of [`Self::break_writes`]: used to test that a
    /// read/repository function surfaces — rather than swallows — a genuine
    /// read failure instead of collapsing it into the same "not found" /
    /// zero-valued result it uses for a legitimate absence.
    pub fn break_reads(self) -> Self {
        self.with_failing_reads(true)
    }

    /// Like [`Self::break_reads`], but single-row `get` still delegates to the
    /// real data: only the multi-row reads (`list`/`count`/`sum`/`aggregate`/
    /// `query_raw`) fail.
    ///
    /// This is the shape a "load one record, then query for related rows"
    /// handler actually faces when the database wobbles — the single-row read
    /// lands and the follow-up query is the one that fails.
    /// [`Self::break_reads`] cannot reach that branch at all, because it fails
    /// the first read and the handler never gets as far as the query under
    /// test.
    pub fn break_list_reads(self) -> Self {
        self.with_failing_reads(false)
    }

    fn with_failing_reads(self, fail_get: bool) -> Self {
        self.wrap_database_service(|inner| Arc::new(FailingReadsDb { inner, fail_get }))
    }

    /// Record the writes every database call from here on makes — see
    /// [`WriteLog`] — while forwarding each to the real database. The log
    /// is how a test proves a code path writes a table in one call rather
    /// than one per row.
    pub fn record_writes(self) -> (Self, Arc<std::sync::Mutex<WriteLog>>) {
        let log = Arc::new(std::sync::Mutex::new(WriteLog::default()));
        let recorder = log.clone();
        let ctx = self.wrap_database_service(move |inner| {
            Arc::new(RecordingDb {
                inner,
                log: recorder,
            })
        });
        (ctx, log)
    }

    /// Toggle `STRICT_SCHEMA` directly on the backing `DatabaseService`, the
    /// same flag production sets from `WAFER_RUN__DATABASE__STRICT_SCHEMA`
    /// at the database block's own `lifecycle(Init)`
    /// (`wafer_core::interfaces::database::handler::handle_lifecycle`).
    /// `TestContext` never boots that lifecycle event (migrations are
    /// applied directly via [`Self::apply_block_migrations`], not through
    /// it), so there is no config value to flip here — this reaches the
    /// service's own `set_strict_schema` directly instead, after whatever
    /// migrations already ran (this only changes how *future* writes are
    /// validated: no schema introspection, no lazy `ALTER TABLE ADD
    /// COLUMN`), so a test can call this once its fixture's migrations are
    /// in place and then exercise writes exactly as a strict-schema
    /// production deployment (Cloudflare/D1) would.
    pub fn set_strict_schema(&self, enabled: bool) {
        self.db_service.set_strict_schema(enabled);
    }
}

/// The writes a code path made through [`TestContext::record_writes`], by
/// operation, with the size of every multi-write call.
#[derive(Debug, Default)]
pub struct WriteLog {
    /// Single-row `create` calls.
    pub creates: usize,
    /// Single-row `upsert` calls.
    pub upserts: usize,
    /// Single-row `delete` calls.
    pub deletes: usize,
    /// Rows per `create_many` call, in call order.
    pub create_many_rows: Vec<usize>,
    /// Ops per `batch` call, in call order.
    pub batch_ops: Vec<usize>,
}

/// The decorator behind [`TestContext::record_writes`]: notes the writes
/// [`WriteLog`] counts and forwards every call, those included, to `inner`.
/// Errors come back unchanged, so a duplicate key is still the
/// `AlreadyExists` the `DatabaseService` contract names.
struct RecordingDb {
    inner: Arc<dyn wafer_core::interfaces::database::service::DatabaseService>,
    log: Arc<std::sync::Mutex<WriteLog>>,
}

impl RecordingDb {
    fn inner_service(&self) -> &dyn wafer_core::interfaces::database::service::DatabaseService {
        self.inner.as_ref()
    }

    fn note(&self, record: impl FnOnce(&mut WriteLog)) {
        record(&mut self.log.lock().expect("write log"));
    }
}

wafer_core::forward_database_service! {
    impl DatabaseService for RecordingDb {
        forward_to inner_service();

        ops {
            get: forward,
            list: forward,
            create: custom,
            create_many: custom,
            update: forward,
            delete: custom,
            count: forward,
            sum: forward,
            query_raw: forward,
            exec_raw: forward,
            delete_where: forward,
            delete_where_count: forward,
            take_where: forward,
            update_where: forward,
            update_where_count: forward,
            increment_field_where: forward,
            upsert: custom,
            aggregate: forward,
            batch: custom,
            insert_guarded: forward,
            update_guarded: forward,
            ensure_schema_table: forward,
            ensure_schema_tables: forward,
            schema_table_exists: forward,
            schema_columns: forward,
            schema_drop_table: forward,
            schema_add_column: forward,
            set_strict_schema: forward,
            statement_budget: forward,
        }

        async fn create(
            &self,
            collection: &str,
            data: HashMap<String, serde_json::Value>,
        ) -> Result<
            wafer_core::interfaces::database::service::Record,
            wafer_core::interfaces::database::service::DatabaseError,
        > {
            self.note(|log| log.creates += 1);
            self.inner.create(collection, data).await
        }

        async fn create_many(
            &self,
            collection: &str,
            rows: Vec<HashMap<String, serde_json::Value>>,
        ) -> Result<i64, wafer_core::interfaces::database::service::DatabaseError> {
            self.note(|log| log.create_many_rows.push(rows.len()));
            self.inner.create_many(collection, rows).await
        }

        async fn delete(
            &self,
            collection: &str,
            id: &str,
        ) -> Result<(), wafer_core::interfaces::database::service::DatabaseError> {
            self.note(|log| log.deletes += 1);
            self.inner.delete(collection, id).await
        }

        async fn upsert(
            &self,
            collection: &str,
            spec: wafer_core::interfaces::database::service::UpsertSpec,
        ) -> Result<Option<wafer_core::interfaces::database::service::Record>, wafer_core::interfaces::database::service::DatabaseError> {
            self.note(|log| log.upserts += 1);
            self.inner.upsert(collection, spec).await
        }

        async fn batch(
            &self,
            ops: Vec<wafer_core::interfaces::database::service::WriteOp>,
        ) -> Result<
            Vec<wafer_core::interfaces::database::service::WriteOutcome>,
            wafer_core::interfaces::database::service::DatabaseError,
        > {
            self.note(|log| log.batch_ops.push(ops.len()));
            self.inner.batch(ops).await
        }
    }
}

/// `DatabaseService` decorator used by [`TestContext::break_reads`] and
/// [`TestContext::break_list_reads`]. Every read method fails with
/// [`DatabaseError::Internal`]; every mutating/schema method delegates to
/// `inner` unchanged, so a write that duplicates a key still fails as the
/// `AlreadyExists` the `DatabaseService` contract names.
///
/// "Mutating" includes the filtered-write family (`update_where*`,
/// `delete_where*`, `increment_field_where`), which this decorator must
/// override explicitly even though it has nothing to change about them. The
/// `DatabaseService` trait ships *read-based default implementations* of
/// those — `update_where_count` counts and then updates, `update_where`
/// lists and then updates by id — so a decorator that leaves them alone
/// inherits a write that begins with a read, and every filtered write fails
/// here for a reason no real backend has. `wafer-block-sqlite`,
/// `wafer-block-postgres` and `D1DatabaseService` all override the family
/// with a single statement carrying no `count` and no `list`, so a test
/// double that keeps the defaults asserts against a database that does not
/// exist: a handler branch reachable only when a filtered write fails looks
/// covered while production can never enter it.
struct FailingReadsDb {
    inner: Arc<dyn wafer_core::interfaces::database::service::DatabaseService>,
    /// `false` exempts single-row `get` from the failure, so a test can fail
    /// a *listing* while a by-id read still succeeds — see
    /// [`TestContext::break_list_reads`].
    fail_get: bool,
}

#[async_trait::async_trait]
impl wafer_core::interfaces::database::service::DatabaseService for FailingReadsDb {
    fn statement_budget(
        &self,
    ) -> Result<
        wafer_core::interfaces::database::service::StatementBudget,
        wafer_core::interfaces::database::service::DatabaseError,
    > {
        self.inner.statement_budget()
    }
    async fn get(
        &self,
        collection: &str,
        id: &str,
    ) -> Result<
        wafer_core::interfaces::database::service::Record,
        wafer_core::interfaces::database::service::DatabaseError,
    > {
        if self.fail_get {
            return Err(simulated_read_failure());
        }
        self.inner.get(collection, id).await
    }

    async fn list(
        &self,
        _collection: &str,
        _opts: &wafer_block::db::ListOptions,
    ) -> Result<
        wafer_core::interfaces::database::service::RecordList,
        wafer_core::interfaces::database::service::DatabaseError,
    > {
        Err(simulated_read_failure())
    }

    async fn create(
        &self,
        collection: &str,
        data: HashMap<String, serde_json::Value>,
    ) -> Result<
        wafer_core::interfaces::database::service::Record,
        wafer_core::interfaces::database::service::DatabaseError,
    > {
        self.inner.create(collection, data).await
    }

    async fn update(
        &self,
        collection: &str,
        id: &str,
        data: HashMap<String, serde_json::Value>,
    ) -> Result<
        wafer_core::interfaces::database::service::Record,
        wafer_core::interfaces::database::service::DatabaseError,
    > {
        self.inner.update(collection, id, data).await
    }

    async fn delete(
        &self,
        collection: &str,
        id: &str,
    ) -> Result<(), wafer_core::interfaces::database::service::DatabaseError> {
        self.inner.delete(collection, id).await
    }

    // --- the filtered-write family ---------------------------------------
    //
    // Delegated, not failed. Each is one `UPDATE`/`DELETE … WHERE` statement
    // on every real backend; the trait's read-based defaults (see the struct
    // doc above) are what these overrides exist to displace.
    //
    // `take_where` is deliberately NOT here: it returns the rows it removed,
    // so it is a read as much as a write and belongs on the failing side. It
    // is stated there explicitly, for the same reason the family below is
    // stated here — the trait's default reaches the failure through a `list`
    // the real backends' single `DELETE … RETURNING *` never issues, so
    // inheriting it would make the double right by accident.

    async fn update_where(
        &self,
        collection: &str,
        filters: &[wafer_block::db::Filter],
        data: HashMap<String, serde_json::Value>,
    ) -> Result<(), wafer_core::interfaces::database::service::DatabaseError> {
        self.inner.update_where(collection, filters, data).await
    }

    async fn update_where_count(
        &self,
        collection: &str,
        filters: &[wafer_block::db::Filter],
        data: HashMap<String, serde_json::Value>,
    ) -> Result<i64, wafer_core::interfaces::database::service::DatabaseError> {
        self.inner
            .update_where_count(collection, filters, data)
            .await
    }

    async fn delete_where(
        &self,
        collection: &str,
        filters: &[wafer_block::db::Filter],
    ) -> Result<(), wafer_core::interfaces::database::service::DatabaseError> {
        self.inner.delete_where(collection, filters).await
    }

    async fn delete_where_count(
        &self,
        collection: &str,
        filters: &[wafer_block::db::Filter],
    ) -> Result<i64, wafer_core::interfaces::database::service::DatabaseError> {
        self.inner.delete_where_count(collection, filters).await
    }

    async fn increment_field_where(
        &self,
        collection: &str,
        col: &str,
        delta: i64,
        filters: &[wafer_block::db::Filter],
    ) -> Result<i64, wafer_core::interfaces::database::service::DatabaseError> {
        self.inner
            .increment_field_where(collection, col, delta, filters)
            .await
    }

    // The one member of that family on the failing side: it hands back the
    // rows it removed.
    async fn take_where(
        &self,
        _collection: &str,
        _filters: &[wafer_block::db::Filter],
    ) -> Result<
        Vec<wafer_core::interfaces::database::service::Record>,
        wafer_core::interfaces::database::service::DatabaseError,
    > {
        Err(simulated_read_failure())
    }

    async fn count(
        &self,
        _collection: &str,
        _filters: &[wafer_block::db::Filter],
    ) -> Result<i64, wafer_core::interfaces::database::service::DatabaseError> {
        Err(simulated_read_failure())
    }

    async fn sum(
        &self,
        _collection: &str,
        _field: &str,
        _filters: &[wafer_block::db::Filter],
    ) -> Result<f64, wafer_core::interfaces::database::service::DatabaseError> {
        Err(simulated_read_failure())
    }

    async fn query_raw(
        &self,
        _query: &str,
        _args: &[serde_json::Value],
    ) -> Result<
        Vec<wafer_core::interfaces::database::service::Record>,
        wafer_core::interfaces::database::service::DatabaseError,
    > {
        Err(simulated_read_failure())
    }

    async fn exec_raw(
        &self,
        query: &str,
        args: &[serde_json::Value],
    ) -> Result<i64, wafer_core::interfaces::database::service::DatabaseError> {
        self.inner.exec_raw(query, args).await
    }

    async fn upsert(
        &self,
        collection: &str,
        spec: wafer_core::interfaces::database::service::UpsertSpec,
    ) -> Result<
        Option<wafer_core::interfaces::database::service::Record>,
        wafer_core::interfaces::database::service::DatabaseError,
    > {
        self.inner.upsert(collection, spec).await
    }

    // The multi-write and guarded ops are writes: delegated, like `create`.
    // A guarded write's guard check reads the table inside the write's own
    // transaction, which no real backend can fail separately from the write.

    async fn create_many(
        &self,
        collection: &str,
        rows: Vec<HashMap<String, serde_json::Value>>,
    ) -> Result<i64, wafer_core::interfaces::database::service::DatabaseError> {
        self.inner.create_many(collection, rows).await
    }

    async fn batch(
        &self,
        ops: Vec<wafer_core::interfaces::database::service::WriteOp>,
    ) -> Result<
        Vec<wafer_core::interfaces::database::service::WriteOutcome>,
        wafer_core::interfaces::database::service::DatabaseError,
    > {
        self.inner.batch(ops).await
    }

    async fn insert_guarded(
        &self,
        collection: &str,
        data: HashMap<String, serde_json::Value>,
        guards: &[wafer_core::interfaces::database::service::CapGuard],
    ) -> Result<
        wafer_core::interfaces::database::service::GuardedInsert,
        wafer_core::interfaces::database::service::DatabaseError,
    > {
        self.inner.insert_guarded(collection, data, guards).await
    }

    async fn update_guarded(
        &self,
        collection: &str,
        filters: &[wafer_block::db::Filter],
        data: HashMap<String, serde_json::Value>,
        guards: &[wafer_core::interfaces::database::service::CapGuard],
    ) -> Result<
        wafer_core::interfaces::database::service::GuardedUpdate,
        wafer_core::interfaces::database::service::DatabaseError,
    > {
        self.inner
            .update_guarded(collection, filters, data, guards)
            .await
    }

    async fn aggregate(
        &self,
        _collection: &str,
        _spec: wafer_core::interfaces::database::service::AggregateSpec,
    ) -> Result<
        Vec<wafer_core::interfaces::database::service::Record>,
        wafer_core::interfaces::database::service::DatabaseError,
    > {
        Err(simulated_read_failure())
    }

    async fn ensure_schema_table(
        &self,
        table: &wafer_core::interfaces::database::service::Table,
    ) -> Result<(), wafer_core::interfaces::database::service::DatabaseError> {
        self.inner.ensure_schema_table(table).await
    }

    async fn schema_table_exists(
        &self,
        name: &str,
    ) -> Result<bool, wafer_core::interfaces::database::service::DatabaseError> {
        self.inner.schema_table_exists(name).await
    }

    async fn schema_columns(
        &self,
        table: &str,
    ) -> Result<Vec<String>, wafer_core::interfaces::database::service::DatabaseError> {
        self.inner.schema_columns(table).await
    }

    async fn schema_drop_table(
        &self,
        name: &str,
    ) -> Result<(), wafer_core::interfaces::database::service::DatabaseError> {
        self.inner.schema_drop_table(name).await
    }

    async fn schema_add_column(
        &self,
        table: &str,
        column: &wafer_core::interfaces::database::service::Column,
    ) -> Result<(), wafer_core::interfaces::database::service::DatabaseError> {
        self.inner.schema_add_column(table, column).await
    }
}

fn simulated_read_failure() -> wafer_core::interfaces::database::service::DatabaseError {
    wafer_core::interfaces::database::service::DatabaseError::Internal(
        "simulated operational read failure (TestContext::break_reads)".into(),
    )
}

/// `DatabaseService` decorator used by [`TestContext::break_writes`]. Every
/// mutating method fails with [`DatabaseError::Internal`] — a broken
/// database, never a taken key, which the `DatabaseService` contract reports
/// as `AlreadyExists` — and every read/schema method delegates to `inner`
/// unchanged.
///
/// "Mutating" includes the filtered-write family (`update_where*`,
/// `delete_where*`, `take_where`, `increment_field_where`), which this
/// decorator must override explicitly — the mirror of the same paragraph on
/// [`FailingReadsDb`]. The `DatabaseService` trait ships *read-based default
/// implementations* of those: `update_where_count` counts and then updates,
/// `update_where` lists and then updates by id, `delete_where` lists and then
/// deletes by id, `take_where` lists and then deletes, and
/// `increment_field_where` reports that the backend does not implement it at
/// all. `wafer-block-sqlite`, `wafer-block-postgres` and `D1DatabaseService`
/// all override the family with a single statement carrying no `count` and no
/// `list`.
///
/// Inheriting the defaults here did not merely reach the right answer by the
/// wrong route — it reached the WRONG answer whenever the filter matched no
/// rows: the default lists nothing, writes nothing, and returns `Ok`, so a
/// filtered write SUCCEEDED on a database whose writes are supposed to be
/// failing. `repo::products::restore` of an already-live product is exactly
/// that write. A handler branch reachable only when a filtered write fails
/// then looks covered while production can never enter it; the restore
/// collision-probe test in `products/tests/handler_tests.rs` was that test on
/// [`FailingReadsDb`], and this double kept the same defect armed for the
/// next one.
struct FailingWritesDb {
    inner: Arc<dyn wafer_core::interfaces::database::service::DatabaseService>,
}

#[async_trait::async_trait]
impl wafer_core::interfaces::database::service::DatabaseService for FailingWritesDb {
    fn statement_budget(
        &self,
    ) -> Result<
        wafer_core::interfaces::database::service::StatementBudget,
        wafer_core::interfaces::database::service::DatabaseError,
    > {
        self.inner.statement_budget()
    }
    async fn get(
        &self,
        collection: &str,
        id: &str,
    ) -> Result<
        wafer_core::interfaces::database::service::Record,
        wafer_core::interfaces::database::service::DatabaseError,
    > {
        self.inner.get(collection, id).await
    }

    async fn list(
        &self,
        collection: &str,
        opts: &wafer_block::db::ListOptions,
    ) -> Result<
        wafer_core::interfaces::database::service::RecordList,
        wafer_core::interfaces::database::service::DatabaseError,
    > {
        self.inner.list(collection, opts).await
    }

    async fn create(
        &self,
        _collection: &str,
        _data: HashMap<String, serde_json::Value>,
    ) -> Result<
        wafer_core::interfaces::database::service::Record,
        wafer_core::interfaces::database::service::DatabaseError,
    > {
        Err(simulated_write_failure())
    }

    async fn update(
        &self,
        _collection: &str,
        _id: &str,
        _data: HashMap<String, serde_json::Value>,
    ) -> Result<
        wafer_core::interfaces::database::service::Record,
        wafer_core::interfaces::database::service::DatabaseError,
    > {
        Err(simulated_write_failure())
    }

    async fn delete(
        &self,
        _collection: &str,
        _id: &str,
    ) -> Result<(), wafer_core::interfaces::database::service::DatabaseError> {
        Err(simulated_write_failure())
    }

    // --- the filtered-write family ---------------------------------------
    //
    // Failed, not inherited. See the struct doc above: every one of these is
    // one `UPDATE`/`DELETE … WHERE` statement on every real backend, and the
    // trait's read-based defaults answer `Ok` for a filter that matches
    // nothing — a successful write on a database whose writes are failing.

    async fn update_where(
        &self,
        _collection: &str,
        _filters: &[wafer_block::db::Filter],
        _data: HashMap<String, serde_json::Value>,
    ) -> Result<(), wafer_core::interfaces::database::service::DatabaseError> {
        Err(simulated_write_failure())
    }

    async fn update_where_count(
        &self,
        _collection: &str,
        _filters: &[wafer_block::db::Filter],
        _data: HashMap<String, serde_json::Value>,
    ) -> Result<i64, wafer_core::interfaces::database::service::DatabaseError> {
        Err(simulated_write_failure())
    }

    async fn delete_where(
        &self,
        _collection: &str,
        _filters: &[wafer_block::db::Filter],
    ) -> Result<(), wafer_core::interfaces::database::service::DatabaseError> {
        Err(simulated_write_failure())
    }

    async fn delete_where_count(
        &self,
        _collection: &str,
        _filters: &[wafer_block::db::Filter],
    ) -> Result<i64, wafer_core::interfaces::database::service::DatabaseError> {
        Err(simulated_write_failure())
    }

    async fn take_where(
        &self,
        _collection: &str,
        _filters: &[wafer_block::db::Filter],
    ) -> Result<
        Vec<wafer_core::interfaces::database::service::Record>,
        wafer_core::interfaces::database::service::DatabaseError,
    > {
        Err(simulated_write_failure())
    }

    async fn increment_field_where(
        &self,
        _collection: &str,
        _col: &str,
        _delta: i64,
        _filters: &[wafer_block::db::Filter],
    ) -> Result<i64, wafer_core::interfaces::database::service::DatabaseError> {
        Err(simulated_write_failure())
    }

    async fn count(
        &self,
        collection: &str,
        filters: &[wafer_block::db::Filter],
    ) -> Result<i64, wafer_core::interfaces::database::service::DatabaseError> {
        self.inner.count(collection, filters).await
    }

    async fn sum(
        &self,
        collection: &str,
        field: &str,
        filters: &[wafer_block::db::Filter],
    ) -> Result<f64, wafer_core::interfaces::database::service::DatabaseError> {
        self.inner.sum(collection, field, filters).await
    }

    async fn query_raw(
        &self,
        query: &str,
        args: &[serde_json::Value],
    ) -> Result<
        Vec<wafer_core::interfaces::database::service::Record>,
        wafer_core::interfaces::database::service::DatabaseError,
    > {
        self.inner.query_raw(query, args).await
    }

    async fn exec_raw(
        &self,
        _query: &str,
        _args: &[serde_json::Value],
    ) -> Result<i64, wafer_core::interfaces::database::service::DatabaseError> {
        Err(simulated_write_failure())
    }

    async fn upsert(
        &self,
        _collection: &str,
        _spec: wafer_core::interfaces::database::service::UpsertSpec,
    ) -> Result<
        Option<wafer_core::interfaces::database::service::Record>,
        wafer_core::interfaces::database::service::DatabaseError,
    > {
        Err(simulated_write_failure())
    }

    async fn create_many(
        &self,
        _collection: &str,
        _rows: Vec<HashMap<String, serde_json::Value>>,
    ) -> Result<i64, wafer_core::interfaces::database::service::DatabaseError> {
        Err(simulated_write_failure())
    }

    async fn batch(
        &self,
        _ops: Vec<wafer_core::interfaces::database::service::WriteOp>,
    ) -> Result<
        Vec<wafer_core::interfaces::database::service::WriteOutcome>,
        wafer_core::interfaces::database::service::DatabaseError,
    > {
        Err(simulated_write_failure())
    }

    async fn insert_guarded(
        &self,
        _collection: &str,
        _data: HashMap<String, serde_json::Value>,
        _guards: &[wafer_core::interfaces::database::service::CapGuard],
    ) -> Result<
        wafer_core::interfaces::database::service::GuardedInsert,
        wafer_core::interfaces::database::service::DatabaseError,
    > {
        Err(simulated_write_failure())
    }

    async fn update_guarded(
        &self,
        _collection: &str,
        _filters: &[wafer_block::db::Filter],
        _data: HashMap<String, serde_json::Value>,
        _guards: &[wafer_core::interfaces::database::service::CapGuard],
    ) -> Result<
        wafer_core::interfaces::database::service::GuardedUpdate,
        wafer_core::interfaces::database::service::DatabaseError,
    > {
        Err(simulated_write_failure())
    }

    async fn aggregate(
        &self,
        collection: &str,
        spec: wafer_core::interfaces::database::service::AggregateSpec,
    ) -> Result<
        Vec<wafer_core::interfaces::database::service::Record>,
        wafer_core::interfaces::database::service::DatabaseError,
    > {
        self.inner.aggregate(collection, spec).await
    }

    async fn ensure_schema_table(
        &self,
        table: &wafer_core::interfaces::database::service::Table,
    ) -> Result<(), wafer_core::interfaces::database::service::DatabaseError> {
        self.inner.ensure_schema_table(table).await
    }

    async fn schema_table_exists(
        &self,
        name: &str,
    ) -> Result<bool, wafer_core::interfaces::database::service::DatabaseError> {
        self.inner.schema_table_exists(name).await
    }

    async fn schema_columns(
        &self,
        table: &str,
    ) -> Result<Vec<String>, wafer_core::interfaces::database::service::DatabaseError> {
        self.inner.schema_columns(table).await
    }

    async fn schema_drop_table(
        &self,
        name: &str,
    ) -> Result<(), wafer_core::interfaces::database::service::DatabaseError> {
        self.inner.schema_drop_table(name).await
    }

    async fn schema_add_column(
        &self,
        table: &str,
        column: &wafer_core::interfaces::database::service::Column,
    ) -> Result<(), wafer_core::interfaces::database::service::DatabaseError> {
        self.inner.schema_add_column(table, column).await
    }
}

fn simulated_write_failure() -> wafer_core::interfaces::database::service::DatabaseError {
    wafer_core::interfaces::database::service::DatabaseError::Internal(
        "simulated operational write failure (TestContext::break_writes)".into(),
    )
}

#[async_trait::async_trait]
impl Context for TestContext {
    /// Host-side WRAP enforcement, on every frame: the decision
    /// `RuntimeContext::check_resource_access` makes — `wafer_block::wrap::check_access`
    /// keyed on the caller, over every grant the deployment carries (see
    /// [`dispatch::collect_wrap_grants`]), with `impresspress/admin` as the
    /// admin block. The caller is this frame's [`dispatch::Caller`]: the
    /// calling block, the admin block when the test itself called, and
    /// nobody — refused, as in production — for a top-level frame.
    ///
    /// The runtime's second check, the caller's declared resource
    /// capabilities, has nothing to act on here: no block this crate
    /// registers declares capabilities, and a block that declares none is
    /// unrestricted on the native runtime.
    fn check_resource_access(
        &self,
        resource: &str,
        resource_type: wafer_run::ResourceType,
        access: wafer_block::ResourceAccess,
    ) -> Result<(), WaferError> {
        wafer_block::wrap::check_access(
            self.called_by.wrap_identity(),
            resource,
            access,
            Some(&resource_type),
            &self.deployment_wrap_grants(),
            crate::blocks::admin::ADMIN_BLOCK_ID,
        )
    }

    /// The same decision as [`Self::check_resource_access`], without the
    /// error — `RuntimeContext::resource_access_admitted` answers from the
    /// same check the same way.
    fn resource_access_admitted(
        &self,
        resource: &str,
        resource_type: wafer_run::ResourceType,
        access: wafer_block::ResourceAccess,
    ) -> bool {
        self.check_resource_access(resource, resource_type, access)
            .is_ok()
    }

    /// `RuntimeContext::dispatch_call` as far as the callee's handler: the
    /// runtime's own admission, [`wafer_run::runtime::call_gates::admit_call`],
    /// over this frame (see the [`CallFrame`](wafer_run::runtime::call_gates::CallFrame)
    /// impl below) — call depth, cancellation, the caller's `requires`, its
    /// `call_block` capability, registration and the interface action, each
    /// refusing with the runtime's code and text — then the callee's handler
    /// on the sub-context the runtime would build ([`Self::for_callee`]).
    ///
    /// Not modelled: the lazy `lifecycle(Init)` the runtime runs on a callee
    /// between admission and its handler. It is not an admission gate — the
    /// runtime runs it against its own init slots after `admit_call` — and a
    /// fixture does a block's Init work (its migrations, its seeds) explicitly
    /// in setup (e.g. [`Self::with_auth`]), so a test states the
    /// schema it runs against.
    ///
    /// WRAP is not a `call_block` gate, here or in production: the service
    /// handler a call reaches authorizes the op it decoded, through
    /// [`Context::check_resource_access`] on the callee frame below.
    async fn call_block(&self, name: &str, msg: Message, input: InputStream) -> OutputStream {
        let admitted = match wafer_run::runtime::call_gates::admit_call(self, name, &msg) {
            Ok(admitted) => admitted,
            Err(refusal) => return OutputStream::error(refusal),
        };
        let callee = self.for_callee(admitted.resolved, admitted.requires);
        admitted.block.handle(&callee, msg, input).await
    }

    /// The block that called into this frame, as `RuntimeContext` re-points
    /// it on every hop: `None` for a top-level frame and for a call the test
    /// made itself.
    ///
    /// Handler code that *reads* its caller behaves as in production — the
    /// storage handler behind `blocks::storage::ImpresspressStorageBlock`
    /// namespaces every plain folder under it, and the crypto block signs a
    /// token under its caller's derived key.
    fn caller_id(&self) -> Option<&str> {
        self.called_by.block()
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn registered_blocks(&self) -> &[wafer_run::BlockInfo] {
        &self.block_infos
    }

    fn config_get(&self, key: &str) -> Option<&str> {
        self.config.get(key).map(String::as_str)
    }

    fn clone_arc(&self) -> Arc<dyn Context> {
        // Cheap — all interior state is `Arc`/`Mutex`-shared.
        Arc::new(self.clone())
    }
}

/// Test double that wraps a [`TestContext`] and turns specific
/// `"wafer-run/database"` service-op calls, scoped to a specific collection
/// (table), into a simulated infra failure — while every other call
/// (including other tables under the same op, and every other database op)
/// passes through to the real in-memory SQLite context untouched.
///
/// Scoping by collection (not just op) matters because many different repo
/// calls share the same wire op kind — e.g. every `update_by_filters` caller
/// sends `"database.update_where"` regardless of which table it targets, so
/// failing the op alone would also break unrelated calls to other tables in
/// the same request flow.
///
/// Used to reproduce "a downstream DB call fails" fault-injection scenarios
/// — e.g. a guard read that must fail closed, or a revocation write whose
/// failure must not be swallowed as success — without needing a fake
/// database backend. Match `(op, collection)` against [`Message::action`]
/// (the wire service-op string, e.g. `"database.get"`,
/// `"database.update_where"` — see `wafer_block::common::ServiceOp`) and the
/// request body's `collection` field.
#[derive(Clone)]
pub struct FailingDbOpContext {
    inner: TestContext,
    failing: Vec<(&'static str, &'static str)>,
    /// The error the matched op answers with. `Internal` (via
    /// [`FailingDbOpContext::new`]) for an infra outage;
    /// [`FailingDbOpContext::failing_with`] picks another code so a test can
    /// reproduce a *caller* error arriving from below — an
    /// `InvalidArgument` a repository guard raised, say — and pin how the
    /// handler above translates it.
    error: WaferError,
    /// How many matching calls to let through before failing (see
    /// [`FailingDbOpContext::after_passing`]). Shared across clones so a
    /// handler's `clone_arc` sees the same countdown.
    passes_before_failing: Arc<std::sync::atomic::AtomicUsize>,
}

/// Request-body shape shared by every `wafer-run/database` wire request:
/// they all carry a `collection` field, and serde ignores the other
/// (irrelevant) fields on decode.
///
/// `pub(crate)` so every context decorator that has to route on the table —
/// not just [`FailingDbOpContext`] — decodes the field through one
/// declaration.
#[derive(serde::Deserialize)]
pub(crate) struct CollectionPeek {
    pub collection: String,
}

impl FailingDbOpContext {
    /// Wrap `inner`, failing every `"wafer-run/database"` call whose
    /// `(msg.action(), request.collection)` matches an entry in `failing`
    /// with a simulated [`ErrorCode::Internal`] error. All other calls pass
    /// through untouched.
    pub fn new(inner: TestContext, failing: Vec<(&'static str, &'static str)>) -> Self {
        Self::failing_with(
            inner,
            failing,
            WaferError::new(ErrorCode::Internal, "simulated database outage"),
        )
    }

    /// [`Self::new`] with the answered error chosen by the caller, for the
    /// codes that are not an outage — a repository guard's `InvalidArgument`,
    /// a `FailedPrecondition`, and so on. A handler that funnels every
    /// non-`NotFound` error into a 500 discards exactly these.
    pub fn failing_with(
        inner: TestContext,
        failing: Vec<(&'static str, &'static str)>,
        error: WaferError,
    ) -> Self {
        Self {
            inner,
            failing,
            error,
            passes_before_failing: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    /// Let the first `n` matching calls through untouched and fail only from
    /// the `n + 1`th on. A handler whose guarded call is preceded by another
    /// call of the same op on the same table (a lookup, then a check) needs
    /// this to isolate the second one.
    pub fn after_passing(self, n: usize) -> Self {
        self.passes_before_failing
            .store(n, std::sync::atomic::Ordering::SeqCst);
        self
    }

    /// Consume one allowed pass. `true` when this matching call should still
    /// reach the inner context.
    fn let_one_pass(&self) -> bool {
        use std::sync::atomic::Ordering;
        self.passes_before_failing
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
    }
}

#[async_trait::async_trait]
impl Context for FailingDbOpContext {
    fn check_resource_access(
        &self,
        resource: &str,
        resource_type: wafer_run::ResourceType,
        access: wafer_block::ResourceAccess,
    ) -> Result<(), WaferError> {
        self.inner
            .check_resource_access(resource, resource_type, access)
    }

    fn resource_access_admitted(
        &self,
        resource: &str,
        resource_type: wafer_run::ResourceType,
        access: wafer_block::ResourceAccess,
    ) -> bool {
        self.inner
            .resource_access_admitted(resource, resource_type, access)
    }

    async fn call_block(&self, name: &str, msg: Message, input: InputStream) -> OutputStream {
        if name == "wafer-run/database" && self.failing.iter().any(|(op, _)| *op == msg.action()) {
            let bytes = match input.collect_to_bytes().await {
                Ok(bytes) => bytes,
                Err(e) => return OutputStream::error(e),
            };
            let collection = wafer_block::codec::decode::<CollectionPeek>(&bytes)
                .map(|p| p.collection)
                .unwrap_or_default();
            if self
                .failing
                .iter()
                .any(|(op, table)| *op == msg.action() && *table == collection)
                && !self.let_one_pass()
            {
                return OutputStream::error(self.error.clone());
            }
            return self
                .inner
                .call_block(name, msg, InputStream::from_bytes(bytes))
                .await;
        }
        self.inner.call_block(name, msg, input).await
    }

    fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }

    fn registered_blocks(&self) -> &[wafer_run::BlockInfo] {
        self.inner.registered_blocks()
    }

    fn config_get(&self, key: &str) -> Option<&str> {
        self.inner.config_get(key)
    }

    fn clone_arc(&self) -> Arc<dyn Context> {
        Arc::new(self.clone())
    }
}

/// [`FailingDbOpContext`] for any other service block: wraps a
/// [`TestContext`] and answers `error` to every call to `block` whose op
/// ([`Message::action`], e.g. `"storage.put"` or `"crypto.compare_hash"` — see
/// `wafer_block::common::ServiceOp`) is one of `failing`, while every other
/// call passes through untouched.
///
/// Matched on the op alone: a storage request names a folder the storage
/// block rewrites into the caller's namespace, and a crypto request carries
/// no resource at all, so the op is what a test can aim at. A handler that
/// makes the same op twice (a manifest read, then a blob read) is isolated
/// with [`Self::after_passing`].
#[derive(Clone)]
pub struct FailingServiceOpContext {
    inner: TestContext,
    block: &'static str,
    failing: Vec<&'static str>,
    error: WaferError,
    /// Matching calls still to let through; shared across clones, as in
    /// [`FailingDbOpContext`].
    passes_before_failing: Arc<std::sync::atomic::AtomicUsize>,
}

impl FailingServiceOpContext {
    /// Wrap `inner`, answering `error` to every call to `block` whose op is in
    /// `failing`.
    pub fn failing_with(
        inner: TestContext,
        block: &'static str,
        failing: Vec<&'static str>,
        error: WaferError,
    ) -> Self {
        Self {
            inner,
            block,
            failing,
            error,
            passes_before_failing: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    /// Let the first `n` matching calls through and fail from the `n + 1`th.
    pub fn after_passing(self, n: usize) -> Self {
        self.passes_before_failing
            .store(n, std::sync::atomic::Ordering::SeqCst);
        self
    }
}

#[async_trait::async_trait]
impl Context for FailingServiceOpContext {
    fn check_resource_access(
        &self,
        resource: &str,
        resource_type: wafer_run::ResourceType,
        access: wafer_block::ResourceAccess,
    ) -> Result<(), WaferError> {
        self.inner
            .check_resource_access(resource, resource_type, access)
    }

    fn resource_access_admitted(
        &self,
        resource: &str,
        resource_type: wafer_run::ResourceType,
        access: wafer_block::ResourceAccess,
    ) -> bool {
        self.inner
            .resource_access_admitted(resource, resource_type, access)
    }

    async fn call_block(&self, name: &str, msg: Message, input: InputStream) -> OutputStream {
        use std::sync::atomic::Ordering;
        if name == self.block
            && self.failing.contains(&msg.action())
            && self
                .passes_before_failing
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                .is_err()
        {
            return OutputStream::error(self.error.clone());
        }
        self.inner.call_block(name, msg, input).await
    }

    fn caller_id(&self) -> Option<&str> {
        self.inner.caller_id()
    }

    fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }

    fn registered_blocks(&self) -> &[wafer_run::BlockInfo] {
        self.inner.registered_blocks()
    }

    fn config_get(&self, key: &str) -> Option<&str> {
        self.inner.config_get(key)
    }

    fn clone_arc(&self) -> Arc<dyn Context> {
        Arc::new(self.clone())
    }
}

/// Test double that wraps a [`TestContext`] and holds the first `n` calls
/// matching `(op, collection)` open until all `n` of them have arrived,
/// releasing them together.
///
/// It is how a test pins a read-then-write race. A handler that reads a row
/// and then writes based on what it read has a window between the two, and two
/// plain concurrent requests do not reliably land inside it — the first can
/// finish its whole sequence before the second reads, and the test then passes
/// on code that has no interlock at all. Rendezvousing on the READ puts every
/// racer past it, holding the same pre-write state, before any of them is
/// allowed to write.
///
/// The hold is applied AFTER the inner call has answered, so no database lock
/// is held while a request waits for its partners. Only the first `n` matching
/// calls are held; later ones pass straight through, so a request that comes
/// back for a second matching call cannot park on a barrier with no partner
/// left to release it.
///
/// `#[cfg(test)]`, unlike its sibling decorators: the barrier is
/// `tokio::sync::Barrier`, and tokio is an OPTIONAL dependency of this crate.
/// It is present as a dev-dependency in the test profile, but not in a
/// `--features test-support` library build — which is what the `tests/`
/// integration crates and the wasm targets compile — so a non-`cfg(test)`
/// version of this would have to add tokio to that feature and push it into
/// builds that must not carry it.
#[cfg(test)]
#[derive(Clone)]
pub struct RendezvousDbOpContext {
    inner: TestContext,
    op: &'static str,
    collection: &'static str,
    /// Matching calls still to be held. Shared across clones, so a handler's
    /// `clone_arc` sees the same countdown.
    holds_left: Arc<std::sync::atomic::AtomicUsize>,
    barrier: Arc<tokio::sync::Barrier>,
    /// Matching calls THIS racer lets through before it takes a hold — see
    /// [`Self::passing_first`]. Shared by this racer's clones only.
    passes_left: Arc<std::sync::atomic::AtomicUsize>,
}

#[cfg(test)]
impl RendezvousDbOpContext {
    /// Wrap `inner`, holding the first `n` `"wafer-run/database"` calls whose
    /// `(msg.action(), request.collection)` is `(op, collection)`.
    pub fn new(inner: TestContext, op: &'static str, collection: &'static str, n: usize) -> Self {
        Self {
            inner,
            op,
            collection,
            holds_left: Arc::new(std::sync::atomic::AtomicUsize::new(n)),
            barrier: Arc::new(tokio::sync::Barrier::new(n)),
            passes_left: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    /// A racer on the same rendezvous that lets its own first `n` matching
    /// calls straight through, and is held on the one after.
    ///
    /// For a request that makes the same call more than once when the race
    /// is on a later one: the holds are shared, so without this the first
    /// matching calls — whichever racer makes them — use them up. Give each
    /// racer its own `passing_first`, and every racer is held at the same
    /// point in its own sequence.
    pub fn passing_first(&self, n: usize) -> Self {
        Self {
            passes_left: Arc::new(std::sync::atomic::AtomicUsize::new(n)),
            ..self.clone()
        }
    }

    /// Spend one of this racer's passes. `true` while any are left.
    fn take_pass(&self) -> bool {
        use std::sync::atomic::Ordering;
        self.passes_left
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
    }

    /// Claim one of the holds. `true` while any are left.
    fn take_hold(&self) -> bool {
        use std::sync::atomic::Ordering;
        self.holds_left
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
    }
}

#[cfg(test)]
#[async_trait::async_trait]
impl Context for RendezvousDbOpContext {
    fn check_resource_access(
        &self,
        resource: &str,
        resource_type: wafer_run::ResourceType,
        access: wafer_block::ResourceAccess,
    ) -> Result<(), WaferError> {
        self.inner
            .check_resource_access(resource, resource_type, access)
    }

    fn resource_access_admitted(
        &self,
        resource: &str,
        resource_type: wafer_run::ResourceType,
        access: wafer_block::ResourceAccess,
    ) -> bool {
        self.inner
            .resource_access_admitted(resource, resource_type, access)
    }

    async fn call_block(&self, name: &str, msg: Message, input: InputStream) -> OutputStream {
        if !(name == "wafer-run/database" && msg.action() == self.op) {
            return self.inner.call_block(name, msg, input).await;
        }
        let bytes = match input.collect_to_bytes().await {
            Ok(bytes) => bytes,
            Err(e) => return OutputStream::error(e),
        };
        let matches = wafer_block::codec::decode::<CollectionPeek>(&bytes)
            .map(|p| p.collection == self.collection)
            .unwrap_or(false);
        let out = self
            .inner
            .call_block(name, msg, InputStream::from_bytes(bytes))
            .await;
        if matches && !self.take_pass() && self.take_hold() {
            self.barrier.wait().await;
        }
        out
    }

    fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }

    fn registered_blocks(&self) -> &[BlockInfo] {
        self.inner.registered_blocks()
    }

    fn config_get(&self, key: &str) -> Option<&str> {
        self.inner.config_get(key)
    }

    fn clone_arc(&self) -> Arc<dyn Context> {
        Arc::new(self.clone())
    }
}

/// Wraps a [`TestContext`] and runs a one-shot `hook` right after the first
/// `"wafer-run/database"` call whose `(msg.action(), request.collection)` is
/// `(op, collection)` has answered, before that answer reaches the caller.
///
/// That is a write committed at one exact point in the caller's sequence of
/// reads — after the matched read, before whatever it reads next — which is
/// the interleaving a race between a request and a concurrent mutation needs
/// forced rather than hoped for. The hook runs once; later matching calls
/// pass straight through. [`Self::fired`] says whether the point was reached.
///
/// `#[cfg(test)]` for the reason [`RendezvousDbOpContext`] gives.
#[cfg(test)]
#[derive(Clone)]
pub struct AfterDbOpContext {
    inner: TestContext,
    op: &'static str,
    collection: &'static str,
    /// Taken by the first matching call; shared across clones, so a
    /// handler's `clone_arc` sees the same one-shot.
    hook: Arc<Mutex<Option<futures::future::BoxFuture<'static, ()>>>>,
}

#[cfg(test)]
impl AfterDbOpContext {
    /// Wrap `inner`, running `hook` after the first `(op, collection)` call.
    pub fn new(
        inner: TestContext,
        op: &'static str,
        collection: &'static str,
        hook: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> Self {
        Self {
            inner,
            op,
            collection,
            hook: Arc::new(Mutex::new(Some(Box::pin(hook)))),
        }
    }

    /// Whether a matching call has been made and the hook has run.
    pub fn fired(&self) -> bool {
        self.hook
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_none()
    }
}

#[cfg(test)]
#[async_trait::async_trait]
impl Context for AfterDbOpContext {
    fn check_resource_access(
        &self,
        resource: &str,
        resource_type: wafer_run::ResourceType,
        access: wafer_block::ResourceAccess,
    ) -> Result<(), WaferError> {
        self.inner
            .check_resource_access(resource, resource_type, access)
    }

    fn resource_access_admitted(
        &self,
        resource: &str,
        resource_type: wafer_run::ResourceType,
        access: wafer_block::ResourceAccess,
    ) -> bool {
        self.inner
            .resource_access_admitted(resource, resource_type, access)
    }

    async fn call_block(&self, name: &str, msg: Message, input: InputStream) -> OutputStream {
        if !(name == "wafer-run/database" && msg.action() == self.op) {
            return self.inner.call_block(name, msg, input).await;
        }
        let bytes = match input.collect_to_bytes().await {
            Ok(bytes) => bytes,
            Err(e) => return OutputStream::error(e),
        };
        let matches = wafer_block::codec::decode::<CollectionPeek>(&bytes)
            .map(|p| p.collection == self.collection)
            .unwrap_or(false);
        let out = self
            .inner
            .call_block(name, msg, InputStream::from_bytes(bytes))
            .await;
        if matches {
            let hook = self.hook.lock().unwrap_or_else(|e| e.into_inner()).take();
            if let Some(hook) = hook {
                hook.await;
            }
        }
        out
    }

    fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }

    fn registered_blocks(&self) -> &[BlockInfo] {
        self.inner.registered_blocks()
    }

    fn config_get(&self, key: &str) -> Option<&str> {
        self.inner.config_get(key)
    }

    fn clone_arc(&self) -> Arc<dyn Context> {
        Arc::new(self.clone())
    }
}

/// Wraps a [`Context`] and re-decodes every `database.aggregate` result the
/// way PostgreSQL and `wafer-block-postgres` would type it: an uncast `Sum`,
/// `SumWhere` or `Avg` comes back as a JSON float, everything else as the
/// in-memory SQLite database answered it.
///
/// This is not a hypothetical shape. PostgreSQL's `sum(bigint)` and `avg` are
/// `NUMERIC`, and `wafer-block-postgres` decodes `NUMERIC` through
/// `BigDecimal` into `f64`, so a money column declared `BIGINT` — which every
/// one of them is in the products block's `.postgres.sql` schema — sums to
/// `1000.0` rather than `1000` unless the request casts it. A `BIGINT` cast
/// makes it an integer again, and `COUNT(*)` / `CaseWhenSum` are `bigint`
/// already, so those pass through. (`SUM` of an `INTEGER` column is `bigint`
/// on PostgreSQL, so for one of those this wrapper is stricter than the real
/// server: it floats every uncast sum.)
///
/// The `Tests (postgres feature)` CI job cannot see this: the `postgres`
/// feature is a pure cfg flag and that job runs no server. The real-server
/// job (`PostgreSQL migrations`) pins the premise — that `sum` of a money
/// column really is `NUMERIC` there, and `bigint` once cast — in SQL; this
/// wrapper is what lets a Rust test drive the real analytics code against
/// the shape that produces.
#[derive(Clone)]
pub struct FloatAggregateContext {
    inner: Arc<dyn Context>,
}

impl FloatAggregateContext {
    /// Wrap `inner`. Every op but `database.aggregate` passes through
    /// untouched.
    pub fn new(inner: impl Context + 'static) -> Self {
        Self {
            inner: Arc::new(inner),
        }
    }

    /// The aliases of `req`'s aggregates that come back from PostgreSQL as a
    /// JSON float: every `Sum` and `SumWhere` not cast to `BIGINT` (`NUMERIC`
    /// there), and every `Avg` — the database handler rejects `BIGINT` on an
    /// `Avg`, so it is either uncast `NUMERIC` or cast to
    /// `DOUBLE PRECISION`, and both decode as a float.
    fn numeric_aliases(req: &wafer_block::wire::database::AggregateRequest) -> Vec<String> {
        use wafer_block::wire::database::AggregateColumnDef;
        use wafer_sql_utils::aggregate::CastType;
        let uncast = |cast_as: &Option<String>| {
            cast_as.as_deref().and_then(CastType::parse) != Some(CastType::BigInt)
        };
        req.aggregates
            .iter()
            .filter_map(|column| match column {
                AggregateColumnDef::Sum { alias, cast_as, .. }
                | AggregateColumnDef::SumWhere { alias, cast_as, .. }
                | AggregateColumnDef::Avg { alias, cast_as, .. } => {
                    uncast(cast_as).then(|| alias.clone())
                }
                AggregateColumnDef::Count { .. }
                | AggregateColumnDef::Max { .. }
                | AggregateColumnDef::CaseWhenSum { .. } => None,
            })
            .collect()
    }
}

#[async_trait::async_trait]
impl Context for FloatAggregateContext {
    fn check_resource_access(
        &self,
        resource: &str,
        resource_type: wafer_run::ResourceType,
        access: wafer_block::ResourceAccess,
    ) -> Result<(), WaferError> {
        self.inner
            .check_resource_access(resource, resource_type, access)
    }

    fn resource_access_admitted(
        &self,
        resource: &str,
        resource_type: wafer_run::ResourceType,
        access: wafer_block::ResourceAccess,
    ) -> bool {
        self.inner
            .resource_access_admitted(resource, resource_type, access)
    }

    async fn call_block(&self, name: &str, msg: Message, input: InputStream) -> OutputStream {
        let rewrites = name == "wafer-run/database" && msg.action() == "database.aggregate";
        if !rewrites {
            return self.inner.call_block(name, msg, input).await;
        }
        let request_bytes = match input.collect_to_bytes().await {
            Ok(bytes) => bytes,
            Err(e) => return OutputStream::error(e),
        };
        let request: wafer_block::wire::database::AggregateRequest =
            match wafer_block::codec::decode(&request_bytes) {
                Ok(request) => request,
                Err(e) => return OutputStream::error(e.invalid_argument()),
            };
        let numeric = Self::numeric_aliases(&request);
        let out = self
            .inner
            .call_block(name, msg, InputStream::from_bytes(request_bytes))
            .await;
        let buf = match out.collect_buffered().await {
            Ok(buf) => buf,
            Err(TerminalNotResponse::Error(e)) => return OutputStream::error(e),
            Err(other) => {
                return OutputStream::error(WaferError::new(
                    ErrorCode::Internal,
                    format!("float-aggregate wrapper saw a non-response terminal: {other:?}"),
                ))
            }
        };
        let mut records: Vec<wafer_block::wire::database::Record> =
            match wafer_block::codec::decode(&buf.body) {
                Ok(records) => records,
                Err(e) => return OutputStream::error(e.internal()),
            };
        for record in &mut records {
            for alias in &numeric {
                if let Some(value) = record.data.get_mut(alias) {
                    if let Some(whole) = value.as_i64() {
                        *value = serde_json::json!(whole as f64);
                    }
                }
            }
        }
        match wafer_block::codec::encode(&records) {
            Ok(bytes) => OutputStream::respond(bytes),
            Err(e) => OutputStream::error(e),
        }
    }

    fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }

    fn registered_blocks(&self) -> &[BlockInfo] {
        self.inner.registered_blocks()
    }

    fn config_get(&self, key: &str) -> Option<&str> {
        self.inner.config_get(key)
    }

    fn clone_arc(&self) -> Arc<dyn Context> {
        Arc::new(self.clone())
    }
}

/// Wraps a [`TestContext`] and strips the COLUMNS off every record a
/// `database.create` or `database.update` answers with, leaving the
/// `{id, data}` envelope's `data` empty. The write still lands in the real
/// in-memory SQLite underneath — only the echo is thinned.
///
/// This is the backend a repository function has to survive and cannot
/// simulate any other way: `DatabaseService::create`/`update` are free to
/// return the row they stored or a bare acknowledgement, and a repo that
/// DECODES the echo and reports a decode failure as an error is telling its
/// caller the write failed when the change is already committed.
/// `platform_state::variables` is where that mattered, differently on each
/// side: `insert`'s error was classified by re-reading the key, so the row the
/// request had just created came back as "that key is already taken", and
/// `upsert_by_key`'s error returned from `admin::ops::update_variable` before
/// its `audit_log` call, so an edit that landed went unrecorded.
///
/// The real in-memory SQLite echoes the row, which is why no existing test
/// reached those branches.
/// It wraps any [`Context`], not just a [`TestContext`], so it composes with
/// [`FailingDbOpContext`] — a write that is REFUSED must keep the refusal's own
/// code on its way back out through this wrapper, and the only way to say that
/// in a test is to put a refusing context underneath it.
#[derive(Clone)]
pub struct EcholessWriteContext {
    inner: Arc<dyn Context>,
    keep_columns: Vec<String>,
    keep_id: bool,
}

impl EcholessWriteContext {
    /// Wrap `inner`. Every op but `database.create`/`database.update` passes
    /// through untouched; those keep their `id` and lose every column.
    pub fn new(inner: impl Context + 'static) -> Self {
        Self {
            inner: Arc::new(inner),
            keep_columns: Vec::new(),
            keep_id: true,
        }
    }

    /// Echo the named columns and drop the rest — the PARTIAL echo, which is
    /// the case a repo is most likely to get wrong: a record carrying `key` and
    /// little else still decodes, so a repo that only falls back when decoding
    /// FAILS hands its caller a row with the other columns silently defaulted.
    pub fn keeping_columns(mut self, keep: &[&str]) -> Self {
        self.keep_columns = keep.iter().map(|c| (*c).to_string()).collect();
        self
    }

    /// Answer with an empty `id` as well. A backend that acknowledges a write
    /// without naming the row is entitled to; a repo that publishes that empty
    /// id as the row's identity is not.
    pub fn without_the_id(mut self) -> Self {
        self.keep_id = false;
        self
    }
}

#[async_trait::async_trait]
impl Context for EcholessWriteContext {
    fn check_resource_access(
        &self,
        resource: &str,
        resource_type: wafer_run::ResourceType,
        access: wafer_block::ResourceAccess,
    ) -> Result<(), WaferError> {
        self.inner
            .check_resource_access(resource, resource_type, access)
    }

    fn resource_access_admitted(
        &self,
        resource: &str,
        resource_type: wafer_run::ResourceType,
        access: wafer_block::ResourceAccess,
    ) -> bool {
        self.inner
            .resource_access_admitted(resource, resource_type, access)
    }

    async fn call_block(&self, name: &str, msg: Message, input: InputStream) -> OutputStream {
        let thins_the_echo = name == "wafer-run/database"
            && matches!(msg.action(), "database.create" | "database.update");
        if !thins_the_echo {
            return self.inner.call_block(name, msg, input).await;
        }
        let out = self.inner.call_block(name, msg, input).await;
        let buf = match out.collect_buffered().await {
            Ok(buf) => buf,
            // The write itself failed, and that answer is not this wrapper's
            // to rewrite: substituting an `Internal` here would make a test
            // that paired this wrapper with a denied context assert 403 and
            // silently receive 500. The inner error is forwarded as it stands.
            Err(TerminalNotResponse::Error(e)) => return OutputStream::error(e),
            Err(other) => {
                return OutputStream::error(WaferError::new(
                    ErrorCode::Internal,
                    format!("echoless-write wrapper saw a non-response terminal: {other:?}"),
                ))
            }
        };
        // The service wire format is the codec's, not JSON — decode and
        // re-encode through it so the client sees a well-formed record that
        // simply carries no columns.
        let mut record: wafer_block::wire::database::Record =
            match wafer_block::codec::decode(&buf.body) {
                Ok(record) => record,
                Err(e) => return OutputStream::error(e.internal()),
            };
        record
            .data
            .retain(|column, _| self.keep_columns.iter().any(|k| k == column));
        if !self.keep_id {
            record.id = String::new();
        }
        match wafer_block::codec::encode(&record) {
            Ok(bytes) => OutputStream::respond(bytes),
            Err(e) => OutputStream::error(e),
        }
    }

    fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }

    fn registered_blocks(&self) -> &[BlockInfo] {
        self.inner.registered_blocks()
    }

    fn config_get(&self, key: &str) -> Option<&str> {
        self.inner.config_get(key)
    }

    fn clone_arc(&self) -> Arc<dyn Context> {
        Arc::new(self.clone())
    }
}

/// The JWT secret the registered `wafer-run/crypto` block signs with.
///
/// Distinct from [`TEST_JWT_SECRET`], which is what the hand-rolled
/// [`access_token_for`] signs with; this one is long enough for
/// `Argon2JwtCryptoService`'s minimum.
const CRYPTO_BLOCK_JWT_SECRET: &str = "test-jwt-secret-padded-to-min-32-bytes-aaaa";

impl TestContext {
    /// [`TestContext::with_auth`] plus a real `wafer-run/crypto` block, so
    /// handlers that mint or verify JWTs (login, signup, refresh) run end to
    /// end against a fixed test secret.
    pub async fn with_auth_and_crypto() -> Self {
        Self::with_auth_and_crypto_service(Arc::new(real_crypto_service())).await
    }

    /// [`TestContext::with_auth_and_crypto`] over [`PinnedMintCrypto`]: every
    /// token the test mints is signed as if two mints of one claim set were
    /// the same mint. Use it to decide what a handler does when two tokens
    /// would otherwise be indistinguishable.
    pub async fn with_auth_and_pinned_mint_crypto() -> Self {
        Self::with_auth_and_crypto_service(Arc::new(PinnedMintCrypto::new())).await
    }

    /// The fixture runs as `impresspress/auth-ui`, the block every session
    /// token is minted in: the crypto block signs under its caller's derived
    /// key and refuses a call with none.
    pub(crate) async fn with_auth_and_crypto_service(svc: Arc<dyn CryptoService>) -> Self {
        let mut ctx = Self::with_auth().await;
        let crypto_block: Arc<dyn wafer_run::Block> =
            Arc::new(wafer_core::service_blocks::crypto::CryptoBlock::new(svc));
        ctx.register_block("wafer-run/crypto", crypto_block);
        ctx.running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID)
    }
}

/// The config block behind [`TestContext::refuse_config_writes`] and
/// [`TestContext::refuse_config_reads`]: every `op` (on `key`, when one is
/// named) answers `error`, everything else reaches the wrapped block.
struct RefusingConfigOp {
    inner: Arc<dyn Block>,
    op: &'static str,
    key: Option<String>,
    error: WaferError,
}

#[wafer_block::wafer_async_trait]
impl Block for RefusingConfigOp {
    fn info(&self) -> BlockInfo {
        self.inner.info()
    }

    async fn handle(&self, ctx: &dyn Context, msg: Message, input: InputStream) -> OutputStream {
        if msg.kind != self.op {
            return self.inner.handle(ctx, msg, input).await;
        }
        let Some(key) = &self.key else {
            return OutputStream::error(self.error.clone());
        };
        let body = match input.collect_to_bytes().await {
            Ok(bytes) => bytes,
            Err(e) => return OutputStream::error(e),
        };
        let requested = wafer_block::codec::decode::<wafer_block::wire::config::GetRequest>(&body)
            .map(|req| req.key)
            .unwrap_or_default();
        if &requested == key {
            return OutputStream::error(self.error.clone());
        }
        self.inner
            .handle(ctx, msg, InputStream::from_bytes(body))
            .await
    }
}

pub(crate) fn real_crypto_service() -> wafer_block_crypto::service::Argon2JwtCryptoService {
    wafer_block_crypto::service::Argon2JwtCryptoService::new(CRYPTO_BLOCK_JWT_SECRET.to_string())
        .expect("test secret is long enough")
}

/// The production crypto service with its clock pinned.
///
/// The signer encodes claims canonically (keys sorted at every depth), so two
/// mints of an identical claim set are byte-identical whenever they land in
/// the same second: `iat`/`exp` are whole seconds. That is ordinary in
/// production (a client that refreshes right after logging in) but only
/// occasional in a test, which cannot wait for the scheduler to put two mints
/// inside one second. This service makes it certain instead: every token is
/// stamped with the instant this service was built. A mint path that needs
/// its tokens to differ has to put something that differs in the claims, and
/// a test over this service is what proves it does.
///
/// It re-encodes what the real service signed rather than signing from
/// scratch — same header, same key derivation, same claims in the same
/// canonical order, each token keeping its own lifetime — so it stays a pin
/// on the clock, not a second implementation of JWT signing.
pub struct PinnedMintCrypto {
    inner: wafer_block_crypto::service::Argon2JwtCryptoService,
    /// The `iat` every token minted through this service carries.
    minted_at: i64,
}

impl Default for PinnedMintCrypto {
    fn default() -> Self {
        Self::new()
    }
}

impl PinnedMintCrypto {
    pub fn new() -> Self {
        Self {
            inner: real_crypto_service(),
            minted_at: chrono::Utc::now().timestamp(),
        }
    }

    /// Re-encode `token`'s payload with this service's pinned `iat`, then
    /// re-sign it with `key`. The token's lifetime (`exp - iat`) is carried
    /// over from what the inner service stamped. The payload is already
    /// canonical and a `BTreeMap` keeps it in that order, so the re-encoding
    /// changes nothing but the two timestamps.
    fn pin(&self, token: String, key: &[u8]) -> Result<String, CryptoError> {
        use wafer_block_crypto::primitives::{b64url_decode, b64url_encode, hmac_sha256};

        let mut parts = token.split('.');
        let (Some(header), Some(payload), Some(_signature), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(CryptoError::SignError(format!(
                "the crypto service did not return a compact JWT: {token}"
            )));
        };
        let mut claims: BTreeMap<String, serde_json::Value> =
            serde_json::from_slice(&b64url_decode(payload)?).map_err(|e| {
                CryptoError::SignError(format!("JWT payload is not an object: {e}"))
            })?;

        let iat = claims
            .get("iat")
            .and_then(serde_json::Value::as_i64)
            .ok_or_else(|| CryptoError::SignError("JWT payload has no iat".to_string()))?;
        let exp = claims
            .get("exp")
            .and_then(serde_json::Value::as_i64)
            .ok_or_else(|| CryptoError::SignError("JWT payload has no exp".to_string()))?;
        claims.insert("iat".to_string(), serde_json::json!(self.minted_at));
        claims.insert(
            "exp".to_string(),
            serde_json::json!(self.minted_at + exp - iat),
        );

        let canonical = serde_json::to_string(&claims)
            .map_err(|e| CryptoError::SignError(format!("re-encoding the JWT payload: {e}")))?;
        let signing_input = format!("{header}.{}", b64url_encode(canonical.as_bytes()));
        let signature = hmac_sha256(key, signing_input.as_bytes());
        Ok(format!("{signing_input}.{}", b64url_encode(&signature)))
    }
}

#[wafer_block::wafer_async_trait]
impl CryptoService for PinnedMintCrypto {
    async fn hash(&self, password: &str) -> Result<String, CryptoError> {
        self.inner.hash(password).await
    }

    async fn compare_hash(&self, password: &str, hash: &str) -> Result<(), CryptoError> {
        self.inner.compare_hash(password, hash).await
    }

    async fn sign_for(
        &self,
        block_id: &str,
        claims: BTreeMap<String, serde_json::Value>,
        expiry: std::time::Duration,
    ) -> Result<String, CryptoError> {
        let signed = self.inner.sign_for(block_id, claims, expiry).await?;
        let derived = wafer_block_crypto::primitives::derive_block_key(
            CRYPTO_BLOCK_JWT_SECRET.as_bytes(),
            block_id,
        );
        self.pin(signed, derived.as_bytes())
    }

    async fn verify_for(
        &self,
        block_id: &str,
        token: &str,
    ) -> Result<BTreeMap<String, serde_json::Value>, CryptoError> {
        self.inner.verify_for(block_id, token).await
    }

    async fn random_bytes(&self, n: usize) -> Result<Vec<u8>, CryptoError> {
        self.inner.random_bytes(n).await
    }
}

/// How [`FaultyHasher`] fails a password operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HasherFault {
    /// The backend doing the hashing cannot be reached, as a password-hasher
    /// Worker whose Durable Object does not answer reports it:
    /// `CryptoError::Unavailable`.
    Unreachable,
    /// The hashing itself failed, a fault retrying does not fix:
    /// `CryptoError::HashError`.
    Broken,
}

impl HasherFault {
    fn error(self) -> CryptoError {
        match self {
            Self::Unreachable => {
                CryptoError::Unavailable("password hasher unavailable: fake outage".to_string())
            }
            Self::Broken => CryptoError::HashError("fake hashing fault".to_string()),
        }
    }
}

/// The production crypto service, except that `hash` and `compare_hash` fail
/// with a [`HasherFault`] once one is set for them. Registered behind the
/// real `wafer-run/crypto` block
/// ([`TestContext::with_auth_and_faulty_hasher`]), so the error reaches the
/// handler through the block's own `CryptoError` → `ErrorCode` mapping, as a
/// failing Cloudflare password hasher's does.
pub struct FaultyHasher {
    inner: wafer_block_crypto::service::Argon2JwtCryptoService,
    hash_fault: std::sync::Mutex<Option<HasherFault>>,
    compare_fault: std::sync::Mutex<Option<HasherFault>>,
}

impl FaultyHasher {
    /// Make every later `hash` fail with `fault` (`None` heals it).
    pub fn fail_hash(&self, fault: Option<HasherFault>) {
        *self.hash_fault.lock().unwrap_or_else(|e| e.into_inner()) = fault;
    }

    /// Make every later `compare_hash` fail with `fault` (`None` heals it).
    pub fn fail_compare(&self, fault: Option<HasherFault>) {
        *self.compare_fault.lock().unwrap_or_else(|e| e.into_inner()) = fault;
    }
}

#[wafer_block::wafer_async_trait]
impl CryptoService for FaultyHasher {
    async fn hash(&self, password: &str) -> Result<String, CryptoError> {
        let fault = *self.hash_fault.lock().unwrap_or_else(|e| e.into_inner());
        match fault {
            Some(fault) => Err(fault.error()),
            None => self.inner.hash(password).await,
        }
    }

    async fn compare_hash(&self, password: &str, hash: &str) -> Result<(), CryptoError> {
        let fault = *self.compare_fault.lock().unwrap_or_else(|e| e.into_inner());
        match fault {
            Some(fault) => Err(fault.error()),
            None => self.inner.compare_hash(password, hash).await,
        }
    }

    async fn sign_for(
        &self,
        block_id: &str,
        claims: BTreeMap<String, serde_json::Value>,
        expiry: std::time::Duration,
    ) -> Result<String, CryptoError> {
        self.inner.sign_for(block_id, claims, expiry).await
    }

    async fn verify_for(
        &self,
        block_id: &str,
        token: &str,
    ) -> Result<BTreeMap<String, serde_json::Value>, CryptoError> {
        self.inner.verify_for(block_id, token).await
    }

    async fn random_bytes(&self, n: usize) -> Result<Vec<u8>, CryptoError> {
        self.inner.random_bytes(n).await
    }
}

impl TestContext {
    /// [`TestContext::with_auth_and_crypto`] over a [`FaultyHasher`], which
    /// works until the test sets a fault on it.
    pub async fn with_auth_and_faulty_hasher() -> (Self, Arc<FaultyHasher>) {
        let hasher = Arc::new(FaultyHasher {
            inner: real_crypto_service(),
            hash_fault: std::sync::Mutex::new(None),
            compare_fault: std::sync::Mutex::new(None),
        });
        let ctx = Self::with_auth_and_crypto_service(hasher.clone()).await;
        (ctx, hasher)
    }
}

/// Seed an account through the production `repo::users::insert`.
///
/// Every auth test that needs a user wants the same row: an unverified
/// address with no verification token and no avatar, differing only in the
/// email, the display name and the role. Spelled as a `NewUser` literal that
/// is seventeen copies of the same six fields across `tests/auth/`, and
/// seventeen places to edit when the struct gains a field.
///
/// The insert is the real one, so a test still exercises the column set and
/// the defaults the repo writes; only the fields nobody is asserting on are
/// hidden. Anything a case does assert on it sets:
///
/// ```ignore
/// let user = seed_user("p@e.com").display_name("P").role("admin").insert(&ctx).await;
/// ```
///
/// The two fields that are *not* here — `email_verified` and
/// `verification_token_hash` — are `false`/`None` at every current call site.
/// A case that needs either is asserting on verification itself and should
/// say so with `users::insert` directly, rather than reaching for a builder
/// default it then has to remember to override.
pub struct SeedUser<'a> {
    email: &'a str,
    display_name: &'a str,
    avatar_url: Option<&'a str>,
    role: &'a str,
}

/// A user to seed: `email`, a display name that defaults to the email, no
/// avatar, and the `user` role. See [`SeedUser`].
pub fn seed_user(email: &str) -> SeedUser<'_> {
    SeedUser {
        email,
        display_name: email,
        avatar_url: None,
        role: "user",
    }
}

impl<'a> SeedUser<'a> {
    /// The profile name, when the case asserts on it or on its absence.
    pub fn display_name(mut self, name: &'a str) -> Self {
        self.display_name = name;
        self
    }

    /// The avatar URL, when the case asserts it round-trips.
    pub fn avatar_url(mut self, url: &'a str) -> Self {
        self.avatar_url = Some(url);
        self
    }

    /// The inline `users.role` column — `"admin"` is what makes an account an
    /// admin, not a `user_roles` row (see `repo::users::NewUser::role`).
    pub fn role(mut self, role: &'a str) -> Self {
        self.role = role;
        self
    }

    /// Insert the row, panicking on failure the way a fixture should.
    pub async fn insert(self, ctx: &dyn Context) -> crate::blocks::auth::repo::users::UserRow {
        crate::blocks::auth::repo::users::insert(
            ctx,
            crate::blocks::auth::repo::users::NewUser {
                email: self.email.to_string(),
                display_name: self.display_name.to_string(),
                avatar_url: self.avatar_url.map(str::to_string),
                role: self.role.to_string(),
                email_verified: false,
                verification_token_hash: None,
            },
        )
        .await
        .unwrap_or_else(|e| panic!("seed user {}: {e:?}", self.email))
    }
}

/// Every interface spec a block in this workspace can declare: wafer-run's
/// well-known ones plus impresspress's own, which is the config block's
/// ([`crate::blocks::config::CONFIG_INTERFACE`]). `Wafer` holds the same two
/// sets — `wafer_block::interfaces::all()` at construction, plus whatever
/// `register_interface` adds, which in this repo is that one spec — keyed by
/// interface name, as `check_action_interface` reads them.
fn interface_specs() -> &'static HashMap<String, wafer_block::InterfaceSpec> {
    static SPECS: std::sync::OnceLock<HashMap<String, wafer_block::InterfaceSpec>> =
        std::sync::OnceLock::new();
    SPECS.get_or_init(|| {
        wafer_block::interfaces::all()
            .into_iter()
            .chain([crate::blocks::config::interface_spec()])
            .map(|spec| (spec.name.clone(), spec))
            .collect()
    })
}

/// What [`wafer_run::runtime::call_gates::admit_call`] reads about a
/// [`TestContext`] frame — the answers a `RuntimeContext` gives from its own
/// fields, given here from the fixture's.
impl wafer_run::runtime::call_gates::CallFrame for TestContext {
    fn call_depth(&self) -> u32 {
        self.call_depth
    }

    /// The ceiling every context the runtime builds starts with.
    fn max_call_depth(&self) -> u32 {
        wafer_run::runtime::call_gates::DEFAULT_MAX_CALL_DEPTH
    }

    /// A fixture sets no deadline; a test models a timed-out dispatch with
    /// [`TestContext::cancel`].
    fn deadline(&self) -> Option<wafer_run::platform::Instant> {
        None
    }

    fn cancellation(&self) -> &std::sync::atomic::AtomicBool {
        &self.cancelled
    }

    /// The service short names (`db`, `storage`) the builder installs as
    /// aliases, from the same list ([`crate::builder::SERVICE_ALIASES`]).
    fn canonicalize<'a>(&'a self, name: &'a str) -> &'a str {
        crate::builder::SERVICE_ALIASES
            .iter()
            .find(|(alias, _)| *alias == name)
            .map_or(name, |(_, target)| *target)
    }

    fn caller_requires(&self) -> Option<&[String]> {
        self.caller_requires.as_deref()
    }

    /// The capabilities of the block whose code runs on this frame, read off
    /// the block the fixture registered under that name, as the runtime reads
    /// them off the block it runs as. The fixture's own frame and an unframed
    /// one run no block's code, so no capability limits them.
    fn caller_capabilities(&self) -> Option<wafer_run::BlockCapabilities> {
        match &self.frame {
            dispatch::Frame::Block(name) => self
                .registered(name)
                .and_then(|block| block.block_capabilities()),
            dispatch::Frame::Unframed | dispatch::Frame::Fixture => None,
        }
    }

    fn lookup(&self, name: &str) -> Option<Arc<dyn Block>> {
        self.registered(name)
    }

    fn interface_specs(&self) -> &HashMap<String, wafer_block::InterfaceSpec> {
        interface_specs()
    }

    /// The runtime logs a warning and admits the call; so does the fixture,
    /// without the log.
    fn unknown_interface(&self, _resolved: &str, _interface: &str) {}
}

/// A signed-in session, as [`TestContext::sign_in`] took it off a real login
/// response. Attach it to a wire message with [`Self::bearer`] or
/// [`Self::cookie`] and send that through [`TestContext::request`].
#[derive(Clone, Debug)]
pub struct Session {
    /// The signed-in user's id, as the login response named it.
    pub user_id: String,
    /// The access token from the login response body.
    pub access_token: String,
    /// The `auth_token` cookie value from the login response's `Set-Cookie`.
    pub cookie: String,
}

impl Session {
    /// `msg` with this session's access token in an `Authorization: Bearer`
    /// header — how an API client presents it, and a credential the CSRF
    /// origin policy exempts.
    pub fn bearer(&self, mut msg: Message) -> Message {
        msg.set_meta(
            "http.header.authorization",
            format!("Bearer {}", self.access_token),
        );
        msg
    }

    /// `msg` with this session's `auth_token` cookie — how a browser presents
    /// it, and what the CSRF origin policy guards.
    pub fn cookie(&self, mut msg: Message) -> Message {
        msg.set_meta("http.header.cookie", format!("auth_token={}", self.cookie));
        msg
    }

    /// Create an API key through `POST /b/auth/api/api-keys` over
    /// [`TestContext::request`], as this session, and return the raw key the
    /// response reveals once. Present it with [`api_key_header`].
    pub async fn create_api_key(&self, ctx: &TestContext, name: &str) -> String {
        let out = ctx
            .request_json(
                self.bearer(anon_msg("create", "/b/auth/api/api-keys")),
                &serde_json::json!({ "name": name }),
            )
            .await;
        let parts = wafer_block::http_codec::collect_http_response(out).await;
        let body: serde_json::Value = serde_json::from_slice(&parts.body).unwrap_or_default();
        assert_eq!(parts.status, 200, "API key creation failed: {body}");
        body["key"]
            .as_str()
            .expect("the create response reveals the key")
            .to_string()
    }
}

/// `msg` with `key` in an `Authorization: ApiKey` header — the form
/// `pipeline::handle_request` resolves through the API-key table.
pub fn api_key_header(mut msg: Message, key: &str) -> Message {
    msg.set_meta("http.header.authorization", format!("ApiKey {key}"));
    msg
}

/// Build an anonymous request `Message`. No `auth.user_id` meta set.
pub fn anon_msg(action: &str, path: &str) -> Message {
    let mut m = Message::new("http.request");
    m.set_meta("req.action", action);
    m.set_meta("req.resource", path);
    m
}

/// Build an authenticated request `Message` for `user_id`. No admin role.
pub fn auth_msg(action: &str, path: &str, user_id: &str) -> Message {
    let mut m = anon_msg(action, path);
    m.set_meta("auth.user_id", user_id);
    m
}

/// A config value no process environment can be holding.
///
/// `EnvConfigService::get` falls through to `std::env::var` when it has no
/// override, so a test asserting on a fixed literal like `#ff0000` PASSES for
/// anyone whose shell exports that key. On a reproduction test — one whose
/// whole job is to fail while the defect is live — that is a silent false
/// green, demonstrated with
/// `WAFER_RUN_SHARED__PRIMARY_COLOR='#ff0000' cargo test ...`.
pub fn unique_config_value() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    format!(
        "#repro-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after the epoch")
            .as_nanos()
    )
}

/// Build an admin request `Message` (user_id `"admin_1"`, role `admin`).
pub fn admin_msg(action: &str, path: &str) -> Message {
    let mut m = auth_msg(action, path, "admin_1");
    m.set_meta("auth.user_roles", "admin");
    m
}

/// The admin audit-log rows whose `action` is exactly `action`, in write
/// order.
///
/// Lives here rather than in the admin block's test module because
/// `blocks::admin::logs::audit_log` is the ONE audit writer:
/// userportal's portal buttons and `ui::settings_form`'s five settings pages
/// write their rows into the same table under their own WRAP identity, so a
/// test in any block asserts against it the same way.
///
/// Read from the fixture's own frame ([`TestContext::fixture`]), whatever
/// block `ctx` runs as, so a missing READ grant on the block under test
/// cannot be mistaken for a missing row.
pub async fn audit_rows(
    ctx: &TestContext,
    action: &str,
) -> Vec<wafer_core::clients::database::Record> {
    crate::db_read::list_every(
        &ctx.fixture(),
        crate::blocks::admin::AUDIT_LOGS_TABLE,
        vec![wafer_block::db::Filter {
            field: "action".to_string(),
            operator: wafer_block::db::FilterOp::Equal,
            value: serde_json::Value::String(action.to_string()),
        }],
    )
    .await
    .expect("read the audit log")
}

/// How many admin audit-log rows carry `action`. See [`audit_rows`].
pub async fn audit_count(ctx: &TestContext, action: &str) -> usize {
    audit_rows(ctx, action).await.len()
}

/// Drain an `OutputStream` to a `BufferedResponse`. Panics if the stream
/// terminates with anything other than `Complete` or `Halt`.
///
/// `Halt` is a legitimate success-shaped terminal (used e.g. by CORS
/// preflight to short-circuit with a 204 + headers), so tests treat it
/// the same as `Complete` — the body+meta are returned for assertion.
///
/// Tests should not see errors from handlers under test unless they're
/// explicitly asserting on error paths — use `output_is_error` for that.
pub async fn collect_or_panic(out: OutputStream) -> BufferedResponse {
    match out.collect_buffered().await {
        Ok(buf) => buf,
        Err(TerminalNotResponse::Halt(buf)) => buf,
        Err(TerminalNotResponse::Error(e)) => {
            panic!("handler returned error: {} ({:?})", e.message, e.code)
        }
        Err(TerminalNotResponse::Drop { .. }) => panic!("handler dropped the request"),
        Err(TerminalNotResponse::Continue(_)) => panic!("handler returned Continue"),
        Err(TerminalNotResponse::Malformed) => panic!("handler returned malformed stream"),
    }
}

/// Read the HTTP status from an `OutputStream`. Defaults to 200 if the
/// handler didn't set a `resp.status` meta entry.
pub async fn output_status(out: OutputStream) -> u16 {
    let buf = collect_or_panic(out).await;
    buf.meta
        .iter()
        .find(|m| m.key == "resp.status")
        .and_then(|m| m.value.parse::<u16>().ok())
        .unwrap_or(200)
}

/// The HTTP status an adapter would send for `out`, **including** for the
/// error terminals the router and the `err_*` helpers produce.
///
/// [`output_status`] deliberately panics on an error terminal, because a
/// handler under test erroring is normally a bug. A test asserting the
/// router's access gate is the opposite case: a 403 there arrives as
/// `TerminalNotResponse::Error(PermissionDenied)`, never as `resp.status`
/// meta. The mapping is `wafer_block::http_codec`'s, the same one the real
/// adapters use, so this reports the status the caller would actually see.
pub async fn output_http_status(out: OutputStream) -> u16 {
    match out.collect_buffered().await {
        Ok(buf) => wafer_block::http_codec::resolve_status(&buf.meta, 200),
        Err(TerminalNotResponse::Halt(buf)) => {
            wafer_block::http_codec::resolve_status(&buf.meta, 200)
        }
        Err(TerminalNotResponse::Error(e)) => wafer_block::http_codec::resolve_error_status(&e),
        Err(other) => panic!("unexpected terminal: {other:?}"),
    }
}

/// Read a named response header (e.g. `"Location"` for redirects).
/// The lookup is case-sensitive — pass the exact name handlers used in
/// `set_header(name, _)`.
pub async fn output_header(out: OutputStream, name: &str) -> Option<String> {
    let key = format!("resp.header.{name}");
    let buf = collect_or_panic(out).await;
    buf.meta
        .iter()
        .find(|m| m.key == key)
        .map(|m| m.value.clone())
}

/// A named response header as the HTTP boundary would send it, **including**
/// for error terminals.
///
/// The error-terminal sibling of [`output_header`], for the same reason
/// [`output_http_status`] is [`output_status`]'s: a refusal carries its
/// headers in `WaferError::meta`, which `collect_buffered` surfaces as an
/// `Err` rather than a `BufferedResponse`. Reads through
/// `wafer_block::http_codec`, so the answer is the header the caller actually
/// receives. Header names are matched case-insensitively, as HTTP does.
pub async fn output_http_header(out: OutputStream, name: &str) -> Option<String> {
    let headers = wafer_block::http_codec::collect_http_response(out)
        .await
        .headers;
    headers
        .into_iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value)
}

/// Read the body as a UTF-8 string. Panics if the body is not valid UTF-8.
pub async fn output_html(out: OutputStream) -> String {
    let buf = collect_or_panic(out).await;
    String::from_utf8(buf.body).expect("body was not valid UTF-8")
}

/// Read the body as raw bytes.
pub async fn output_body(out: OutputStream) -> Vec<u8> {
    collect_or_panic(out).await.body
}

/// Read the body as JSON. Returns `Value::Null` if the body fails to parse.
pub async fn output_json(out: OutputStream) -> serde_json::Value {
    let buf = collect_or_panic(out).await;
    serde_json::from_slice(&buf.body).unwrap_or(serde_json::Value::Null)
}

/// The JSON body an adapter would SEND for `out`, error terminals included.
///
/// [`output_json`] reads a success body, and panics on an error terminal
/// because a handler under test erroring is normally a bug. An error terminal
/// carries no body at all until `wafer_block::http_codec` renders one — the
/// `{"error": "<Code>", "message": "<text>"}` envelope — so a test that wants
/// to assert on what a *browser* receives from a refusal has to run that
/// render. This runs it, through the same `collect_http_response` the real
/// adapters use, so the assertion cannot drift from the bytes on the wire.
///
/// The pairing is [`output_http_status`]: status and body from the one
/// rendering, rather than from a second description of it.
pub async fn output_http_json(out: OutputStream) -> serde_json::Value {
    let parts = wafer_block::http_codec::collect_http_response(out).await;
    serde_json::from_slice(&parts.body).unwrap_or(serde_json::Value::Null)
}

/// True if the OutputStream terminated with an error matching `code`.
/// The code string should match the ErrorCode debug format (e.g., "NotFound", "Internal").
pub async fn output_is_error(out: OutputStream, code: &str) -> bool {
    matches!(
        out.collect_buffered().await,
        Err(TerminalNotResponse::Error(e)) if format!("{:?}", e.code) == code
    )
}

/// `BlockInfo` for every Worker-shipping block, fetched from the real block
/// structs (not hand-rolled fixtures) so `discovery_json`/`openapi_document`
/// exercise the actual declarations shipped in `blocks/*/mod.rs`.
///
/// This is the block list that backs the generated `/openapi.json` document
/// in tests. A block absent from this list never appears in the document at
/// all — regardless of how correct its own schema declarations are — so the
/// per-block openapi snapshot gate (`tests/openapi_snapshot.rs`) depends on
/// this staying in sync with every block that carries schema-bearing
/// endpoints (or is expected to soon).
#[cfg(all(
    feature = "block-files",
    feature = "block-messages",
    feature = "block-products",
    feature = "block-tickets",
    feature = "block-llm",
    feature = "block-vector"
))]
pub fn real_block_infos() -> Vec<BlockInfo> {
    #[cfg_attr(
        not(any(feature = "block-legalpages", feature = "block-dev")),
        expect(unused_mut, reason = "only the two feature-gated pushes below need it")
    )]
    let mut infos = vec![
        crate::blocks::auth_ui::AuthUiBlock::new().info(),
        crate::blocks::files::FilesBlock::new().info(),
        crate::blocks::products::ProductsBlock::new().info(),
        crate::blocks::admin::AdminBlock::new().info(),
        crate::blocks::messages::MessagesBlock::new().info(),
        crate::blocks::tickets::TicketsBlock::new().info(),
        // `info()` is declarative; the provider-admin handle it is built
        // with never runs here, so the no-op one suffices (same as
        // `blocks::feature_block_infos`).
        crate::blocks::llm::LlmBlock::new(Arc::new(
            crate::blocks::llm::provider_admin::NoopProviderAdmin,
        ))
        .info(),
        crate::blocks::vector::VectorBlock::new().info(),
    ];

    // Legalpages ships under its own feature, which the `cfg` on this
    // function does not require, so it joins only when compiled in. It has
    // to join at all because `tests/openapi_snapshot.rs` now guards it: a
    // block absent from this list contributes no path to the generated
    // document, and its snapshot would read `{}` forever no matter what it
    // declared.
    #[cfg(feature = "block-legalpages")]
    infos.push(crate::blocks::legalpages::LegalPagesBlock::new().info());

    // The dev sandbox ships only under its own (non-default) feature, so its
    // `BlockInfo` joins the document only when the block is compiled in. Like
    // `llm` above, `info()` is declarative — neither the `RuntimeControl` nor
    // the `ShellSource` handle it is built with is ever called here, so the
    // test doubles suffice.
    #[cfg(feature = "block-dev")]
    infos.push(
        crate::blocks::dev::DevBlock::with_workspace(crate::blocks::dev::DevShared::new(
            crate::blocks::dev::test_support::FakeControl::new(),
            Arc::new(crate::blocks::dev::test_support::FakeShell::new()),
        ))
        .info(),
    );

    infos
}

/// The JWT secret every `handle_request` test call passes.
pub const TEST_JWT_SECRET: &str = "test-jwt-secret";

/// An access token for `sub` carrying `roles`, signed the way `auth_ui` signs
/// one: the block-derived key from [`TEST_JWT_SECRET`] and `expected_issuer`'s
/// default issuer. Both `crate::crypto::verify_access_token` (and so
/// `pipeline::handle_request`'s step 2 and `AuthServiceImpl`) accept it.
///
/// A `TestContext` that must *verify* one of these needs
/// `set_config(blocks::auth::JWT_SECRET_KEY, TEST_JWT_SECRET)` — the secret is
/// read from the config snapshot, not passed in.
pub fn access_token_for(sub: &str, roles: &[&str]) -> String {
    use std::time::Duration;

    use wafer_block_crypto::primitives;

    let derived = primitives::derive_block_key(
        TEST_JWT_SECRET.as_bytes(),
        crate::blocks::auth_ui::AUTH_UI_BLOCK_ID,
    );
    let mut claims = BTreeMap::new();
    claims.insert("sub".to_string(), serde_json::json!(sub));
    claims.insert("type".to_string(), serde_json::json!("access"));
    // Must match `expected_issuer`'s default
    // (`crate::crypto::expected_issuer`): a `TestContext` has
    // no `WAFER_RUN_SHARED__FRONTEND_URL` configured.
    claims.insert(
        "iss".to_string(),
        serde_json::json!("http://localhost:5173"),
    );
    claims.insert("roles".to_string(), serde_json::json!(roles));
    primitives::jwt_sign(claims, Duration::from_secs(3600), derived.as_bytes())
        .expect("test jwt_sign")
}

/// A `Bearer` access token carrying `roles`, so
/// `pipeline::handle_request`'s step 2 resolves it to a real identity with
/// those roles. This is how a test asks for a document *as* an authenticated
/// or admin caller.
pub fn bearer_for_roles(roles: &[&str]) -> String {
    format!("Bearer {}", access_token_for("user-test-1", roles))
}

/// Fetch a discovery document (`/openapi.json` or `/.well-known/agent.json`)
/// generated from [`real_block_infos`], as the caller `roles` describes:
/// `None` is anonymous, `Some(&["user"])` an authenticated user,
/// `Some(&["admin"])` an admin. The documents are filtered by the caller's
/// tier the same way the WebMCP manifest is, so which caller asks matters.
///
/// The identity is pre-resolved on the message (the same `auth.*` meta
/// [`admin_msg`] sets), not minted as a JWT, so this works on a bare
/// [`TestContext::new`] — no auth tables needed. What these tests check is
/// the filter given a caller; that step 2 resolves a real bearer into that
/// caller *before* the filter runs is pinned separately, through
/// [`bearer_for_roles`] on a [`TestContext::with_auth`]
/// (`pipeline::discovery_tests::openapi_describes_admin_endpoints_to_an_admin`).
#[cfg(all(
    feature = "block-files",
    feature = "block-messages",
    feature = "block-products",
    feature = "block-tickets",
    feature = "block-llm",
    feature = "block-vector"
))]
pub async fn discovery_json_as(
    ctx: &TestContext,
    path: &str,
    host: &str,
    roles: Option<&[&str]>,
) -> serde_json::Value {
    let mut msg = match roles {
        None => anon_msg("retrieve", path),
        Some(roles) => {
            let mut msg = auth_msg("retrieve", path, "user-test-1");
            msg.set_meta("auth.user_roles", roles.join(","));
            msg
        }
    };
    msg.set_meta("http.header.host", host);
    // The pipeline is the router block's code.
    let out = crate::pipeline::handle_request(
        &ctx.clone()
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID),
        msg,
        InputStream::from_bytes(Vec::new()),
        None,
        TEST_JWT_SECRET,
        false,
        &crate::features::AllEnabled,
        &real_block_infos(),
        &[],
    )
    .await;
    let buf = collect_or_panic(out).await;
    serde_json::from_slice(&buf.body).expect("discovery response is valid JSON")
}

/// The *complete* discovery document — fetched as an admin, the one caller
/// who sees every endpoint. Shared by `pipeline.rs`'s discovery tests and the
/// per-block openapi snapshot gate, both of which assert on privileged
/// endpoints; a test about what a lower tier receives uses
/// [`discovery_json_as`] directly.
#[cfg(all(
    feature = "block-files",
    feature = "block-messages",
    feature = "block-products",
    feature = "block-tickets",
    feature = "block-llm",
    feature = "block-vector"
))]
pub async fn discovery_json(ctx: &TestContext, path: &str, host: &str) -> serde_json::Value {
    discovery_json_as(ctx, path, host, Some(&["admin"])).await
}

/// Fetch the generated `/openapi.json` document. Shared by pipeline tests
/// and the per-block snapshot gate.
#[cfg(feature = "test-support")]
#[cfg(all(
    feature = "block-files",
    feature = "block-messages",
    feature = "block-products",
    feature = "block-tickets",
    feature = "block-llm",
    feature = "block-vector"
))]
pub async fn openapi_document(ctx: &TestContext) -> serde_json::Value {
    discovery_json(ctx, "/openapi.json", "impresspress.example.com").await
}

// ---------------------------------------------------------------------------
// In-memory storage backend
// ---------------------------------------------------------------------------

/// The folder name the object store actually sees, for a block's own folder.
///
/// The same rule `wafer_core::interfaces::storage::handler::resolve_folder`
/// applies on the real path:
/// a block's namespace root (`folder == ""`) is the caller id ALONE, with no
/// trailing separator — `impresspress/dev`, not `impresspress/dev/`. Written
/// once here because both [`TestContext::storage_get`] and
/// [`TestContext::hold_next_storage_get`] address the store underneath the
/// storage block, and a key that is off by a separator addresses
/// nothing: the read answers `NotFound` and the hold parks a `get` that never
/// comes.
fn store_folder(block: &str, folder: &str) -> String {
    if folder.is_empty() {
        block.to_string()
    } else {
        format!("{block}/{folder}")
    }
}

/// A `get` [`InMemoryStorageService::hold_next_get`] has parked, and the
/// handle that lets it through.
///
/// The park is **bounded** — it gives up after a fixed number of polls — and
/// that bound is load-bearing rather than defensive. A test that parks a read
/// and then drives a mutation is asserting one of two outcomes: either the
/// mutation runs to completion while the read is suspended (the read is
/// unserialized, and the release arrives), or the mutation blocks on the lock
/// the read is holding and the release can never arrive at all. Only the bound
/// distinguishes the second case from a hang, so the correct behaviour is the
/// one that exhausts the budget.
///
/// Which makes both accessors obligatory in a serialization test:
/// [`Self::was_reached`], because a seam that never fired proves nothing, and
/// [`Self::budget_expired`], because "the release never arrived" and "the
/// release arrived late" resume identically and every other assertion in such
/// a test passes either way.
///
/// # Why this exists alongside `gc::GcInterleave`
///
/// They answer the same question — "nothing in the fixture yields, so no
/// ordering is observable" — at two different levels, and the project's
/// preference for one canonical seam is why the difference is written down
/// rather than left to be rediscovered:
///
/// * [`crate::blocks::dev::gc::GcInterleave`] is a **production** seam: a
///   trait on the collector, passed `Uninterrupted` in the shipped build. It
///   exists because the collector's soundness argument is about one specific
///   gap (between its listings and its roots) that no caller can reach from
///   outside. It is the right shape when the interleaving point is internal to
///   one function and has to be named as part of that function's contract.
/// * `HeldGet` is a **fixture** seam, underneath the object store. It reaches
///   any handler that reads any object, with no production code changed at
///   all. It is the right shape when the property under test is "this handler
///   is serialized against that one", where adding a seam to every handler
///   involved would be adding production surface to observe something that is
///   not the handler's own contract.
///
/// A new test should prefer `HeldGet` unless the gap it needs is genuinely
/// invisible from the store — and a second production seam of `GcInterleave`'s
/// kind should be argued for rather than assumed.
#[derive(Default)]
pub struct HeldGet {
    released: std::sync::atomic::AtomicBool,
    reached: std::sync::atomic::AtomicBool,
    expired: std::sync::atomic::AtomicBool,
}

impl HeldGet {
    /// How many polls a parked `get` waits before giving up.
    ///
    /// The bound exists only to turn a hang into a reported failure, so it is
    /// deliberately far larger than any mutation a test drives against it.
    /// That sizing is not cosmetic: the other half of the join is re-polled on
    /// every one of these polls, so a budget in the hundreds RACES the
    /// mutation, and a mutation whose storage or database calls happen to
    /// suspend more times than the budget allows would expire the park for a
    /// reason that has nothing to do with the lock under test — the park would
    /// end early, the mutation would not have landed, and the test would pass
    /// against unfixed code. That is a flake, and it is the shape `cc67a3d8`
    /// already recorded once in this repository.
    ///
    /// A hundred thousand self-waking polls is tens of milliseconds, which is
    /// what a genuine deadlock costs before it is reported, and no mutation in
    /// these fixtures comes within three orders of magnitude of it.
    const POLL_BUDGET: u32 = 100_000;

    /// Let the parked `get` through.
    pub fn release(&self) {
        self.released
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Whether a `get` ever actually parked here.
    ///
    /// A test that never reached its own seam proves nothing, so assert this.
    pub fn was_reached(&self) -> bool {
        self.reached.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Whether the park ended on its poll budget rather than on a release.
    ///
    /// The two outcomes are what a serialization test is choosing between, and
    /// nothing else distinguishes them: a park that was released and a park
    /// that timed out both resume and both let the rest of the test pass. Only
    /// this says which happened.
    ///
    /// Assert it wherever the argument is "the other half of the join could
    /// not reach the release, because it is blocked on the lock this read is
    /// holding". Without it, a future change that merely made the other half
    /// SLOW — one more `await` on its way to the release — would exhaust the
    /// budget for an entirely different reason and the test would keep
    /// passing, against unfixed code. `cc67a3d8` records a concurrency test in
    /// this repository that flaked for exactly that shape.
    pub fn budget_expired(&self) -> bool {
        self.expired.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Suspend until [`Self::release`] or the poll budget, whichever is first.
    async fn park(&self) {
        self.reached
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let mut polls = 0u32;
        std::future::poll_fn(|cx| {
            if self.released.load(std::sync::atomic::Ordering::SeqCst) {
                return std::task::Poll::Ready(());
            }
            if polls >= Self::POLL_BUDGET {
                self.expired
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                return std::task::Poll::Ready(());
            }
            polls += 1;
            // Wake immediately: the point is to hand the executor a chance to
            // poll whatever else the test is driving, not to sleep.
            cx.waker().wake_by_ref();
            std::task::Poll::Pending
        })
        .await;
    }
}

/// One object held by [`InMemoryStorageService`].
struct StoredObject {
    data: Vec<u8>,
    content_type: String,
    last_modified: chrono::DateTime<chrono::Utc>,
}

/// In-memory [`StorageService`](wafer_core::interfaces::storage::service::StorageService)
/// for fixtures that need a working object store.
///
/// The counterpart of this module's in-memory SQLite: a *real* backend with
/// real semantics (a `get` of an absent key is `NotFound`, a `list` honours
/// prefix/offset/limit, a `put` overwrites), not a stub that answers `Ok` to
/// everything. Tests that assert content-addressed storage — same bytes
/// stored once, a stale read failing — only mean something against a backend
/// that can actually say no.
///
/// Registered under `wafer-run/storage` behind
/// [`crate::blocks::storage::ImpresspressStorageBlock`], so a fixture also
/// exercises the storage handler's per-block namespacing
/// (`{caller}/{folder}/{key}`) and its cross-block grant checks.
#[derive(Default)]
pub struct InMemoryStorageService {
    /// Objects keyed by `(folder, key)`. `BTreeMap` so `list` is ordered
    /// without a sort, matching the lexicographic order object stores return.
    objects: Mutex<std::collections::BTreeMap<(String, String), StoredObject>>,
    /// Folder name → `(public, created_at)`. A `put` auto-creates the folder,
    /// as a filesystem backend's `create_dir_all` and S3's implicit prefixes
    /// both do.
    folders: Mutex<std::collections::BTreeMap<String, (bool, chrono::DateTime<chrono::Utc>)>>,
    /// Set by [`Self::fail_next_put`]: the message the next `put` refuses
    /// with, consumed by that call.
    ///
    /// A publish that fails *after* the runtime has been swapped is the one
    /// path the sandbox has to unwind (design §7.3), and nothing else in a
    /// fixture can produce it: every other failure is refused before anything
    /// has been changed.
    fail_next_put: Mutex<Option<String>>,
    /// One-shot `put` refusals for one object each, keyed by `(folder, key)`
    /// and installed by [`Self::fail_next_put_to`].
    fail_puts: Mutex<std::collections::BTreeMap<(String, String), String>>,
    /// One-shot `delete` refusals, keyed and installed the same way by
    /// [`Self::fail_next_delete_of`].
    fail_deletes: Mutex<std::collections::BTreeMap<(String, String), String>>,
    /// Every read in the order it arrived, as `"{op} {folder}/{key}"` with
    /// `op` one of `get` and `get_streaming`. Kept apart from [`Self::ops`],
    /// whose consumers assert the exact sequence of writes.
    reads: Mutex<Vec<String>>,
    /// Objects whose next `get` parks, keyed by `(folder, key)` and installed
    /// by [`Self::hold_next_get`]. One-shot: the entry is taken by the `get`
    /// that matches it.
    ///
    /// The store is otherwise entirely synchronous — every operation resolves
    /// on its first poll — which is exactly why an ordering bug is invisible
    /// in a fixture (`blocks::dev::gc`'s `GcInterleave` says the same thing
    /// about the collector). A real backend suspends on every call; this is
    /// how a test puts one suspension where the property under test lives.
    held_gets: Mutex<std::collections::BTreeMap<(String, String), Arc<HeldGet>>>,
    /// Every mutating operation in the order it arrived, as
    /// `"{op} {folder}/{key}"`.
    ///
    /// Content-addressed stores make *what* was written easy to assert and
    /// *when* impossible: two publishes of the same bytes are indistinguishable
    /// in the final state. The site publisher's contract is an ordering one
    /// (`index.html` after everything it references), so the order has to be
    /// recorded as it happens.
    ops: Mutex<Vec<String>>,
}

impl InMemoryStorageService {
    /// A store with no folders and no objects.
    pub fn new() -> Self {
        Self::default()
    }

    /// Park the next `get` of `folder`/`key` until the returned handle is
    /// released, so a test can drive something else while one read is
    /// suspended mid-flight.
    ///
    /// The suspension is what makes a concurrency test mean anything here: an
    /// in-memory store never yields, so two futures joined against it run one
    /// after the other and no interleaving a handler is vulnerable to can
    /// occur. See [`HeldGet`] for why the park is bounded.
    pub fn hold_next_get(&self, folder: &str, key: &str) -> Arc<HeldGet> {
        let hold = Arc::new(HeldGet::default());
        self.held_gets
            .lock()
            .expect("held_gets mutex poisoned")
            .insert((folder.to_string(), key.to_string()), hold.clone());
        hold
    }

    /// Make the next `put` refuse with `message`, storing nothing.
    pub fn fail_next_put(&self, message: &str) {
        *self.fail_next_put.lock().expect("fail_next_put mutex") = Some(message.to_string());
    }

    /// Make the next `put` of `folder`/`key` refuse with `message`, storing
    /// nothing. Other objects' `put`s are unaffected.
    pub fn fail_next_put_to(&self, folder: &str, key: &str, message: &str) {
        self.fail_puts
            .lock()
            .expect("fail_puts mutex")
            .insert((folder.to_string(), key.to_string()), message.to_string());
    }

    /// Make the next `delete` of `folder`/`key` refuse with `message`,
    /// deleting nothing.
    pub fn fail_next_delete_of(&self, folder: &str, key: &str, message: &str) {
        self.fail_deletes
            .lock()
            .expect("fail_deletes mutex")
            .insert((folder.to_string(), key.to_string()), message.to_string());
    }

    /// Every mutating operation this store has seen, oldest first.
    pub fn ops(&self) -> Vec<String> {
        self.ops.lock().expect("ops mutex poisoned").clone()
    }

    /// Every read this store has served, oldest first.
    pub fn reads(&self) -> Vec<String> {
        self.reads.lock().expect("reads mutex poisoned").clone()
    }

    /// Record one read.
    fn record_read(&self, op: &str, folder: &str, key: &str) {
        self.reads
            .lock()
            .expect("reads mutex poisoned")
            .push(format!("{op} {folder}/{key}"));
    }

    /// The object at `folder`/`key`, with its metadata.
    fn lookup(
        &self,
        folder: &str,
        key: &str,
    ) -> Result<
        (
            Vec<u8>,
            wafer_core::interfaces::storage::service::ObjectInfo,
        ),
        wafer_core::interfaces::storage::service::StorageError,
    > {
        let guard = self.objects.lock().expect("objects mutex poisoned");
        let object = guard
            .get(&(folder.to_string(), key.to_string()))
            .ok_or(wafer_core::interfaces::storage::service::StorageError::NotFound)?;
        Ok((
            object.data.clone(),
            wafer_core::interfaces::storage::service::ObjectInfo {
                key: key.to_string(),
                size: object.data.len() as i64,
                content_type: object.content_type.clone(),
                last_modified: object.last_modified,
            },
        ))
    }

    /// Record one mutating operation.
    fn record(&self, op: &str, folder: &str, key: &str) {
        self.ops
            .lock()
            .expect("ops mutex poisoned")
            .push(format!("{op} {folder}/{key}"));
    }
}

#[wafer_block::wafer_async_trait]
impl wafer_core::interfaces::storage::service::StorageService for InMemoryStorageService {
    async fn put(
        &self,
        folder: &str,
        key: &str,
        data: &[u8],
        content_type: &str,
    ) -> Result<(), wafer_core::interfaces::storage::service::StorageError> {
        let now = chrono::Utc::now();
        self.record("put", folder, key);
        if let Some(message) = self
            .fail_next_put
            .lock()
            .expect("fail_next_put mutex")
            .take()
        {
            return Err(wafer_core::interfaces::storage::service::StorageError::Internal(message));
        }
        if let Some(message) = self
            .fail_puts
            .lock()
            .expect("fail_puts mutex")
            .remove(&(folder.to_string(), key.to_string()))
        {
            return Err(wafer_core::interfaces::storage::service::StorageError::Internal(message));
        }
        self.folders
            .lock()
            .expect("folders mutex poisoned")
            .entry(folder.to_string())
            .or_insert((false, now));
        self.objects.lock().expect("objects mutex poisoned").insert(
            (folder.to_string(), key.to_string()),
            StoredObject {
                data: data.to_vec(),
                content_type: content_type.to_string(),
                last_modified: now,
            },
        );
        Ok(())
    }

    async fn get(
        &self,
        folder: &str,
        key: &str,
    ) -> Result<
        (
            Vec<u8>,
            wafer_core::interfaces::storage::service::ObjectInfo,
        ),
        wafer_core::interfaces::storage::service::StorageError,
    > {
        // Before the lookup, so a parked read observes whatever the code it
        // was interleaved with left behind rather than a snapshot taken
        // beforehand. The hold is taken out of the map here, so it fires once.
        let hold = self
            .held_gets
            .lock()
            .expect("held_gets mutex poisoned")
            .remove(&(folder.to_string(), key.to_string()));
        if let Some(hold) = hold {
            hold.park().await;
        }
        self.record_read("get", folder, key);
        self.lookup(folder, key)
    }

    /// Overridden rather than left to the trait default, which calls
    /// [`Self::get`] and would record every streaming read as a buffered one —
    /// the distinction [`Self::reads`] exists to make.
    async fn get_streaming(
        &self,
        folder: &str,
        key: &str,
    ) -> Result<
        (
            OutputStream,
            wafer_core::interfaces::storage::service::ObjectInfo,
        ),
        wafer_core::interfaces::storage::service::StorageError,
    > {
        self.record_read("get_streaming", folder, key);
        let (data, info) = self.lookup(folder, key)?;
        Ok((OutputStream::respond(data), info))
    }

    async fn delete(
        &self,
        folder: &str,
        key: &str,
    ) -> Result<(), wafer_core::interfaces::storage::service::StorageError> {
        self.record("delete", folder, key);
        if let Some(message) = self
            .fail_deletes
            .lock()
            .expect("fail_deletes mutex")
            .remove(&(folder.to_string(), key.to_string()))
        {
            return Err(wafer_core::interfaces::storage::service::StorageError::Internal(message));
        }
        self.objects
            .lock()
            .expect("objects mutex poisoned")
            .remove(&(folder.to_string(), key.to_string()))
            .map(|_| ())
            .ok_or(wafer_core::interfaces::storage::service::StorageError::NotFound)
    }

    async fn list(
        &self,
        folder: &str,
        opts: &wafer_core::interfaces::storage::service::ListOptions,
    ) -> Result<
        wafer_core::interfaces::storage::service::ObjectList,
        wafer_core::interfaces::storage::service::StorageError,
    > {
        let guard = self.objects.lock().expect("objects mutex poisoned");
        let matched: Vec<wafer_core::interfaces::storage::service::ObjectInfo> = guard
            .iter()
            .filter(|((f, k), _)| f == folder && k.starts_with(&opts.prefix))
            .map(
                |((_, k), object)| wafer_core::interfaces::storage::service::ObjectInfo {
                    key: k.clone(),
                    size: object.data.len() as i64,
                    content_type: object.content_type.clone(),
                    last_modified: object.last_modified,
                },
            )
            .collect();
        let total_count = matched.len() as i64;
        let skipped = matched.into_iter().skip(opts.offset.max(0) as usize);
        let objects = if opts.limit > 0 {
            skipped.take(opts.limit as usize).collect()
        } else {
            skipped.collect()
        };
        Ok(wafer_core::interfaces::storage::service::ObjectList {
            objects,
            total_count,
            // No cursor support: the backend is offset-only, which
            // `ObjectList::next_cursor` documents as the `None` case.
            next_cursor: None,
        })
    }

    async fn create_folder(
        &self,
        name: &str,
        public: bool,
    ) -> Result<(), wafer_core::interfaces::storage::service::StorageError> {
        self.folders
            .lock()
            .expect("folders mutex poisoned")
            .insert(name.to_string(), (public, chrono::Utc::now()));
        Ok(())
    }

    async fn delete_folder(
        &self,
        name: &str,
    ) -> Result<(), wafer_core::interfaces::storage::service::StorageError> {
        self.folders
            .lock()
            .expect("folders mutex poisoned")
            .remove(name);
        self.objects
            .lock()
            .expect("objects mutex poisoned")
            .retain(|(folder, _), _| folder != name);
        Ok(())
    }

    async fn list_folders(
        &self,
    ) -> Result<
        Vec<wafer_core::interfaces::storage::service::FolderInfo>,
        wafer_core::interfaces::storage::service::StorageError,
    > {
        Ok(self
            .folders
            .lock()
            .expect("folders mutex poisoned")
            .iter()
            .map(|(name, (public, created_at))| {
                wafer_core::interfaces::storage::service::FolderInfo {
                    name: name.clone(),
                    public: *public,
                    created_at: *created_at,
                }
            })
            .collect())
    }
}

// ---------------------------------------------------------------------------
// Log capture
// ---------------------------------------------------------------------------

/// Minimal [`tracing::Subscriber`] that records the rendered `message` field
/// of every event it sees, so a test can assert on what was (or was not)
/// logged without pulling in `tracing-subscriber`.
///
/// Shared, because more than one route has had to prove it does NOT log:
/// refusal diagnostics that are static across calls belong at runtime
/// construction, not on a path a caller can loop (`pipeline`'s
/// `/b/webmcp/manifest.json`, `blocks::dev::tools`' `/b/dev/api/tools.json`).
/// Install it with `tracing::subscriber::set_default`, which is scoped to the
/// current thread — so a `#[tokio::test]` on the multi-thread runtime would
/// miss events from work that migrated to another worker. Every use so far
/// runs the awaited call on the test's own thread.
///
/// The STRUCTURED FIELDS are captured separately from the message, and a test
/// that cares what a line claims has to assert on both. A field is not a lesser
/// part of the line: a claim this module had deliberately taken OUT of a
/// message text — `warn_how_to_undo_a_pin`'s count of upgrade pins, removed
/// because it could contradict the page it points at — was still being emitted
/// as `upgrade_pins=…`, and a message-only assertion saw nothing.
#[derive(Clone, Default)]
pub struct MessageCapture {
    messages: Arc<Mutex<Vec<String>>>,
    /// One entry per event: its non-`message` fields rendered as
    /// `name=value` and joined with spaces, in the order `tracing` visits them.
    fields: Arc<Mutex<Vec<String>>>,
}

#[derive(Default)]
struct MessageVisitor {
    message: String,
    fields: String,
}

impl tracing::field::Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
            return;
        }
        if !self.fields.is_empty() {
            self.fields.push(' ');
        }
        self.fields.push_str(&format!("{}={value:?}", field.name()));
    }
}

impl tracing::Subscriber for MessageCapture {
    fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        self.messages
            .lock()
            .expect("MessageCapture mutex poisoned")
            .push(visitor.message);
        self.fields
            .lock()
            .expect("MessageCapture mutex poisoned")
            .push(visitor.fields);
    }
    fn enter(&self, _span: &tracing::span::Id) {}
    fn exit(&self, _span: &tracing::span::Id) {}
}

impl MessageCapture {
    /// How many captured messages contain `needle`. Message text only — see
    /// [`Self::count_fields_containing`] for what the same line carried as
    /// structured fields.
    pub fn count_containing(&self, needle: &str) -> usize {
        Self::count(&self.messages, needle)
    }

    /// How many captured events carry `needle` among their non-`message`
    /// fields, each rendered `name=value`.
    pub fn count_fields_containing(&self, needle: &str) -> usize {
        Self::count(&self.fields, needle)
    }

    fn count(store: &Arc<Mutex<Vec<String>>>, needle: &str) -> usize {
        store
            .lock()
            .expect("MessageCapture mutex poisoned")
            .iter()
            .filter(|entry| entry.contains(needle))
            .count()
    }
}

#[cfg(test)]
mod tests {
    use wafer_block::db::ListOptions;
    use wafer_core::clients::database as db;

    use super::*;

    #[tokio::test]
    async fn database_create_and_get_round_trip() {
        let ctx = TestContext::new().await;

        db::exec_raw(
            &ctx.fixture(),
            "CREATE TABLE round_trip (id TEXT PRIMARY KEY, name TEXT)",
            &[],
        )
        .await
        .expect("create table");

        db::exec_raw(
            &ctx.fixture(),
            "INSERT INTO round_trip (id, name) VALUES (?, ?)",
            &[serde_json::json!("r1"), serde_json::json!("alpha")],
        )
        .await
        .expect("insert row");

        let rows = db::query_raw(
            &ctx.fixture(),
            "SELECT id, name FROM round_trip WHERE id = ?",
            &[serde_json::json!("r1")],
        )
        .await
        .expect("select");

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "r1");
        assert_eq!(
            rows[0].data.get("name").and_then(|v| v.as_str()),
            Some("alpha")
        );
    }

    #[test]
    fn anon_msg_sets_action_and_path_with_no_user_id() {
        let m = anon_msg("retrieve", "/b/auth/login");
        assert_eq!(m.action(), "retrieve");
        assert_eq!(m.path(), "/b/auth/login");
        assert_eq!(m.user_id(), "");
    }

    #[test]
    fn auth_msg_sets_user_id() {
        let m = auth_msg("retrieve", "/b/userportal/", "user-a");
        assert_eq!(m.action(), "retrieve");
        assert_eq!(m.path(), "/b/userportal/");
        assert_eq!(m.user_id(), "user-a");
    }

    #[test]
    fn admin_msg_marks_admin_role() {
        use crate::util::is_admin;
        let m = admin_msg("retrieve", "/b/admin/users");
        assert_eq!(m.user_id(), "admin_1");
        assert!(is_admin(&m));
    }

    #[tokio::test]
    async fn output_status_reads_status_meta() {
        use crate::http::ResponseBuilder;
        let out = ResponseBuilder::new()
            .status(302)
            .body(Vec::new(), "text/plain");
        assert_eq!(output_status(out).await, 302);
    }

    #[tokio::test]
    async fn output_status_defaults_to_200_when_unset() {
        use crate::http::ResponseBuilder;
        let out = ResponseBuilder::new().body(Vec::new(), "text/plain");
        assert_eq!(output_status(out).await, 200);
    }

    #[tokio::test]
    async fn output_header_reads_named_header() {
        use crate::http::ResponseBuilder;
        let out = ResponseBuilder::new()
            .status(302)
            .set_header("Location", "/dashboard")
            .body(Vec::new(), "text/plain");
        assert_eq!(
            output_header(out, "Location").await.as_deref(),
            Some("/dashboard")
        );
    }

    #[tokio::test]
    async fn output_html_reads_body_as_utf8() {
        use crate::http::ResponseBuilder;
        let out = ResponseBuilder::new()
            .status(200)
            .body(b"<h1>hi</h1>".to_vec(), "text/html");
        assert_eq!(output_html(out).await, "<h1>hi</h1>");
    }

    #[tokio::test]
    async fn output_json_parses_body() {
        use crate::http::ResponseBuilder;
        let out = ResponseBuilder::new()
            .status(200)
            .body(br#"{"ok":true}"#.to_vec(), "application/json");
        assert_eq!(output_json(out).await, serde_json::json!({"ok": true}));
    }

    #[tokio::test]
    async fn with_auth_applies_orgs_and_users_tables() {
        let ctx = TestContext::with_auth().await;
        // Verify auth tables exist by inserting a user, then an org, then selecting.
        ctx.seed_auth_user("user-a").await;

        db::exec_raw(
            &ctx.fixture(),
            "INSERT INTO wafer_run__auth__orgs (id, name, owner_user_id, is_reserved, created_at) \
             VALUES (?, ?, ?, 0, ?)",
            &[
                serde_json::json!("org-1"),
                serde_json::json!("acme"),
                serde_json::json!("user-a"),
                serde_json::json!("2026-01-01T00:00:00Z"),
            ],
        )
        .await
        .expect("insert org");

        let rows = db::query_raw(
            &ctx.fixture(),
            "SELECT name FROM wafer_run__auth__orgs WHERE id = ?",
            &[serde_json::json!("org-1")],
        )
        .await
        .expect("select org");
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].data.get("name").and_then(|v| v.as_str()),
            Some("acme")
        );
    }

    #[tokio::test]
    async fn registered_block_is_dispatched_through_call_block() {
        use async_trait::async_trait;
        use wafer_run::{Block as RunBlock, BlockCategory, BlockInfo, LifecycleEvent};

        struct EchoBlock;

        #[async_trait]
        impl RunBlock for EchoBlock {
            fn info(&self) -> BlockInfo {
                BlockInfo::new("test/echo", "0.0.1", "echo@v1", "echoes the request path")
                    .category(BlockCategory::Service)
            }

            async fn handle(
                &self,
                _ctx: &dyn Context,
                msg: Message,
                _input: InputStream,
            ) -> OutputStream {
                crate::http::ResponseBuilder::new()
                    .status(200)
                    .body(msg.path().as_bytes().to_vec(), "text/plain")
            }

            async fn lifecycle(
                &self,
                _ctx: &dyn Context,
                _e: LifecycleEvent,
            ) -> Result<(), WaferError> {
                Ok(())
            }
        }

        let mut ctx = TestContext::new().await;
        ctx.register_block("test/echo", Arc::new(EchoBlock));

        let msg = anon_msg("retrieve", "/echo-me");
        let resp = ctx.call_block("test/echo", msg, InputStream::empty()).await;
        let body = output_html(resp).await;
        assert_eq!(body, "/echo-me");
    }

    #[tokio::test]
    async fn a_block_frame_is_denied_an_unowned_table_without_a_grant() {
        // Caller "block-x" tries to read auth-owned table; no grants → denied.
        let ctx = TestContext::with_auth().await.running_as("test/block-x");

        let result = db::list(&ctx, "wafer_run__auth__users", &ListOptions::default()).await;

        let err = result.expect_err("WRAP must deny call without grant");
        assert!(
            err.to_string().contains("WRAP"),
            "error must mention WRAP, got: {err}"
        );
    }

    /// Every entry in the filtered-write family, so a new one added to the
    /// `DatabaseService` trait shows up here as a compile error rather than
    /// as a silently inherited default.
    async fn filtered_writes_all_fail(
        svc: &dyn wafer_core::interfaces::database::service::DatabaseService,
        collection: &str,
        filters: &[wafer_block::db::Filter],
        label: &str,
    ) {
        let data = HashMap::from([(
            "name".to_string(),
            serde_json::json!("written despite the fault"),
        )]);
        assert!(
            svc.update_where(collection, filters, data.clone())
                .await
                .is_err(),
            "update_where must fail ({label})"
        );
        assert!(
            svc.update_where_count(collection, filters, data)
                .await
                .is_err(),
            "update_where_count must fail ({label})"
        );
        assert!(
            svc.delete_where(collection, filters).await.is_err(),
            "delete_where must fail ({label})"
        );
        assert!(
            svc.delete_where_count(collection, filters).await.is_err(),
            "delete_where_count must fail ({label})"
        );
        assert!(
            svc.take_where(collection, filters).await.is_err(),
            "take_where must fail ({label})"
        );
        assert!(
            svc.increment_field_where(collection, "hits", 1, filters)
                .await
                .is_err(),
            "increment_field_where must fail ({label})"
        );
    }

    /// `id = ?` for a row that does not exist — the case that separates a
    /// correct double from one riding the trait defaults.
    fn matches_nothing() -> Vec<wafer_block::db::Filter> {
        vec![wafer_block::db::Filter {
            field: "id".to_string(),
            operator: wafer_block::db::FilterOp::Equal,
            value: serde_json::json!("no-such-row"),
        }]
    }

    fn matches_the_seeded_row() -> Vec<wafer_block::db::Filter> {
        vec![wafer_block::db::Filter {
            field: "id".to_string(),
            operator: wafer_block::db::FilterOp::Equal,
            value: serde_json::json!("r1"),
        }]
    }

    async fn seeded_ctx() -> TestContext {
        let ctx = TestContext::new().await;
        db::exec_raw(
            &ctx.fixture(),
            "CREATE TABLE filtered_writes (id TEXT PRIMARY KEY, name TEXT, hits INTEGER              DEFAULT 0, created_at TEXT, updated_at TEXT)",
            &[],
        )
        .await
        .expect("create table");
        db::exec_raw(
            &ctx.fixture(),
            "INSERT INTO filtered_writes (id, name, hits) VALUES ('r1', 'alpha', 0)",
            &[],
        )
        .await
        .expect("seed row");
        ctx
    }

    /// [`TestContext::break_writes`] simulates a backend whose writes fail.
    /// The filtered-write family has to fail there too — and it is the family
    /// the `DatabaseService` trait ships READ-BASED defaults for:
    /// `update_where_count` counts and then updates, `update_where` lists and
    /// then updates by id, `delete_where` lists and then deletes by id,
    /// `take_where` lists and then deletes, and `increment_field_where`
    /// reports "not implemented by this database backend".
    ///
    /// `wafer-block-sqlite`, `wafer-block-postgres` and `D1DatabaseService`
    /// every one override the family with a SINGLE statement carrying no
    /// `count` and no `list`, so a double that inherits the defaults is a
    /// database that does not exist. The tell is a write matching zero rows:
    /// the defaults list nothing, update nothing, and return `Ok` — a
    /// *successful* write on a backend whose writes are supposed to be
    /// failing. A handler branch reachable only when a filtered write fails
    /// then looks covered while production can never enter it, which is
    /// exactly how the restore collision-probe test in
    /// `products/tests/handler_tests.rs` came to assert an outcome production
    /// could not produce (that instance was fixed on `FailingReadsDb`, leaving
    /// this one armed for the next test to use it).
    #[tokio::test]
    async fn break_writes_fails_every_filtered_write() {
        let ctx = seeded_ctx().await.break_writes();

        // The zero-match case first: this is the one the trait defaults get
        // wrong, by doing nothing and calling it success.
        filtered_writes_all_fail(
            ctx.db_service.as_ref(),
            "filtered_writes",
            &matches_nothing(),
            "no rows matched",
        )
        .await;
        // And the matching case, which the defaults happen to fail — but
        // only after a read the real backends never issue.
        filtered_writes_all_fail(
            ctx.db_service.as_ref(),
            "filtered_writes",
            &matches_the_seeded_row(),
            "one row matched",
        )
        .await;

        // Reads still delegate, which is the whole point of `break_writes`:
        // a handler's "read current state, then persist a change" shape must
        // reach the branch under test rather than failing earlier.
        let row = ctx
            .db_service
            .get("filtered_writes", "r1")
            .await
            .expect("reads must still resolve under break_writes");
        assert_eq!(
            row.data.get("name").and_then(|v| v.as_str()),
            Some("alpha"),
            "a failed filtered write must not have changed the row"
        );
    }

    /// The mirror of the above for [`TestContext::break_reads`]: its
    /// filtered-write overrides already delegate (a broken read layer must
    /// not fail a write), and `take_where` — which returns the rows it
    /// removed, so it reads as much as it writes — fails.
    ///
    /// `take_where` used to reach that failure through the trait's default
    /// (list, then delete by id), so the double only failed because the
    /// *list* did. Real backends issue one `DELETE … RETURNING *`, so the
    /// answer was right for a reason production does not have. It is stated
    /// directly now, like the rest of the family.
    #[tokio::test]
    async fn break_reads_leaves_filtered_writes_working_and_fails_take_where() {
        let ctx = seeded_ctx().await.break_reads();
        let filters = matches_the_seeded_row();
        let data = HashMap::from([("name".to_string(), serde_json::json!("beta"))]);

        assert_eq!(
            ctx.db_service
                .update_where_count("filtered_writes", &filters, data)
                .await
                .expect("a broken read layer must not fail a filtered write"),
            1,
        );
        assert!(
            ctx.db_service
                .take_where("filtered_writes", &filters)
                .await
                .is_err(),
            "take_where hands back the rows it removed, so a broken read layer must fail it"
        );
        assert_eq!(
            ctx.db_service
                .delete_where_count("filtered_writes", &filters)
                .await
                .expect("a broken read layer must not fail a filtered delete"),
            1,
        );
    }

    /// Gate 2: `call_block` refuses an action the TARGET's declared interface
    /// does not list, as `RuntimeContext::dispatch_call` does — and admits
    /// one it lists, so the gate is not simply refusing everything.
    ///
    /// Without this the gate has no test of its own: every other test calls
    /// actions the target declares, so deleting the gate changes nothing they
    /// assert. What it exists to catch is a call the fixture would certify
    /// and the runtime refuse, which is how `impresspress.config.get_many`
    /// reached a server under `config@v1`.
    #[tokio::test]
    async fn call_block_refuses_an_action_the_targets_interface_does_not_declare() {
        /// A block declaring wafer-run's `database@v1`, whose action map is
        /// the `database.*` op family.
        struct DatabaseShaped;

        #[wafer_block::wafer_async_trait]
        impl Block for DatabaseShaped {
            fn info(&self) -> wafer_run::BlockInfo {
                wafer_run::BlockInfo::new(
                    "test/database-shaped",
                    "0.0.1",
                    "database@v1",
                    "declares database@v1 and answers anything it is handed",
                )
            }

            async fn handle(
                &self,
                _ctx: &dyn Context,
                _msg: Message,
                _input: InputStream,
            ) -> OutputStream {
                OutputStream::respond(b"reached the block".to_vec())
            }
        }

        let mut ctx = TestContext::new().await;
        ctx.register_block("test/database-shaped", Arc::new(DatabaseShaped));

        let declared = ctx
            .call_block(
                "test/database-shaped",
                Message::new(wafer_block::common::ServiceOp::DATABASE_LIST),
                InputStream::empty(),
            )
            .await;
        assert_eq!(
            collect_or_panic(declared).await.body,
            b"reached the block",
            "a declared action must reach the block"
        );

        let refused = ctx
            .call_block(
                "test/database-shaped",
                Message::new("database.teleport"),
                InputStream::empty(),
            )
            .await;
        match refused.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::Unimplemented, "{e:?}");
                assert!(e.message.contains("database.teleport"), "{e:?}");
                assert!(e.message.contains("database@v1"), "{e:?}");
            }
            other => panic!("an undeclared action must be refused, got {other:?}"),
        }
    }

    /// The gate above validates against a HAND-MAINTAINED spec set
    /// ([`interface_specs`]): wafer-run's well-known specs plus this repo's
    /// own. A second `Wafer::register_interface` call site would put a spec on
    /// the runtime that the fixture does not have, and every call to that
    /// interface would then be checked here against nothing — the gate would
    /// silently skip it (`ActionCheck::UnknownInterface`) while production
    /// validated it.
    #[test]
    fn the_fixture_spec_set_matches_every_register_interface_call_site() {
        use crate::test_support::source_scan::{
            code_before_comment, strip_test_modules, SourceWalk,
        };

        let sites: Vec<(String, String)> = SourceWalk::crate_src()
            .least(300)
            .collect()
            .iter()
            .flat_map(|file| {
                strip_test_modules(&file.text)
                    .lines()
                    .filter(|line| code_before_comment(line).contains("register_interface("))
                    .map(|line| (file.rel.clone(), line.trim().to_string()))
                    .collect::<Vec<_>>()
            })
            .collect();

        assert_eq!(
            sites.len(),
            1,
            "this crate registers exactly one interface spec; every call site must be covered \
             by `test_support::interface_specs`, so a new one belongs in that list too — found \
             {sites:?}"
        );
        assert_eq!(sites[0].0, "blocks/config.rs", "{sites:?}");
        assert!(sites[0].1.contains("interface_spec()"), "{sites:?}");
        assert!(
            interface_specs().contains_key(crate::blocks::config::CONFIG_INTERFACE),
            "the fixture's spec set must hold the spec that call site registers"
        );
    }

    #[tokio::test]
    async fn a_deployment_grant_admits_the_block_it_names() {
        let mut ctx = TestContext::with_auth().await;
        ctx.add_deployment_grants(vec![ResourceGrant::read(
            "test/block-x",
            "wafer_run__auth__users",
        )]);
        let ctx = ctx.running_as("test/block-x");

        // Empty users table — listing must succeed (zero rows is success).
        let res = db::list(&ctx, "wafer_run__auth__users", &ListOptions::default())
            .await
            .expect("WRAP must allow listing with matching grant");
        assert_eq!(res.records.len(), 0);
    }

    /// A block reached through the router that reads a table it holds no
    /// grant for is refused, as the runtime refuses it — with no opt-in.
    ///
    /// Before the fixture enforced WRAP on every frame this passed silently:
    /// `dispatch` ran the block with no WRAP check at all, so a block missing
    /// its grant was certified by every routed test.
    #[tokio::test]
    async fn a_routed_block_reading_a_table_it_holds_no_grant_for_is_refused() {
        /// Answers with the outcome of listing `wafer-run/auth`'s users.
        struct ReadsAuthUsers;

        #[wafer_block::wafer_async_trait]
        impl Block for ReadsAuthUsers {
            fn info(&self) -> BlockInfo {
                BlockInfo::new("test/reader", "0.0.1", "http-handler@v1", "reads users")
            }

            async fn handle(
                &self,
                ctx: &dyn Context,
                _msg: Message,
                _input: InputStream,
            ) -> OutputStream {
                match db::list(ctx, "wafer_run__auth__users", &ListOptions::default()).await {
                    Ok(_) => OutputStream::respond(b"read".to_vec()),
                    Err(e) => OutputStream::error(e),
                }
            }
        }

        let mut ctx = TestContext::with_auth().await;
        ctx.register_block("test/reader", Arc::new(ReadsAuthUsers));
        ctx.add_extra_route(crate::routing::ExtraRoute::new(
            "/b/reader",
            "test/reader",
            crate::routing::RouteAccess::Public,
        ));

        let out = ctx
            .dispatch_resolved(anon_msg("retrieve", "/b/reader"))
            .await;
        match out.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::PermissionDenied, "{e:?}");
            }
            other => panic!("the ungranted read must be refused, got {other:?}"),
        }
    }

    /// A block the runtime would refuse to register is refused here too.
    /// `register_block` used to rename whatever it was handed to the name it
    /// was registered under and accept any declaration, so a block that
    /// could never boot passed every unit test.
    struct Declares(BlockInfo);

    /// A key in the shared namespace, which no block may declare.
    const RESERVED_PROBE_KEY: &str = "WAFER_RUN_SHARED__PROBE";
    /// A key in another block's namespace, which `test/declared` may not
    /// declare.
    const FOREIGN_PROBE_KEY: &str = "OTHER__BLOCK__KEY";

    #[wafer_block::wafer_async_trait]
    impl Block for Declares {
        fn info(&self) -> BlockInfo {
            self.0.clone()
        }

        async fn handle(
            &self,
            _ctx: &dyn Context,
            _msg: Message,
            _input: InputStream,
        ) -> OutputStream {
            OutputStream::respond(Vec::new())
        }
    }

    #[tokio::test]
    #[should_panic(
        expected = "the runtime refuses to register test/declared: block registered as \
                    'test/declared' reports its name as 'test/other'"
    )]
    async fn a_block_reporting_another_name_is_not_registered() {
        let mut ctx = TestContext::new().await;
        ctx.register_block(
            "test/declared",
            Arc::new(Declares(BlockInfo::new(
                "test/other",
                "0.0.1",
                "probe@v1",
                "misnamed",
            ))),
        );
    }

    #[tokio::test]
    #[should_panic(
        expected = "the runtime refuses to register test/declared: block 'test/declared' \
                    declares reserved config key"
        // The key itself is `RESERVED_PROBE_KEY`, which an attribute cannot
        // name; the variant's own wording is what distinguishes this refusal.
    )]
    async fn a_block_declaring_a_reserved_config_key_is_not_registered() {
        let mut ctx = TestContext::new().await;
        ctx.register_block(
            "test/declared",
            Arc::new(Declares(
                BlockInfo::new("test/declared", "0.0.1", "probe@v1", "reserved key").config_keys(
                    vec![wafer_run::ConfigVar::new(
                        RESERVED_PROBE_KEY,
                        "reserved",
                        "",
                    )],
                ),
            )),
        );
    }

    #[tokio::test]
    #[should_panic(
        expected = "the runtime refuses to register test/declared: block 'test/declared' \
                    declares config var 'OTHER__BLOCK__KEY', which is outside its own prefix \
                    'TEST__DECLARED__'"
    )]
    async fn a_declared_only_block_is_admitted_the_same_way() {
        let mut ctx = TestContext::new().await;
        ctx.register_block_info(
            "test/declared",
            BlockInfo::new("test/declared", "0.0.1", "probe@v1", "foreign key").config_keys(vec![
                wafer_run::ConfigVar::new(FOREIGN_PROBE_KEY, "foreign", ""),
            ]),
        );
    }

    /// A grant boot rejects is refused at registration, with the runtime's
    /// own `GrantsRejected` text: the runtime leaves it out of the grant set
    /// and refuses to start, so a fixture that dropped it silently would run
    /// a deployment production never boots.
    #[tokio::test]
    #[should_panic(
        expected = "the runtime refuses to boot: 1 typed grant(s) rejected:\n  - block \
                               `test/declared`: typed Network grants may only be declared by the \
                               admin block"
    )]
    async fn a_block_declaring_a_typed_grant_it_may_not_is_not_registered() {
        let mut ctx = TestContext::new().await;
        ctx.register_block(
            "test/declared",
            Arc::new(Declares(
                BlockInfo::new("test/declared", "0.0.1", "probe@v1", "typed grant")
                    .grants(vec![ResourceGrant::read("*", "https://example.com")
                        .typed(wafer_run::ResourceType::Network)]),
            )),
        );
    }

    /// The same refusal for a namespace grant on a table another block owns,
    /// through the declared-only path sandbox guests take.
    #[tokio::test]
    #[should_panic(
        expected = "the runtime refuses to boot: 1 typed grant(s) rejected:\n  - block \
                               `test/declared`: resource `other__block__rows` is owned by \
                               `other/block`, not by declaring block"
    )]
    async fn a_declared_only_block_granting_what_it_does_not_own_is_not_registered() {
        let mut ctx = TestContext::new().await;
        ctx.register_block_info(
            "test/declared",
            BlockInfo::new("test/declared", "0.0.1", "probe@v1", "foreign grant")
                .grants(vec![ResourceGrant::read("*", "other__block__rows")]),
        );
    }

    /// The fixture's own frame is test setup: it writes and reads any table,
    /// as the admin block may. It is reached only through `fixture()`: what a
    /// constructor hands back is nobody's code, and is refused.
    #[tokio::test]
    async fn the_fixture_frame_is_authorized_as_the_admin_block() {
        let unframed = TestContext::with_auth().await;
        let err = db::list(&unframed, "wafer_run__auth__users", &ListOptions::default())
            .await
            .expect_err("an unframed context is nobody's code, and is refused");
        assert_eq!(err.code, ErrorCode::PermissionDenied, "{err:?}");

        let ctx = unframed.fixture();
        let res = db::list(&ctx, "wafer_run__auth__users", &ListOptions::default())
            .await
            .expect("the fixture frame reads any table");
        assert_eq!(res.records.len(), 0);
        assert!(
            ctx.running_as("test/block-x")
                .fixture()
                .check_resource_access(
                    "wafer_run__auth__users",
                    wafer_run::ResourceType::Db,
                    wafer_block::ResourceAccess::Read,
                )
                .is_ok(),
            "`fixture()` is the fixture frame again"
        );
    }

    /// A block's own top-level frame has no caller, and the runtime refuses
    /// an unattributed resource check; only the frames it calls into carry
    /// it as their caller.
    #[tokio::test]
    async fn a_top_level_block_frame_has_no_caller_to_authorize() {
        let ctx = TestContext::with_auth().await.running_as("wafer-run/auth");
        assert_eq!(ctx.caller_id(), None);
        assert!(ctx
            .check_resource_access(
                "wafer_run__auth__users",
                wafer_run::ResourceType::Db,
                wafer_block::ResourceAccess::Read,
            )
            .is_err());
        db::list(&ctx, "wafer_run__auth__users", &ListOptions::default())
            .await
            .expect("the database it calls is called by the block, which owns the table");
    }
}

/// The events `tracing` emitted on this thread while a [`CapturedEvents`]
/// was installed, for a test that asserts an operator-facing log line.
///
/// Installed as the thread's default subscriber (`tracing::subscriber::
/// set_default`), so it sees only what the installing thread emits — which is
/// what a `#[tokio::test]` handler call on the default current-thread runtime
/// runs on — and never another test's events. Written against `tracing`'s own
/// `Subscriber` trait because the crate has no `tracing-subscriber`
/// dependency to borrow a layer from.
#[cfg(test)]
pub struct CapturedEvents {
    events: Arc<Mutex<Vec<CapturedEvent>>>,
    _guard: tracing::subscriber::DefaultGuard,
}

/// One event [`CapturedEvents`] saw: its level and every field it carried,
/// the format string's rendering included under `"message"`.
#[cfg(test)]
#[derive(Debug, Clone)]
pub struct CapturedEvent {
    pub level: tracing::Level,
    pub fields: std::collections::BTreeMap<String, String>,
}

#[cfg(test)]
impl CapturedEvents {
    /// Start capturing; capture stops when the value is dropped.
    pub fn install() -> Self {
        // `tracing-core` caches each callsite's interest the first time the
        // callsite is hit. While at most one dispatcher is registered it asks
        // only the HITTING thread's default, so another test thread that hits
        // a callsite first caches "never" for it — and this thread's
        // recorder then never sees the event. A second dispatcher kept alive
        // for the whole process makes every registration ask all of them;
        // it answers "sometimes", which leaves the decision to each event's
        // own thread.
        static UNDECIDED: std::sync::OnceLock<tracing::Dispatch> = std::sync::OnceLock::new();
        UNDECIDED.get_or_init(|| tracing::Dispatch::new(Undecided));

        let events = Arc::new(Mutex::new(Vec::new()));
        let guard = tracing::subscriber::set_default(EventRecorder(Arc::clone(&events)));
        Self {
            events,
            _guard: guard,
        }
    }

    /// Every event captured so far, in emission order.
    pub fn events(&self) -> Vec<CapturedEvent> {
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

#[cfg(test)]
struct EventRecorder(Arc<Mutex<Vec<CapturedEvent>>>);

/// Records nothing, and answers every callsite "sometimes" (see
/// [`CapturedEvents::install`]).
#[cfg(test)]
struct Undecided;

#[cfg(test)]
impl tracing::Subscriber for Undecided {
    fn register_callsite(
        &self,
        _: &'static tracing::Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        tracing::subscriber::Interest::sometimes()
    }

    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        false
    }

    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

    fn event(&self, _: &tracing::Event<'_>) {}

    fn enter(&self, _: &tracing::span::Id) {}

    fn exit(&self, _: &tracing::span::Id) {}
}

#[cfg(test)]
impl tracing::Subscriber for EventRecorder {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        struct Fields<'a>(&'a mut std::collections::BTreeMap<String, String>);
        impl tracing::field::Visit for Fields<'_> {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                self.0
                    .insert(field.name().to_string(), format!("{value:?}"));
            }

            fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
                self.0.insert(field.name().to_string(), value.to_string());
            }
        }
        let mut fields = std::collections::BTreeMap::new();
        event.record(&mut Fields(&mut fields));
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(CapturedEvent {
                level: *event.metadata().level(),
                fields,
            });
    }

    fn enter(&self, _: &tracing::span::Id) {}

    fn exit(&self, _: &tracing::span::Id) {}
}
