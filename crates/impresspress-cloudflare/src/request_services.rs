//! Request-current Cloudflare service injection.
//!
//! A cached [`wafer_run::Wafer`] must not retain D1, KV, R2, or other
//! request-derived Workers handles. The service blocks inside that Wafer
//! therefore hold the stateless forwarding proxies in this module. Every poll
//! of a request's scoped future installs the request's concrete
//! [`RequestServices`] bundle, and takes it out again before yielding back to
//! the Workers executor.
//!
//! The poll boundary is the important part. Workers may interleave fetch
//! events whenever one request returns `Poll::Pending`; a plain
//! thread-local "current request" set once around an `.await` would therefore
//! be incorrect. Re-entering the scope on every poll gives nested awaited
//! service calls and lazy block Init the right bundle while allowing another
//! request to use the isolate between polls.
//!
//! This relies on Cloudflare's single-threaded wasm32 isolate execution model.
//! It is adapter-local: native and VM runtimes continue to inject their normal
//! long-lived services directly.
//!
//! # Root and nested scopes
//!
//! Workers can stop a request mid-poll (its CPU limit), and a stop runs no
//! destructor: the scope guard that would have taken the bundle out never
//! runs, and the dead request's bundle stays current. So what a poll puts back
//! when it ends is decided by the constructor, at the call site:
//!
//! - Roots put back nothing. A root is the outermost scope of a future the
//!   adapter hands the executor, and nothing else, and it roots **both**
//!   request cells: this one and `impresspress_core::after_response`'s.
//!   - The `fetch` future: `dispatch` (`lib.rs`) enters [`scope_root`] with
//!     `after_response::scope_root` inside it, installing the request's
//!     bundle and its after-response work.
//!   - Every other root runs no dispatch, so it takes
//!     [`scope_root_outside_dispatch`]: the bundle, and no after-response
//!     work, so what it defers or guards is refused rather than queued into
//!     a request. These are each deferred task, which runs in the
//!     `ctx.wait_until` future that carries the request's after-response
//!     work (`dispatch`); the `scheduled` future's sweep (`run_sweep`,
//!     `lib.rs`); and each spawned producer ([`wrap_spawned`]).
//!
//!   The executor never polls one task inside another's poll, so nothing a
//!   root's poll is nested in has a request current. A root that finds one
//!   has found a hard-stopped request, logs `ip-root-found-stale` (see
//!   [`report_root_found_stale`]), and clears the slot when its poll ends.
//!   It does not free the request: the stopped task's future still owns it,
//!   and that task is never dropped.
//! - [`scope`] and [`scope_sync`] put back what they found, so they nest.
//!   The runtime build's scopes (`runtime_build`) are nested although they
//!   sit in root position, on the `fetch` and `scheduled` futures before
//!   their root: a misplaced root is worse than a misplaced nested scope.
//!   After a stop they find the dead bundle and put it back between their
//!   polls until the next root clears it. A build stopped mid-Init, the
//!   likeliest stop on a CPU-limited plan, leaves exactly that.
//!   `/_deploy/init` and `/_deploy/prepare` reach no root on their own
//!   future at all. A root where a nested scope belongs would empty the
//!   outer scope's bundle in the middle of its poll. When in doubt, nest.
//!
//! Nesting cannot be counted at run time instead: a depth counter would be
//! stranded by the same stop. The `ctx.wait_until` futures other than the
//! deferred tasks hold no scope: the audit-row write uses a D1 handle of its
//! own, and the final-write wait and the config-version retry reach no
//! request service and defer nothing.

use std::{
    collections::{BTreeMap, HashMap},
    future::Future,
    pin::Pin,
    rc::Rc,
    sync::Arc,
    task::{Context as TaskContext, Poll},
    time::Duration,
};

pub(crate) use impresspress_core::prepared_plan::{
    RELEASE_ASSET_ID_VAR, RELEASE_ASSET_MANIFEST_VAR, RELEASE_ASSET_PREFIX_VAR,
};
use impresspress_core::{
    release_inventory::{is_normalized_logical_key, ReleaseInventory, RELEASES_ROOT},
    IsolateCell,
};
use wafer_block::{
    db::{Filter, ListOptions as DbListOptions},
    ConfigVar, InputStream, OutputStream,
};
use wafer_core::interfaces::{
    config::service::ConfigService,
    crypto::service::{CryptoError, CryptoService},
    database::service::{
        AggregateSpec, CapGuard, Column, DatabaseError, DatabaseService, GuardedInsert,
        GuardedUpdate, RawStatement, Record, RecordList, Table, TransactionError, UpsertSpec,
        WriteOp, WriteOutcome,
    },
    logger::service::{Field, LoggerService},
    network::service::{
        NetworkError, NetworkService, Request as NetworkRequest, Response as NetworkResponse,
        ResponseHead,
    },
    storage::service::{
        FolderInfo, ListOptions as StorageListOptions, ObjectInfo, ObjectList, StorageError,
        StorageService,
    },
};
use wafer_run::{ConfigError, ConfigSource, EnvBlockConfig};

/// Pure, Worker-version-bound release identity. This contains no R2 handle
/// and no key inventory — the inventory itself is fetched lazily (and
/// digest-verified) from `{prefix}/keys.json` by [`impresspress_core::release_inventory::load_release_inventory`].
#[derive(Debug)]
pub(crate) struct ReleaseAssetIdentity {
    id: String,
    prefix: String,
    manifest_key: String,
    keys_sha256: String,
}

thread_local! {
    /// Parsed pure Worker-version data only; no Env, binding, or I/O object is
    /// retained across requests.
    ///
    /// `IsolateCell` rather than `RefCell` — a borrow flag stranded by a
    /// Cloudflare hard-stop would trap the next request that touches it. See
    /// `impresspress_core::isolate_cell`.
    static RELEASE_IDENTITY_CACHE: IsolateCell<(String, Arc<ReleaseAssetIdentity>)> =
        const { IsolateCell::new() };
}

impl ReleaseAssetIdentity {
    /// Parse the release-routing contract out of an already-captured
    /// environment. The four vars are read once, by
    /// [`CfEnvironment::capture`](crate::environment::CfEnvironment::capture);
    /// this is the only thing that interprets them.
    pub(crate) fn from_environment(
        environment: &crate::environment::CfEnvironment,
    ) -> Result<Option<Arc<Self>>, String> {
        let vars = environment.release_asset_vars();
        let (id, prefix, manifest_key, keys_sha256) = (
            vars.id.map(str::to_string),
            vars.prefix.map(str::to_string),
            vars.manifest_key.map(str::to_string),
            vars.keys_sha256.map(str::to_string),
        );

        if id.is_none() && prefix.is_none() && manifest_key.is_none() && keys_sha256.is_none() {
            return Ok(None);
        }
        let required = |name: &str, value: Option<String>| {
            value.ok_or_else(|| format!("release asset identity is incomplete: missing {name}"))
        };
        let id = required(RELEASE_ASSET_ID_VAR, id)?;
        let prefix = required(RELEASE_ASSET_PREFIX_VAR, prefix)?;
        let manifest_key = required(RELEASE_ASSET_MANIFEST_VAR, manifest_key)?;
        let keys_sha256 = required(
            impresspress_core::RELEASE_ASSET_KEYS_SHA256_VAR,
            keys_sha256,
        )?;

        let cache_key = format!("{id}\n{prefix}\n{manifest_key}\n{keys_sha256}");
        if let Some(identity) = RELEASE_IDENTITY_CACHE.with(|slot| {
            slot.get()
                .filter(|(key, _)| key == &cache_key)
                .map(|(_, identity)| identity)
        }) {
            return Ok(Some(identity));
        }

        let identity = Arc::new(Self::parse(id, prefix, manifest_key, keys_sha256)?);
        RELEASE_IDENTITY_CACHE.with(|slot| slot.set((cache_key, identity.clone())));
        Ok(Some(identity))
    }

    fn parse(
        id: String,
        prefix: String,
        manifest_key: String,
        keys_sha256: String,
    ) -> Result<Self, String> {
        if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("asset id must be a 64-character hexadecimal SHA-256".into());
        }
        let expected_prefix = format!("{RELEASES_ROOT}/{}", id.to_ascii_lowercase());
        if prefix != expected_prefix {
            return Err(format!(
                "asset prefix must be exactly {expected_prefix:?}, got {prefix:?}"
            ));
        }
        let expected_manifest = format!("{prefix}/manifest.json");
        if manifest_key != expected_manifest {
            return Err(format!(
                "asset manifest must be exactly {expected_manifest:?}, got {manifest_key:?}"
            ));
        }
        let hex = keys_sha256.strip_prefix("sha256:").unwrap_or_default();
        if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("keys digest must be sha256:<64 hex>".into());
        }
        Ok(Self {
            id,
            prefix,
            manifest_key,
            keys_sha256,
        })
    }

    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    pub(crate) fn manifest_key(&self) -> &str {
        &self.manifest_key
    }

    pub(crate) fn keys_sha256(&self) -> &str {
        &self.keys_sha256
    }

    pub(crate) fn keys_location(&self) -> (&str, &str) {
        (self.prefix.as_str(), "keys.json")
    }
}

/// A release identity paired with its loaded, digest-verified key inventory
/// — everything needed to answer membership, physical-key, and
/// folder-management questions for one request.
pub(crate) struct LoadedRelease {
    pub(crate) identity: Arc<ReleaseAssetIdentity>,
    pub(crate) inventory: Arc<ReleaseInventory>,
}

impl LoadedRelease {
    pub(crate) fn physical_read_location(
        &self,
        folder: &str,
        key: &str,
    ) -> Option<(String, String)> {
        let logical = joined_logical_key(folder, key)?;
        if !self.inventory.contains(&logical) {
            return None;
        }
        let physical = format!("{}/{logical}", self.identity.prefix);
        let (physical_folder, physical_key) = physical.rsplit_once('/')?;
        Some((physical_folder.to_string(), physical_key.to_string()))
    }

    pub(crate) fn physical_object_key(&self, logical_key: &str) -> Option<String> {
        (is_normalized_logical_key(logical_key) && self.inventory.contains(logical_key))
            .then(|| format!("{}/{logical_key}", self.identity.prefix))
    }

    pub(crate) fn manages_folder(&self, folder: &str) -> bool {
        self.inventory.manages_folder(folder)
    }
}

fn joined_logical_key(folder: &str, key: &str) -> Option<String> {
    if folder.is_empty() || key.is_empty() {
        return None;
    }
    let logical = format!("{folder}/{key}");
    is_normalized_logical_key(&logical).then_some(logical)
}

/// Concrete services derived from exactly one Workers request `Env`.
///
/// Every field is optional only so the forwarding proxies can fail closed and
/// unit tests can construct marker-only bundles. Production construction
/// always fills every service.
pub(crate) struct RequestServices {
    database: Option<Arc<dyn DatabaseService>>,
    storage: Option<Arc<dyn StorageService>>,
    config: Option<Arc<dyn ConfigService>>,
    crypto: Option<Arc<dyn CryptoService>>,
    network: Option<Arc<dyn NetworkService>>,
    logger: Option<Arc<dyn LoggerService>>,
    config_source: Option<Arc<dyn ConfigSource>>,
    release_assets: Result<Option<Arc<ReleaseAssetIdentity>>, String>,
    /// This bundle's number in the Wasm instance, for the
    /// `ip-root-found-stale` line: [`RequestServices::new`] numbers bundles
    /// from 1 in the order it builds them. Tests set their own.
    marker: usize,
}

thread_local! {
    /// The marker the next [`RequestServices::new`] bundle takes.
    static NEXT_MARKER: std::cell::Cell<usize> = const { std::cell::Cell::new(1) };
}

fn next_marker() -> usize {
    NEXT_MARKER.with(|next| {
        let marker = next.get();
        next.set(marker.wrapping_add(1));
        marker
    })
}

impl RequestServices {
    #[expect(
        clippy::too_many_arguments,
        reason = "one argument per service handle the caller already owns; a bundle \
                  struct would be this list with a name on it"
    )]
    pub(crate) fn new(
        environment: &crate::environment::CfEnvironment,
        database: Arc<dyn DatabaseService>,
        storage: Arc<dyn StorageService>,
        config: Arc<dyn ConfigService>,
        crypto: Arc<dyn CryptoService>,
        network: Arc<dyn NetworkService>,
        logger: Arc<dyn LoggerService>,
        config_source: Arc<dyn ConfigSource>,
    ) -> Rc<Self> {
        Rc::new(Self {
            database: Some(database),
            storage: Some(storage),
            config: Some(config),
            crypto: Some(crypto),
            network: Some(network),
            logger: Some(logger),
            config_source: Some(config_source),
            release_assets: ReleaseAssetIdentity::from_environment(environment),
            marker: next_marker(),
        })
    }

    #[cfg(test)]
    pub(crate) fn marker(marker: usize) -> Rc<Self> {
        Rc::new(Self {
            database: None,
            storage: None,
            config: None,
            crypto: None,
            network: None,
            logger: None,
            config_source: None,
            release_assets: Ok(None),
            marker,
        })
    }

    /// A marker bundle whose crypto service is `crypto`, so a test can tell a
    /// call that reached its request's services from one refused for running
    /// outside every request scope.
    #[cfg(test)]
    pub(crate) fn marker_with_crypto(marker: usize, crypto: Arc<dyn CryptoService>) -> Rc<Self> {
        Rc::new(Self {
            database: None,
            storage: None,
            config: None,
            crypto: Some(crypto),
            network: None,
            logger: None,
            config_source: None,
            release_assets: Ok(None),
            marker,
        })
    }

    /// A marker bundle whose database service is `database`.
    #[cfg(test)]
    pub(crate) fn marker_with_database(
        marker: usize,
        database: Arc<dyn DatabaseService>,
    ) -> Rc<Self> {
        Rc::new(Self {
            database: Some(database),
            storage: None,
            config: None,
            crypto: None,
            network: None,
            logger: None,
            config_source: None,
            release_assets: Ok(None),
            marker,
        })
    }

    /// `storage` backs the lazy R2 `keys.json` fetch that `ScopedStorageService`
    /// now performs before it can answer any membership question — tests that
    /// exercise the release guards must supply a fake serving the fixed
    /// digest declared in `release_assets`.
    #[cfg(test)]
    fn marker_with_release(
        marker: usize,
        release_assets: ReleaseAssetIdentity,
        storage: Arc<dyn StorageService>,
    ) -> Rc<Self> {
        Rc::new(Self {
            database: None,
            storage: Some(storage),
            config: None,
            crypto: None,
            network: None,
            logger: None,
            config_source: None,
            release_assets: Ok(Some(Arc::new(release_assets))),
            marker,
        })
    }
}

thread_local! {
    /// The request whose future is currently being polled.
    ///
    /// `IsolateCell` rather than `RefCell`: this is the single hottest piece
    /// of isolate state — entered and left on every poll of every request,
    /// and read by every service proxy — so a borrow flag stranded by a
    /// Cloudflare hard-stop would trap the very next scoped poll, of whichever
    /// request or task it is (see `impresspress_core::isolate_cell`). With no
    /// borrow flag, a stop inside the cell's own `replace` leaves an empty
    /// slot, which the proxies below report as "used outside request poll
    /// scope". A stop anywhere else in a scoped poll leaves the stopped
    /// request's bundle here, because its scope guard never runs; the next
    /// [`scope_root`] poll clears it (see the module doc).
    static CURRENT: IsolateCell<Rc<RequestServices>> = const { IsolateCell::new() };
}

fn current() -> Option<Rc<RequestServices>> {
    CURRENT.with(IsolateCell::get)
}

/// The marker of the bundle in scope, for a test outside this module that
/// needs to know which request's services a future ran under.
#[cfg(test)]
pub(crate) fn current_marker() -> Option<usize> {
    current().map(|services| services.marker)
}

/// Request-current immutable release identity, available only while the
/// request's dispatch future is being polled.
pub(crate) fn current_release_asset_identity(
) -> Result<Option<Arc<ReleaseAssetIdentity>>, StorageError> {
    match current().map(|services| services.release_assets.clone()) {
        Some(Ok(identity)) => Ok(identity),
        Some(Err(error)) => Err(StorageError::Internal(format!(
            "invalid release asset identity: {error}"
        ))),
        None => Ok(None),
    }
}

/// What one scoped poll puts back when it ends.
struct ScopeGuard {
    restore: Option<Rc<RequestServices>>,
}

impl ScopeGuard {
    /// A nested scope: put back what the poll found.
    fn enter(services: Rc<RequestServices>) -> Self {
        let previous = CURRENT.with(|slot| slot.replace(Some(services)));
        Self { restore: previous }
    }

    /// A root scope: put back nothing, and report a bundle found current,
    /// which only a hard-stopped request can have left there.
    fn enter_root(services: Rc<RequestServices>) -> Self {
        let found = CURRENT.with(|slot| slot.replace(Some(services)));
        if let Some(stale) = found {
            report_root_found_stale(REQUEST_SERVICES_CELL, Some(stale.marker));
        }
        Self { restore: None }
    }
}

impl Drop for ScopeGuard {
    fn drop(&mut self) {
        CURRENT.with(|slot| slot.replace(self.restore.take()));
    }
}

/// The `ip-root-found-stale` cell name for [`CURRENT`].
const REQUEST_SERVICES_CELL: &str = "request-services";
/// The `ip-root-found-stale` cell name for
/// `impresspress_core::after_response`'s current request.
const AFTER_RESPONSE_CELL: &str = "after-response";

/// `ip-root-found-stale <cell> <marker> <instance>`: a root scope found
/// `cell` holding a request on entry. The marker is the dead
/// [`RequestServices`] bundle's, or `-` for a cell whose value carries none.
/// The instance is `<isolate>/<wasm>/<generation>`, as the `x-ip-instance`
/// header carries it, once this Wasm instance has served a traced request
/// (`executor_trace`), and `-` otherwise; it makes the line self-attributing
/// where the tail's grouping of lines by invocation is unclear (a spawned
/// producer's task, the untraced `scheduled` path).
///
/// The Worker's executor diagnostic lines share one shape: an `ip-` event
/// name, then space-separated fields, `-` for a field not known. This one is
/// not behind the `IMPRESSPRESS_EXECUTOR_TRACE` var that gates the
/// per-request lines: it is logged only after a request was hard-stopped,
/// which an operator needs to see on any deployment, and it costs nothing
/// otherwise. It lands in the log of the invocation whose root found it,
/// beside that invocation's own lines.
fn root_found_stale_line(cell: &str, marker: Option<usize>, instance: Option<&str>) -> String {
    let marker = marker.map_or_else(|| "-".to_string(), |marker| marker.to_string());
    let instance = instance.unwrap_or("-");
    format!("ip-root-found-stale {cell} {marker} {instance}")
}

/// Log [`root_found_stale_line`].
pub(crate) fn report_root_found_stale(cell: &str, marker: Option<usize>) {
    let instance = crate::executor_trace::traced_instance();
    worker::console_log!(
        "{}",
        root_found_stale_line(cell, marker, instance.as_deref())
    );
}

/// The `found_stale` hook of the adapter's
/// `impresspress_core::after_response::scope_root`.
pub(crate) fn after_response_found_stale() {
    report_root_found_stale(AFTER_RESPONSE_CELL, None);
}

/// Run synchronous builder/start work with the same request-current service
/// selection used by async dispatch. Nested: it puts back what it found.
pub(crate) fn scope_sync<T>(services: Rc<RequestServices>, f: impl FnOnce() -> T) -> T {
    let _guard = ScopeGuard::enter(services);
    f()
}

/// Which constructor made a [`ScopedFuture`]. See the module doc.
#[derive(Clone, Copy)]
enum ScopeKind {
    Nested,
    Root,
}

/// A future that re-enters its request service scope on every poll.
///
/// `F` may be unsized so a task that is already boxed — a
/// [`wafer_block::spawn::SpawnedTask`] — is scoped without a second box.
pub(crate) struct ScopedFuture<F: ?Sized> {
    services: Rc<RequestServices>,
    kind: ScopeKind,
    inner: Pin<Box<F>>,
}

impl<F: Future + ?Sized> Future for ScopedFuture<F> {
    type Output = F::Output;

    fn poll(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Self::Output> {
        let services = self.services.clone();
        let _guard = match self.kind {
            ScopeKind::Nested => ScopeGuard::enter(services),
            ScopeKind::Root => ScopeGuard::enter_root(services),
        };
        self.inner.as_mut().poll(cx)
    }
}

/// A nested request scope: each poll puts back the bundle it found. See the
/// module doc for where a [`scope_root`] belongs instead.
pub(crate) fn scope<F: Future>(services: Rc<RequestServices>, future: F) -> ScopedFuture<F> {
    ScopedFuture {
        services,
        kind: ScopeKind::Nested,
        inner: Box::pin(future),
    }
}

/// The root request scope of a future the adapter hands the executor: each
/// poll puts back nothing, and a bundle found current on entry is reported
/// and dropped. See the module doc for the call sites.
pub(crate) fn scope_root<F: Future>(services: Rc<RequestServices>, future: F) -> ScopedFuture<F> {
    ScopedFuture {
        services,
        kind: ScopeKind::Root,
        inner: Box::pin(future),
    }
}

/// The root of a future the adapter hands the executor that runs no
/// request's dispatch (see the module doc): `services` current, and no
/// after-response work, in both cells as roots. The services root is the
/// outer one, so each cell is reported once, by its own root.
pub(crate) fn scope_root_outside_dispatch<F: Future>(
    services: Rc<RequestServices>,
    future: F,
) -> ScopedFuture<impresspress_core::after_response::Scoped<F>> {
    scope_root(
        services,
        impresspress_core::after_response::scope_root_outside_request(
            future,
            after_response_found_stale,
        ),
    )
}

/// [`scope_root_outside_dispatch`] for a task [`wrap_spawned`] receives
/// already boxed. Each layer takes the box it is given, so the two roots add
/// one box each, and the task's own box is not boxed again.
#[cfg(all(target_arch = "wasm32", not(target_os = "wasi")))]
fn scope_root_outside_dispatch_spawned(
    services: Rc<RequestServices>,
    task: wafer_block::spawn::SpawnedTask,
) -> wafer_block::spawn::SpawnedTask {
    let outside_request: wafer_block::spawn::SpawnedTask = Box::pin(
        impresspress_core::after_response::scope_root_outside_request_boxed(
            task,
            after_response_found_stale,
        ),
    );
    Box::pin(ScopedFuture {
        services,
        kind: ScopeKind::Root,
        inner: outside_request,
    })
}

/// Leave `services` current as a request hard-stopped mid-poll leaves its
/// bundle: a scope guard whose `Drop` never runs.
#[cfg(test)]
pub(crate) fn strand_for_test(services: Rc<RequestServices>) {
    std::mem::forget(ScopeGuard::enter(services));
}

/// Take whatever bundle is current, so a test that stranded one leaves the
/// thread as it found it.
#[cfg(test)]
pub(crate) fn clear_for_test() {
    CURRENT.with(IsolateCell::clear);
}

/// The isolate's spawn wrapper (`wafer_block::spawn::set_spawn_wrapper`,
/// installed by [`crate::init_isolate`]): a task spawned while a request's
/// scope is current — a producer a block hands to
/// `OutputStream::from_producer` inside `handle`, or one a service call
/// spawns — runs every poll inside that request's scope, as the dispatch
/// future does.
///
/// Without it such a task is polled from the JS microtask queue, outside
/// every scope, and each proxy call it makes is refused: the storage block's
/// access-log row (written from its forwarding producer), a streamed reply's
/// final database write, a producer's log lines. A task spawned outside any
/// request (nothing current) is passed through bare, so a proxy call from it
/// is still refused loudly.
///
/// The scope is a root ([`scope_root_outside_dispatch_spawned`]): `spawn_producer`
/// always hands the task to `spawn_local`, so it is a task of its own and
/// nothing else's poll encloses it.
///
/// Only the services scope is carried. A producer's
/// `impresspress_core::deferred::defer` stays refused, and a final-write
/// guard it takes (`impresspress_core::after_response::final_write`) is a
/// silent no-op: when a producer runs relative to the request's head and its
/// after-response drain is a race it cannot see. A producer finishes its
/// work before its terminal; after it, nothing keeps the invocation alive for
/// it, unless its block took a final-write guard in `handle`, while the
/// request was dispatched.
#[cfg(all(target_arch = "wasm32", not(target_os = "wasi")))]
pub(crate) fn wrap_spawned(
    task: wafer_block::spawn::SpawnedTask,
) -> wafer_block::spawn::SpawnedTask {
    match current() {
        Some(services) => scope_root_outside_dispatch_spawned(services, task),
        None => task,
    }
}

fn database() -> Result<Arc<dyn DatabaseService>, DatabaseError> {
    current()
        .and_then(|services| services.database.clone())
        .ok_or_else(|| {
            DatabaseError::Internal(
                "Cloudflare database service used outside request poll scope".into(),
            )
        })
}

fn storage() -> Result<Arc<dyn StorageService>, StorageError> {
    current()
        .and_then(|services| services.storage.clone())
        .ok_or_else(|| {
            StorageError::Internal(
                "Cloudflare storage service used outside request poll scope".into(),
            )
        })
}

fn crypto() -> Result<Arc<dyn CryptoService>, CryptoError> {
    current()
        .and_then(|services| services.crypto.clone())
        .ok_or_else(|| {
            CryptoError::Other("Cloudflare crypto service used outside request poll scope".into())
        })
}

fn network() -> Result<Arc<dyn NetworkService>, NetworkError> {
    current()
        .and_then(|services| services.network.clone())
        .ok_or_else(|| {
            NetworkError::Other("Cloudflare network service used outside request poll scope".into())
        })
}

/// The database service of the request being polled, looked up per call.
/// Every answer, errors included, is that service's unchanged — so a write
/// that duplicates a key is the `AlreadyExists` the `DatabaseService`
/// contract names (see `database.rs`, "A taken key").
pub(crate) struct ScopedDatabaseService;

impl ScopedDatabaseService {
    fn current(&self) -> Result<Arc<dyn DatabaseService>, DatabaseError> {
        database()
    }
}

#[wafer_block::wafer_async_trait]
impl DatabaseService for ScopedDatabaseService {
    async fn get(&self, collection: &str, id: &str) -> Result<Record, DatabaseError> {
        self.current()?.get(collection, id).await
    }

    async fn list(
        &self,
        collection: &str,
        opts: &DbListOptions,
    ) -> Result<RecordList, DatabaseError> {
        self.current()?.list(collection, opts).await
    }

    async fn create(
        &self,
        collection: &str,
        data: HashMap<String, serde_json::Value>,
    ) -> Result<Record, DatabaseError> {
        self.current()?.create(collection, data).await
    }

    async fn update(
        &self,
        collection: &str,
        id: &str,
        data: HashMap<String, serde_json::Value>,
    ) -> Result<Record, DatabaseError> {
        self.current()?.update(collection, id, data).await
    }

    async fn delete(&self, collection: &str, id: &str) -> Result<(), DatabaseError> {
        self.current()?.delete(collection, id).await
    }

    async fn count(&self, collection: &str, filters: &[Filter]) -> Result<i64, DatabaseError> {
        self.current()?.count(collection, filters).await
    }

    async fn sum(
        &self,
        collection: &str,
        field: &str,
        filters: &[Filter],
    ) -> Result<f64, DatabaseError> {
        self.current()?.sum(collection, field, filters).await
    }

    async fn query_raw(
        &self,
        query: &str,
        args: &[serde_json::Value],
    ) -> Result<Vec<Record>, DatabaseError> {
        self.current()?.query_raw(query, args).await
    }

    async fn exec_raw(
        &self,
        query: &str,
        args: &[serde_json::Value],
    ) -> Result<i64, DatabaseError> {
        self.current()?.exec_raw(query, args).await
    }

    async fn exec_raw_transaction(
        &self,
        statements: &[RawStatement],
    ) -> Result<Vec<i64>, TransactionError> {
        self.current()
            .map_err(TransactionError::whole)?
            .exec_raw_transaction(statements)
            .await
    }

    async fn delete_where(
        &self,
        collection: &str,
        filters: &[Filter],
    ) -> Result<(), DatabaseError> {
        self.current()?.delete_where(collection, filters).await
    }

    async fn delete_where_count(
        &self,
        collection: &str,
        filters: &[Filter],
    ) -> Result<i64, DatabaseError> {
        self.current()?
            .delete_where_count(collection, filters)
            .await
    }

    async fn take_where(
        &self,
        collection: &str,
        filters: &[Filter],
    ) -> Result<Vec<Record>, DatabaseError> {
        self.current()?.take_where(collection, filters).await
    }

    async fn update_where(
        &self,
        collection: &str,
        filters: &[Filter],
        data: HashMap<String, serde_json::Value>,
    ) -> Result<(), DatabaseError> {
        self.current()?
            .update_where(collection, filters, data)
            .await
    }

    async fn update_where_count(
        &self,
        collection: &str,
        filters: &[Filter],
        data: HashMap<String, serde_json::Value>,
    ) -> Result<i64, DatabaseError> {
        self.current()?
            .update_where_count(collection, filters, data)
            .await
    }

    async fn increment_field_where(
        &self,
        collection: &str,
        col: &str,
        delta: i64,
        filters: &[Filter],
    ) -> Result<i64, DatabaseError> {
        self.current()?
            .increment_field_where(collection, col, delta, filters)
            .await
    }

    async fn upsert(
        &self,
        collection: &str,
        spec: UpsertSpec,
    ) -> Result<Option<Record>, DatabaseError> {
        self.current()?.upsert(collection, spec).await
    }

    async fn create_many(
        &self,
        collection: &str,
        rows: Vec<HashMap<String, serde_json::Value>>,
    ) -> Result<i64, DatabaseError> {
        self.current()?.create_many(collection, rows).await
    }

    async fn batch(&self, ops: Vec<WriteOp>) -> Result<Vec<WriteOutcome>, DatabaseError> {
        self.current()?.batch(ops).await
    }

    async fn insert_guarded(
        &self,
        collection: &str,
        data: HashMap<String, serde_json::Value>,
        guards: &[CapGuard],
    ) -> Result<GuardedInsert, DatabaseError> {
        self.current()?
            .insert_guarded(collection, data, guards)
            .await
    }

    async fn update_guarded(
        &self,
        collection: &str,
        filters: &[Filter],
        data: HashMap<String, serde_json::Value>,
        guards: &[CapGuard],
    ) -> Result<GuardedUpdate, DatabaseError> {
        self.current()?
            .update_guarded(collection, filters, data, guards)
            .await
    }

    async fn aggregate(
        &self,
        collection: &str,
        spec: AggregateSpec,
    ) -> Result<Vec<Record>, DatabaseError> {
        self.current()?.aggregate(collection, spec).await
    }

    async fn ensure_schema_table(&self, table: &Table) -> Result<(), DatabaseError> {
        self.current()?.ensure_schema_table(table).await
    }

    async fn ensure_schema_tables(&self, tables: &[Table]) -> Result<(), DatabaseError> {
        self.current()?.ensure_schema_tables(tables).await
    }

    async fn schema_table_exists(&self, name: &str) -> Result<bool, DatabaseError> {
        self.current()?.schema_table_exists(name).await
    }

    async fn schema_columns(&self, table: &str) -> Result<Vec<String>, DatabaseError> {
        self.current()?.schema_columns(table).await
    }

    async fn schema_drop_table(&self, name: &str) -> Result<(), DatabaseError> {
        self.current()?.schema_drop_table(name).await
    }

    async fn schema_add_column(&self, table: &str, column: &Column) -> Result<(), DatabaseError> {
        self.current()?.schema_add_column(table, column).await
    }

    async fn catalog_tables(&self) -> Result<Vec<String>, DatabaseError> {
        self.current()?.catalog_tables().await
    }

    async fn catalog_columns(
        &self,
        table: &str,
    ) -> Result<Vec<wafer_core::interfaces::database::service::CatalogColumn>, DatabaseError> {
        self.current()?.catalog_columns(table).await
    }

    async fn catalog_row_count(&self, table: &str) -> Result<i64, DatabaseError> {
        self.current()?.catalog_row_count(table).await
    }
}

#[derive(Default)]
pub(crate) struct ScopedStorageService;

impl ScopedStorageService {
    /// Resolve and load the request-current release, digest-verifying its
    /// key inventory from R2 (isolate-cached thereafter). `Ok(None)` means no
    /// release contract is configured for this request; any other failure —
    /// unfetchable or digest-mismatched `keys.json` — is a hard error so
    /// storage reads never silently fall back to a mutable object.
    async fn current_loaded_release() -> Result<Option<LoadedRelease>, StorageError> {
        let Some(identity) = current_release_asset_identity()? else {
            return Ok(None);
        };
        let (folder, name) = identity.keys_location();
        let inventory = impresspress_core::release_inventory::load_release_inventory(
            folder,
            name,
            identity.keys_sha256(),
            storage()?.as_ref(),
        )
        .await?;
        Ok(Some(LoadedRelease {
            identity,
            inventory,
        }))
    }

    async fn read_location(folder: &str, key: &str) -> Result<(String, String), StorageError> {
        Ok(Self::current_loaded_release()
            .await?
            .and_then(|release| release.physical_read_location(folder, key))
            .unwrap_or_else(|| (folder.to_string(), key.to_string())))
    }

    async fn reject_managed_object_mutation(folder: &str, key: &str) -> Result<(), StorageError> {
        if Self::current_loaded_release()
            .await?
            .and_then(|release| release.physical_read_location(folder, key))
            .is_some()
        {
            return Err(StorageError::Internal(format!(
                "release-managed object {folder}/{key} is immutable"
            )));
        }
        Ok(())
    }

    async fn reject_managed_folder_listing(folder: &str) -> Result<(), StorageError> {
        if Self::current_loaded_release()
            .await?
            .is_some_and(|release| release.manages_folder(folder))
        {
            return Err(StorageError::Internal(format!(
                "listing release-managed folder {folder:?} is unsupported; use the release manifest"
            )));
        }
        Ok(())
    }
}

#[wafer_block::wafer_async_trait]
impl StorageService for ScopedStorageService {
    async fn put(
        &self,
        folder: &str,
        key: &str,
        data: &[u8],
        content_type: &str,
    ) -> Result<(), StorageError> {
        Self::reject_managed_object_mutation(folder, key).await?;
        storage()?.put(folder, key, data, content_type).await
    }

    async fn put_streaming(
        &self,
        folder: &str,
        key: &str,
        data: InputStream,
        content_type: &str,
    ) -> Result<(), StorageError> {
        Self::reject_managed_object_mutation(folder, key).await?;
        storage()?
            .put_streaming(folder, key, data, content_type)
            .await
    }

    async fn get(&self, folder: &str, key: &str) -> Result<(Vec<u8>, ObjectInfo), StorageError> {
        let (read_folder, read_key) = Self::read_location(folder, key).await?;
        let (bytes, mut info) = storage()?.get(&read_folder, &read_key).await?;
        info.key = key.to_string();
        Ok((bytes, info))
    }

    async fn get_streaming(
        &self,
        folder: &str,
        key: &str,
    ) -> Result<(OutputStream, ObjectInfo), StorageError> {
        let (read_folder, read_key) = Self::read_location(folder, key).await?;
        let (stream, mut info) = storage()?.get_streaming(&read_folder, &read_key).await?;
        info.key = key.to_string();
        Ok((stream, info))
    }

    async fn delete(&self, folder: &str, key: &str) -> Result<(), StorageError> {
        Self::reject_managed_object_mutation(folder, key).await?;
        storage()?.delete(folder, key).await
    }

    async fn list(
        &self,
        folder: &str,
        opts: &StorageListOptions,
    ) -> Result<ObjectList, StorageError> {
        Self::reject_managed_folder_listing(folder).await?;
        storage()?.list(folder, opts).await
    }

    async fn create_folder(&self, name: &str, public: bool) -> Result<(), StorageError> {
        storage()?.create_folder(name, public).await
    }

    async fn delete_folder(&self, name: &str) -> Result<(), StorageError> {
        storage()?.delete_folder(name).await
    }

    async fn list_folders(&self) -> Result<Vec<FolderInfo>, StorageError> {
        storage()?.list_folders().await
    }
}

#[derive(Default)]
pub(crate) struct ScopedConfigService;

impl ConfigService for ScopedConfigService {
    fn get(&self, key: &str) -> Option<String> {
        current()
            .and_then(|services| services.config.clone())
            .and_then(|service| service.get(key))
    }

    fn set(&self, key: &str, value: &str) {
        if let Some(service) = current().and_then(|services| services.config.clone()) {
            service.set(key, value);
        }
    }
}

#[derive(Default)]
pub(crate) struct ScopedCryptoService;

#[wafer_block::wafer_async_trait]
impl CryptoService for ScopedCryptoService {
    async fn hash(&self, password: &str) -> Result<String, CryptoError> {
        crypto()?.hash(password).await
    }

    async fn compare_hash(&self, password: &str, hash: &str) -> Result<(), CryptoError> {
        crypto()?.compare_hash(password, hash).await
    }

    async fn sign_for(
        &self,
        block_id: &str,
        claims: BTreeMap<String, serde_json::Value>,
        expiry: Duration,
    ) -> Result<String, CryptoError> {
        crypto()?.sign_for(block_id, claims, expiry).await
    }

    async fn verify_for(
        &self,
        block_id: &str,
        token: &str,
    ) -> Result<BTreeMap<String, serde_json::Value>, CryptoError> {
        crypto()?.verify_for(block_id, token).await
    }

    async fn random_bytes(&self, n: usize) -> Result<Vec<u8>, CryptoError> {
        crypto()?.random_bytes(n).await
    }
}

#[derive(Default)]
pub(crate) struct ScopedNetworkService;

#[wafer_block::wafer_async_trait]
impl NetworkService for ScopedNetworkService {
    async fn do_request(&self, req: &NetworkRequest) -> Result<NetworkResponse, NetworkError> {
        network()?.do_request(req).await
    }

    async fn do_request_streaming(
        &self,
        req: &NetworkRequest,
    ) -> Result<(ResponseHead, OutputStream), NetworkError> {
        network()?.do_request_streaming(req).await
    }
}

#[derive(Default)]
pub(crate) struct ScopedLoggerService;

impl ScopedLoggerService {
    fn with_logger(&self, f: impl FnOnce(&dyn LoggerService)) {
        if let Some(logger) = current().and_then(|services| services.logger.clone()) {
            f(logger.as_ref());
        }
    }
}

impl LoggerService for ScopedLoggerService {
    fn debug(&self, caller: Option<&str>, msg: &str, fields: &[Field]) {
        self.with_logger(|logger| logger.debug(caller, msg, fields));
    }

    fn info(&self, caller: Option<&str>, msg: &str, fields: &[Field]) {
        self.with_logger(|logger| logger.info(caller, msg, fields));
    }

    fn warn(&self, caller: Option<&str>, msg: &str, fields: &[Field]) {
        self.with_logger(|logger| logger.warn(caller, msg, fields));
    }

    fn error(&self, caller: Option<&str>, msg: &str, fields: &[Field]) {
        self.with_logger(|logger| logger.error(caller, msg, fields));
    }
}

#[derive(Default)]
pub(crate) struct ScopedConfigSource;

#[wafer_block::wafer_async_trait]
impl ConfigSource for ScopedConfigSource {
    async fn load_for_block(
        &self,
        block: &str,
        declared_keys: &[ConfigVar],
    ) -> Result<EnvBlockConfig, ConfigError> {
        let source = current()
            .and_then(|services| services.config_source.clone())
            .ok_or_else(|| ConfigError::Transient {
                block: block.to_string(),
                source: Box::new(std::io::Error::other(
                    "Cloudflare config source used outside request poll scope",
                )),
            })?;
        source.load_for_block(block, declared_keys).await
    }
}

pub(crate) fn database_proxy() -> Arc<dyn DatabaseService> {
    Arc::new(ScopedDatabaseService)
}

pub(crate) fn storage_proxy() -> Arc<dyn StorageService> {
    Arc::new(ScopedStorageService)
}

pub(crate) fn config_proxy() -> Arc<dyn ConfigService> {
    Arc::new(ScopedConfigService)
}

pub(crate) fn crypto_proxy() -> Arc<dyn CryptoService> {
    Arc::new(ScopedCryptoService)
}

pub(crate) fn network_proxy() -> Arc<dyn NetworkService> {
    Arc::new(ScopedNetworkService)
}

pub(crate) fn logger_proxy() -> Arc<dyn LoggerService> {
    Arc::new(ScopedLoggerService)
}

pub(crate) fn config_source_proxy() -> Arc<dyn ConfigSource> {
    Arc::new(ScopedConfigSource)
}

/// Installs a spawn wrapper for one test and puts the previous one back on
/// drop, so a test that changes it cannot leave every later test in the same
/// wasm binary running with the wrong one (`init_isolate` installs only once).
#[cfg(all(test, target_arch = "wasm32", not(target_os = "wasi")))]
pub(crate) struct SpawnWrapperGuard(Option<wafer_block::spawn::SpawnWrapper>);

#[cfg(all(test, target_arch = "wasm32", not(target_os = "wasi")))]
impl SpawnWrapperGuard {
    pub(crate) fn install(wrapper: Option<wafer_block::spawn::SpawnWrapper>) -> Self {
        Self(wafer_block::spawn::set_spawn_wrapper(wrapper))
    }
}

#[cfg(all(test, target_arch = "wasm32", not(target_os = "wasi")))]
impl Drop for SpawnWrapperGuard {
    fn drop(&mut self) {
        wafer_block::spawn::set_spawn_wrapper(self.0.take());
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use futures::{executor::LocalPool, task::LocalSpawnExt};
    use wasm_bindgen_test::wasm_bindgen_test;

    use super::*;

    fn marker() -> Option<usize> {
        current().map(|services| services.marker)
    }

    /// Build a valid, digest-matched `keys.json` payload for tests: the raw
    /// bytes plus the `sha256:<hex>` digest `ReleaseInventory::from_json_bytes`
    /// and `ReleaseAssetIdentity::parse` both expect.
    fn inventory_json(keys: &[&str]) -> (Vec<u8>, String) {
        let bytes = serde_json::to_vec(keys).unwrap();
        let sha = format!("sha256:{}", impresspress_core::util::sha256_hex(&bytes));
        (bytes, sha)
    }

    /// Serves a fixed `keys.json` payload at `{prefix}/keys.json`; every other
    /// call is a hard error — release-guard tests only ever need the one GET.
    struct FakeReleaseStorage {
        prefix: String,
        keys_bytes: Vec<u8>,
    }

    #[wafer_block::wafer_async_trait]
    impl StorageService for FakeReleaseStorage {
        async fn put(
            &self,
            _folder: &str,
            _key: &str,
            _data: &[u8],
            _content_type: &str,
        ) -> Result<(), StorageError> {
            Err(StorageError::Internal("unsupported in test".into()))
        }

        async fn put_streaming(
            &self,
            _folder: &str,
            _key: &str,
            _data: InputStream,
            _content_type: &str,
        ) -> Result<(), StorageError> {
            Err(StorageError::Internal("unsupported in test".into()))
        }

        async fn get(
            &self,
            folder: &str,
            key: &str,
        ) -> Result<(Vec<u8>, ObjectInfo), StorageError> {
            if folder == self.prefix && key == "keys.json" {
                return Ok((
                    self.keys_bytes.clone(),
                    ObjectInfo {
                        key: key.to_string(),
                        size: self.keys_bytes.len() as i64,
                        content_type: "application/json".to_string(),
                        last_modified: chrono::Utc::now(),
                    },
                ));
            }
            Err(StorageError::NotFound)
        }

        async fn get_streaming(
            &self,
            _folder: &str,
            _key: &str,
        ) -> Result<(OutputStream, ObjectInfo), StorageError> {
            Err(StorageError::Internal("unsupported in test".into()))
        }

        async fn delete(&self, _folder: &str, _key: &str) -> Result<(), StorageError> {
            Err(StorageError::Internal("unsupported in test".into()))
        }

        async fn list(
            &self,
            _folder: &str,
            _opts: &StorageListOptions,
        ) -> Result<ObjectList, StorageError> {
            Err(StorageError::Internal("unsupported in test".into()))
        }

        async fn create_folder(&self, _name: &str, _public: bool) -> Result<(), StorageError> {
            Err(StorageError::Internal("unsupported in test".into()))
        }

        async fn delete_folder(&self, _name: &str) -> Result<(), StorageError> {
            Err(StorageError::Internal("unsupported in test".into()))
        }

        async fn list_folders(&self) -> Result<Vec<FolderInfo>, StorageError> {
            Err(StorageError::Internal("unsupported in test".into()))
        }
    }

    struct ObserveAcrossPolls {
        expected: usize,
        observations: Rc<RefCell<Vec<Option<usize>>>>,
        poll: usize,
    }

    impl Future for ObserveAcrossPolls {
        type Output = ();

        fn poll(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Self::Output> {
            self.observations.borrow_mut().push(marker());
            assert_eq!(marker(), Some(self.expected));
            self.poll += 1;
            if self.poll < 3 {
                cx.waker().wake_by_ref();
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        }
    }

    #[wasm_bindgen_test]
    fn interleaved_futures_reenter_their_own_request_scope_on_every_poll() {
        let first_seen = Rc::new(RefCell::new(Vec::new()));
        let second_seen = Rc::new(RefCell::new(Vec::new()));
        let mut pool = LocalPool::new();
        pool.spawner()
            .spawn_local(scope(
                RequestServices::marker(11),
                ObserveAcrossPolls {
                    expected: 11,
                    observations: first_seen.clone(),
                    poll: 0,
                },
            ))
            .unwrap();
        pool.spawner()
            .spawn_local(scope(
                RequestServices::marker(22),
                ObserveAcrossPolls {
                    expected: 22,
                    observations: second_seen.clone(),
                    poll: 0,
                },
            ))
            .unwrap();
        pool.run();

        assert_eq!(&*first_seen.borrow(), &[Some(11), Some(11), Some(11)]);
        assert_eq!(&*second_seen.borrow(), &[Some(22), Some(22), Some(22)]);
        assert_eq!(marker(), None);
    }

    #[wasm_bindgen_test]
    async fn nested_scope_restores_outer_request_and_then_fails_closed() {
        let outer = RequestServices::marker(1);
        let inner = RequestServices::marker(2);
        scope_sync(outer, || {
            assert_eq!(marker(), Some(1));
            scope_sync(inner, || assert_eq!(marker(), Some(2)));
            assert_eq!(marker(), Some(1));
        });
        assert_eq!(marker(), None);
        assert!(ScopedConfigService.get("anything").is_none());
        assert!(ScopedCryptoService.random_bytes(1).await.is_err());
    }

    #[wasm_bindgen_test]
    fn parse_rejects_keys_digest_not_shaped_like_sha256_hex() {
        let id = "56".repeat(32);
        let prefix = format!("{RELEASES_ROOT}/{id}");
        let manifest_key = format!("{prefix}/manifest.json");
        // Missing the `sha256:` prefix entirely.
        assert!(ReleaseAssetIdentity::parse(
            id.clone(),
            prefix.clone(),
            manifest_key.clone(),
            "11".repeat(32),
        )
        .is_err());
        // Right prefix, wrong length.
        assert!(ReleaseAssetIdentity::parse(
            id.clone(),
            prefix.clone(),
            manifest_key.clone(),
            "sha256:abcd".into(),
        )
        .is_err());
        // Right prefix and length, non-hex characters.
        assert!(ReleaseAssetIdentity::parse(
            id,
            prefix,
            manifest_key,
            format!("sha256:{}", "zz".repeat(32)),
        )
        .is_err());
    }

    #[wasm_bindgen_test]
    fn release_identity_rejects_inconsistent_contracts() {
        let id = "cd".repeat(32);
        let prefix = format!("{RELEASES_ROOT}/{id}");
        let valid_sha = format!("sha256:{}", "11".repeat(32));
        // Prefix doesn't match the id-derived releases path.
        assert!(ReleaseAssetIdentity::parse(
            id.clone(),
            ".impresspress/releases/v1/wrong".into(),
            format!("{prefix}/manifest.json"),
            valid_sha.clone(),
        )
        .is_err());
        // Manifest key doesn't match `{prefix}/manifest.json`.
        assert!(
            ReleaseAssetIdentity::parse(id, prefix, "wrong/manifest.json".into(), valid_sha)
                .is_err()
        );
    }

    #[wasm_bindgen_test]
    fn loaded_release_redirects_only_exact_normalized_members() {
        let id = "ab".repeat(32);
        let prefix = format!("{RELEASES_ROOT}/{id}");
        let (bytes, sha) = inventory_json(&["gdsf/site/media/hero.webp", "public/app.css"]);
        let identity = Arc::new(
            ReleaseAssetIdentity::parse(
                id.clone(),
                prefix.clone(),
                format!("{prefix}/manifest.json"),
                sha.clone(),
            )
            .unwrap(),
        );
        let inventory = Arc::new(ReleaseInventory::from_json_bytes(&bytes, &sha).unwrap());
        let release = LoadedRelease {
            identity: identity.clone(),
            inventory,
        };

        assert_eq!(identity.id(), id);
        assert_eq!(identity.manifest_key(), format!("{prefix}/manifest.json"));
        assert_eq!(
            release.physical_read_location("gdsf/site", "media/hero.webp"),
            Some((format!("{prefix}/gdsf/site/media"), "hero.webp".to_string()))
        );
        assert_eq!(
            release.physical_read_location("gdsf/site", "media/user-upload.webp"),
            None
        );
        assert_eq!(
            release.physical_read_location("gdsf/site", "../public/app.css"),
            None
        );
        assert_eq!(
            release.physical_object_key("gdsf/site/media/hero.webp"),
            Some(format!("{prefix}/gdsf/site/media/hero.webp"))
        );
        assert!(release.manages_folder("gdsf/site"));
        assert!(!release.manages_folder("uploads"));
    }

    #[wasm_bindgen_test]
    fn loaded_release_membership_rejects_keys_outside_the_inventory() {
        let id = "12".repeat(32);
        let prefix = format!("{RELEASES_ROOT}/{id}");
        let (bytes, sha) = inventory_json(&["assets/a.css", "assets/b.js"]);
        let identity = Arc::new(
            ReleaseAssetIdentity::parse(
                id,
                prefix.clone(),
                format!("{prefix}/manifest.json"),
                sha.clone(),
            )
            .unwrap(),
        );
        let inventory = Arc::new(ReleaseInventory::from_json_bytes(&bytes, &sha).unwrap());
        let release = LoadedRelease {
            identity,
            inventory,
        };

        assert_eq!(
            release.inventory.logical_keys_sorted(),
            vec!["assets/a.css", "assets/b.js"]
        );
        assert!(release.physical_object_key("assets/a.css").is_some());
        assert!(release.physical_object_key("assets/tampered.js").is_none());
    }

    #[wasm_bindgen_test]
    fn scoped_storage_rejects_managed_put_delete_and_list_helpers() {
        let id = "34".repeat(32);
        let prefix = format!("{RELEASES_ROOT}/{id}");
        let (bytes, sha) = inventory_json(&["public/app.css"]);
        let identity =
            ReleaseAssetIdentity::parse(id, prefix.clone(), format!("{prefix}/manifest.json"), sha)
                .unwrap();
        let storage: Arc<dyn StorageService> = Arc::new(FakeReleaseStorage {
            prefix,
            keys_bytes: bytes,
        });

        futures::executor::block_on(scope(
            RequestServices::marker_with_release(1, identity, storage),
            async {
                assert!(
                    ScopedStorageService::reject_managed_object_mutation("public", "app.css")
                        .await
                        .is_err()
                );
                // Both put and delete call the same exact-object guard.
                assert!(ScopedStorageService::reject_managed_object_mutation(
                    "public",
                    "upload.css"
                )
                .await
                .is_ok());
                assert!(
                    ScopedStorageService::reject_managed_folder_listing("public")
                        .await
                        .is_err()
                );
                assert!(
                    ScopedStorageService::reject_managed_folder_listing("uploads")
                        .await
                        .is_ok()
                );
            },
        ));
    }
}
