//! Export/import boundary between [`ImpresspressBuilder`] registration and an
//! immutable [`PreparedRuntimePlan`](crate::PreparedRuntimePlan).
//!
//! Both preparation and hydration execute the consumer's normal builder
//! registration function. The plan replaces deployment-owned settings/config,
//! while compiled block factories and route declarations are parity-checked so
//! a target with a different feature set cannot silently accept the artifact.

use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, RwLock},
};

use wafer_run::BlockRuntime;

use super::ImpresspressBuilder;
use crate::{
    features::BlockSettings,
    prepared_plan::{
        canonicalize_json, PreparedBlockImplementation, PreparedBlockRuntime, PreparedGrantWrite,
        PreparedPlanError, PreparedReleaseAssets, PreparedResourceGrant, PreparedResourceType,
        PreparedRoute, PreparedRouteAccess, PreparedRuntimePlan, PreparedRuntimeStructure,
        WaferLockIdentity,
    },
    routing::{ExtraRoute, RouteAccess, ROUTES},
};

/// Deploy-time plan producer detached from the consumed builder.
///
/// A Cloudflare candidate gets this from
/// [`ImpresspressBuilder::build_with_plan_exporter`], after executing the real
/// consumer registration closure. The exporter retains only immutable descriptor data plus the shared
/// `BlockSettings` snapshot handle. After deploy initialization seeds and
/// republishes those settings, `prepare_runtime_plan()` observes the final
/// snapshot and can return the plan through the candidate endpoint.
///
/// This type is control-plane state and is never serialized into the plan.
#[derive(Clone)]
pub struct PreparedPlanExporter {
    application_blocks: Vec<PreparedBlockImplementation>,
    routes: Vec<PreparedRoute>,
    built_in_route_count: usize,
    block_settings: Arc<RwLock<BlockSettings>>,
    block_configs: BTreeMap<String, serde_json::Value>,
    final_block_configs: BTreeMap<String, serde_json::Value>,
    wrap_grants: Vec<PreparedResourceGrant>,
    deployment_wrap_grants: Arc<RwLock<Vec<PreparedResourceGrant>>>,
}

impl PreparedPlanExporter {
    /// Publish a deploy-time settings re-read before finalizing the plan.
    ///
    /// Deploy initialization seeds settings before initializing the remaining
    /// blocks, while those blocks may stamp migration hashes during `Init`.
    /// A candidate endpoint should therefore re-read settings after every
    /// block has initialized and pass that final snapshot here. This mutation
    /// affects only the shared in-memory descriptor; it performs no I/O.
    pub fn publish_block_settings(&self, settings: BlockSettings) -> Result<(), PreparedPlanError> {
        *self.block_settings.write().map_err(|_| {
            PreparedPlanError::StructureMismatch(
                "BlockSettings lock poisoned while publishing deploy snapshot".into(),
            )
        })? = settings;
        Ok(())
    }

    /// Add the final deploy-time D1 grant snapshot. Repeated calls are safe:
    /// plan normalization sorts and de-duplicates identical grants.
    pub fn publish_wrap_grants(
        &self,
        grants: &[wafer_run::ResourceGrant],
    ) -> Result<(), PreparedPlanError> {
        self.deployment_wrap_grants
            .write()
            .map_err(|_| {
                PreparedPlanError::StructureMismatch(
                    "WRAP grant lock poisoned while publishing deploy snapshot".into(),
                )
            })?
            .extend(grants.iter().map(PreparedResourceGrant::from));
        Ok(())
    }

    /// Snapshot the latest settings together with the immutable registration
    /// descriptor. Pure CPU/memory work; no platform services are consulted.
    pub fn prepared_runtime_structure(
        &self,
    ) -> Result<PreparedRuntimeStructure, PreparedPlanError> {
        let mut structure = PreparedRuntimeStructure {
            application_blocks: self.application_blocks.clone(),
            routes: self.routes.clone(),
            built_in_route_count: self.built_in_route_count,
            block_settings: self
                .block_settings
                .read()
                .map_err(|_| {
                    PreparedPlanError::StructureMismatch(
                        "BlockSettings lock poisoned during plan export".into(),
                    )
                })?
                .to_sorted_map(),
            block_configs: self.block_configs.clone(),
            final_block_configs: self.final_block_configs.clone(),
            wrap_grants: self.wrap_grants.clone(),
            deployment_wrap_grants: self
                .deployment_wrap_grants
                .read()
                .map_err(|_| {
                    PreparedPlanError::StructureMismatch(
                        "WRAP grant lock poisoned during plan export".into(),
                    )
                })?
                .clone(),
        };
        structure.normalize();
        structure.validate()?;
        Ok(structure)
    }

    pub fn prepare_runtime_plan(
        &self,
        application_id: impl Into<String>,
        application_build_sha256: impl Into<String>,
        dependency_lock: WaferLockIdentity,
        release_assets: PreparedReleaseAssets,
    ) -> Result<PreparedRuntimePlan, PreparedPlanError> {
        PreparedRuntimePlan::new(
            application_id,
            application_build_sha256,
            dependency_lock,
            release_assets,
            self.prepared_runtime_structure()?,
        )
    }

    /// Finalize a deployable plan bound to the exact post-funnel KV
    /// structural/config generation.
    pub fn prepare_runtime_plan_with_config_generation(
        &self,
        application_id: impl Into<String>,
        application_build_sha256: impl Into<String>,
        config_generation: impl Into<String>,
        dependency_lock: WaferLockIdentity,
        release_assets: PreparedReleaseAssets,
    ) -> Result<PreparedRuntimePlan, PreparedPlanError> {
        PreparedRuntimePlan::new_with_config_generation(
            application_id,
            application_build_sha256,
            config_generation,
            dependency_lock,
            release_assets,
            self.prepared_runtime_structure()?,
        )
    }
}

/// Why [`ImpresspressBuilder::build_with_plan_exporter`] or
/// [`AppliedPlanBuilder::build_with_plan_exporter`] produced no runtime.
#[derive(Debug, thiserror::Error)]
pub enum PreparedBuildError {
    /// The runtime did not build (`ImpresspressBuilder::build`'s error).
    #[error("builder.build")]
    Build(#[source] wafer_run::RuntimeError),
    /// The runtime built, but the plan it was asked to hydrate describes
    /// another application.
    #[error(transparent)]
    Plan(#[from] PreparedPlanError),
}

impl From<wafer_run::RuntimeError> for PreparedBuildError {
    fn from(error: wafer_run::RuntimeError) -> Self {
        Self::Build(error)
    }
}

/// A builder that imported a prepared plan
/// ([`ImpresspressBuilder::apply_prepared_plan`]).
///
/// It builds only through [`Self::build_with_plan_exporter`], which refuses a
/// runtime whose application blocks differ from the plan's, with
/// [`PreparedPlanError::StructureMismatch`].
///
/// That comparison runs after registration, not at import, because the
/// inventory is not known before: a plan pins each block's version,
/// interface and runtime as well as its name, and those are declared by the
/// block's `info()`, which the runtime reads once, when it registers the
/// block (wafer-run#478). Reading it at import would construct every block
/// and read its declaration a second time on every cold build — the cost this
/// type exists not to pay. A mismatch therefore costs the registration of
/// the blocks, never their seal or `Init`.
pub struct AppliedPlanBuilder {
    builder: ImpresspressBuilder,
    application_blocks: Vec<PreparedBlockImplementation>,
}

impl AppliedPlanBuilder {
    /// The settings handle the runtime will be built with; see
    /// [`ImpresspressBuilder::block_settings_handle`].
    pub fn block_settings_handle(&self) -> Arc<RwLock<BlockSettings>> {
        self.builder.block_settings_handle()
    }

    /// Build the runtime with the plan's settings, configs, routes and
    /// grants, and refuse it unless the application blocks it registered are
    /// the plan's.
    pub fn build_with_plan_exporter(
        self,
    ) -> Result<(wafer_run::Wafer, PreparedPlanExporter), PreparedBuildError> {
        let (wafer, exporter) = self.builder.build_with_plan_exporter()?;
        if exporter.application_blocks != self.application_blocks {
            return Err(PreparedPlanError::StructureMismatch(
                "compiled application block inventory does not match".into(),
            )
            .into());
        }
        Ok((wafer, exporter))
    }
}

/// The implementations of the application blocks named in `application`, from
/// the runtime's registration-time declarations, sorted as a plan stores them.
///
/// `infos` is [`wafer_run::Wafer::block_infos`]: the `BlockInfo` each block's
/// registration read once, so this reads no block's `info()`.
pub(super) fn application_inventory(
    infos: &[Arc<wafer_run::BlockInfo>],
    application: &[String],
) -> Vec<PreparedBlockImplementation> {
    let mut blocks: Vec<_> = infos
        .iter()
        .filter(|info| application.contains(&info.name))
        .map(|info| PreparedBlockImplementation {
            name: info.name.clone(),
            version: info.version.clone(),
            interface: info.interface.clone(),
            runtime: match info.runtime {
                BlockRuntime::Native => PreparedBlockRuntime::Native,
                BlockRuntime::Wasm => PreparedBlockRuntime::Wasm,
            },
        })
        .collect();
    blocks.sort_by(|a, b| {
        (&a.name, &a.version, &a.interface).cmp(&(&b.name, &b.version, &b.interface))
    });
    blocks
}

/// A builder's block-config list as a plan stores it: in name order, each
/// value canonicalized. Wafer's registration map has last-write-wins
/// semantics, so a later entry for a name replaces an earlier one here too.
fn config_map(configs: &[(String, serde_json::Value)]) -> BTreeMap<String, serde_json::Value> {
    let mut map = BTreeMap::new();
    for (name, value) in configs {
        let mut value = value.clone();
        canonicalize_json(&mut value);
        map.insert(name.clone(), value);
    }
    map
}

impl ImpresspressBuilder {
    /// Build the runtime, and the exporter of the plan that describes it.
    ///
    /// This is the candidate-endpoint seam: execute consumer registrations,
    /// call this method, boot the runtime normally, then use the exporter
    /// after boot to package the final seeded structural snapshot. The
    /// exporter's application block inventory is the one `build()`
    /// registered, read from the runtime's registration-time `BlockInfo`s.
    pub fn build_with_plan_exporter(
        self,
    ) -> Result<(wafer_run::Wafer, PreparedPlanExporter), PreparedBuildError> {
        let mut exporter = self.unregistered_exporter();
        let registered = self.register()?;
        exporter.application_blocks = registered.application_blocks;
        // Not validated here: on the request path nothing exports a plan, and
        // `prepared_runtime_structure` validates whenever one is.
        Ok((registered.wafer, exporter))
    }

    /// The exporter of everything this builder holds that a plan carries,
    /// except the application block inventory, which only registration
    /// knows: [`Self::build_with_plan_exporter`] fills it in.
    fn unregistered_exporter(&self) -> PreparedPlanExporter {
        PreparedPlanExporter {
            application_blocks: Vec::new(),
            routes: self.prepared_routes(),
            built_in_route_count: ROUTES.len(),
            block_settings: self.block_settings.clone(),
            block_configs: config_map(&self.block_configs),
            final_block_configs: config_map(&self.final_block_configs),
            wrap_grants: self.prepared_wrap_grants(),
            deployment_wrap_grants: Arc::new(RwLock::new(
                self.deployment_wrap_grants
                    .iter()
                    .map(PreparedResourceGrant::from)
                    .collect(),
            )),
        }
    }

    /// The built-in routes followed by this builder's consumer routes, as a
    /// plan records them.
    fn prepared_routes(&self) -> Vec<PreparedRoute> {
        ROUTES
            .iter()
            .map(|route| PreparedRoute {
                prefix: route.prefix.to_string(),
                access: route.access.into(),
                block: route.block.to_string(),
                dispatch_to: route.dispatch_to.to_string(),
                // Built-in routes are never consumer routes; the undeclared
                // policy for them is `declared_access`'s own fail-closed
                // default.
                refine_undeclared: false,
            })
            .chain(self.extra_routes.iter().map(|route| PreparedRoute {
                prefix: route.prefix.clone(),
                access: route.access.into(),
                block: route.block_name.clone(),
                dispatch_to: route.block_name.clone(),
                refine_undeclared: route.refines_undeclared(),
            }))
            .collect()
    }

    /// This builder's consumer-declared WRAP grants, sorted and de-duplicated
    /// as a plan stores them.
    fn prepared_wrap_grants(&self) -> Vec<PreparedResourceGrant> {
        let mut grants: Vec<_> = self
            .wrap_grants
            .iter()
            .map(PreparedResourceGrant::from)
            .collect();
        grants.sort();
        grants.dedup();
        grants
    }

    /// Verify and apply a prepared plan to the normal builder path.
    ///
    /// Call the consumer's usual registration function first, then invoke
    /// this method, inject current platform services, and call `build()`. The
    /// complete route declaration, final block configs and consumer grants
    /// must match the target compile, or this refuses the plan; the
    /// application block inventory must too, which `build()` checks against
    /// the blocks it registers. Deployment-owned block settings and non-secret
    /// structural configs are then replaced from the plan without external
    /// I/O. Executable block objects and service backends remain those already
    /// installed on this builder.
    ///
    /// This prototype intentionally does not claim that `Wafer::seal()` is
    /// offline: flows, aliases, remote references, grants, and sealed dispatch
    /// structures still need a stable wafer-run import/offline-seal API.
    pub fn apply_prepared_plan(
        mut self,
        plan: &PreparedRuntimePlan,
        expected_application_id: &str,
        expected_application_build_sha256: &str,
        expected_dependency_lock: &WaferLockIdentity,
        expected_release_assets: &PreparedReleaseAssets,
    ) -> Result<AppliedPlanBuilder, PreparedPlanError> {
        plan.verify_compatibility(
            expected_application_id,
            expected_application_build_sha256,
            expected_dependency_lock,
            expected_release_assets,
        )?;

        // What this compile declares must be what the plan was prepared
        // from: its routes, its final block-config overrides and its
        // consumer-declared WRAP grants. Each is compared as the plan stores
        // it, before any block is constructed. The application block
        // inventory is compared by `AppliedPlanBuilder::build_with_plan_exporter`,
        // once registration has read it.
        //
        // The rest of the plan is the deployment's, not the compile's, and is
        // taken from it unread below: the block settings, the structural block
        // configs and the deploy-time grants replace this builder's.
        if self.prepared_routes() != plan.structure.routes
            || ROUTES.len() != plan.structure.built_in_route_count
        {
            return Err(PreparedPlanError::StructureMismatch(
                "compiled built-in/consumer routes do not match".into(),
            ));
        }
        if config_map(&self.final_block_configs) != plan.structure.final_block_configs {
            return Err(PreparedPlanError::StructureMismatch(
                "compiled final block-config overrides do not match".into(),
            ));
        }
        if self.prepared_wrap_grants() != plan.structure.wrap_grants {
            return Err(PreparedPlanError::StructureMismatch(
                "consumer-declared WRAP grants do not match".into(),
            ));
        }

        let blocks: HashMap<_, _> = plan
            .structure
            .block_settings
            .iter()
            .map(|(name, state)| (name.clone(), state.clone()))
            .collect();
        *self.block_settings.write().map_err(|_| {
            PreparedPlanError::StructureMismatch(
                "BlockSettings lock poisoned during plan import".into(),
            )
        })? = BlockSettings::from_blocks(blocks);

        self.block_configs = plan
            .structure
            .block_configs
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        self.final_block_configs = plan
            .structure
            .final_block_configs
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();

        self.extra_routes = plan.structure.routes[plan.structure.built_in_route_count..]
            .iter()
            .map(|route| {
                if route.refine_undeclared {
                    ExtraRoute::refined(
                        route.prefix.clone(),
                        route.block.clone(),
                        route.access.into(),
                    )
                } else {
                    ExtraRoute::new(
                        route.prefix.clone(),
                        route.block.clone(),
                        route.access.into(),
                    )
                }
            })
            .collect();

        self.wrap_grants = plan
            .structure
            .wrap_grants
            .iter()
            .cloned()
            .map(wafer_run::ResourceGrant::from)
            .collect();
        self.deployment_wrap_grants = plan
            .structure
            .deployment_wrap_grants
            .iter()
            .cloned()
            .map(wafer_run::ResourceGrant::from)
            .collect();

        Ok(AppliedPlanBuilder {
            builder: self,
            application_blocks: plan.structure.application_blocks.clone(),
        })
    }
}

impl From<&wafer_run::ResourceGrant> for PreparedResourceGrant {
    fn from(grant: &wafer_run::ResourceGrant) -> Self {
        Self {
            grantee: grant.grantee.clone(),
            resource: grant.resource.clone(),
            write: match grant.write {
                wafer_block::GrantWrite::None => PreparedGrantWrite::Read,
                wafer_block::GrantWrite::Full => PreparedGrantWrite::ReadWrite,
                wafer_block::GrantWrite::Append => PreparedGrantWrite::Append,
            },
            resource_type: grant.resource_type.as_ref().map(|kind| match kind {
                wafer_run::ResourceType::Db => PreparedResourceType::Db,
                wafer_run::ResourceType::Config => PreparedResourceType::Config,
                wafer_run::ResourceType::Storage => PreparedResourceType::Storage,
                wafer_run::ResourceType::Crypto => PreparedResourceType::Crypto,
                wafer_run::ResourceType::Network => PreparedResourceType::Network,
                wafer_run::ResourceType::Vector => PreparedResourceType::Vector,
                wafer_run::ResourceType::Auth => PreparedResourceType::Auth,
                wafer_run::ResourceType::Llm => PreparedResourceType::Llm,
                wafer_run::ResourceType::Image => PreparedResourceType::Image,
                wafer_run::ResourceType::Embedding => PreparedResourceType::Embedding,
            }),
        }
    }
}

impl From<PreparedResourceGrant> for wafer_run::ResourceGrant {
    fn from(grant: PreparedResourceGrant) -> Self {
        Self {
            grantee: grant.grantee,
            resource: grant.resource,
            write: match grant.write {
                PreparedGrantWrite::Read => wafer_block::GrantWrite::None,
                PreparedGrantWrite::ReadWrite => wafer_block::GrantWrite::Full,
                PreparedGrantWrite::Append => wafer_block::GrantWrite::Append,
            },
            resource_type: grant.resource_type.map(|kind| match kind {
                PreparedResourceType::Db => wafer_run::ResourceType::Db,
                PreparedResourceType::Config => wafer_run::ResourceType::Config,
                PreparedResourceType::Storage => wafer_run::ResourceType::Storage,
                PreparedResourceType::Crypto => wafer_run::ResourceType::Crypto,
                PreparedResourceType::Network => wafer_run::ResourceType::Network,
                PreparedResourceType::Vector => wafer_run::ResourceType::Vector,
                PreparedResourceType::Auth => wafer_run::ResourceType::Auth,
                PreparedResourceType::Llm => wafer_run::ResourceType::Llm,
                PreparedResourceType::Image => wafer_run::ResourceType::Image,
                PreparedResourceType::Embedding => wafer_run::ResourceType::Embedding,
            }),
        }
    }
}

impl From<RouteAccess> for PreparedRouteAccess {
    fn from(access: RouteAccess) -> Self {
        match access {
            RouteAccess::Public => Self::Public,
            RouteAccess::Authenticated => Self::Authenticated,
            RouteAccess::Admin => Self::Admin,
        }
    }
}

impl From<PreparedRouteAccess> for RouteAccess {
    fn from(access: PreparedRouteAccess) -> Self {
        match access {
            PreparedRouteAccess::Public => Self::Public,
            PreparedRouteAccess::Authenticated => Self::Authenticated,
            PreparedRouteAccess::Admin => Self::Admin,
        }
    }
}

#[cfg(test)]
pub(super) mod tests {
    use wafer_core::interfaces::network::service::{
        NetworkError, NetworkService, Request, Response,
    };

    use super::*;
    use crate::features::BlockState;

    fn hash(ch: char) -> String {
        format!("sha256:{}", ch.to_string().repeat(64))
    }

    /// Nothing these builds do leaves the process.
    struct NoNetwork;

    #[wafer_block::wafer_async_trait]
    impl NetworkService for NoNetwork {
        async fn do_request(&self, req: &Request) -> Result<Response, NetworkError> {
            Err(NetworkError::Other(format!("no network: {}", req.url)))
        }
    }

    /// `builder` with the six services `build()` requires, over an in-memory
    /// SQLite database, and the two paths a build with `native-embedding` or
    /// `block-fastembed` asks for: the vector store's SQLite (in memory too)
    /// and the embedder's model cache directory, which a build only records —
    /// it is read when the block loads a model, which no test here does.
    pub(in crate::builder) fn serviced(builder: ImpresspressBuilder) -> ImpresspressBuilder {
        let database = Arc::new(
            wafer_block_sqlite::service::SQLiteDatabaseService::open_in_memory()
                .expect("open in-memory sqlite"),
        );
        let (builder, ()) = crate::builder::RuntimeConfig::new()
            .install(
                builder.database(database, wafer_sql_utils::Backend::Sqlite),
                |map| {
                    (
                        crate::builder::fill_config_service(
                            Arc::new(wafer_core::service_blocks::config::EnvConfigService::new()),
                            map,
                        ),
                        (),
                    )
                },
            )
            .expect("install the config surfaces");
        builder
            .storage(Arc::new(crate::test_support::InMemoryStorageService::new()))
            .crypto(Arc::new(crate::test_support::real_crypto_service()))
            .network(Arc::new(NoNetwork))
            .logger(Arc::new(wafer_core::service_blocks::logger::TracingLogger))
            .sqlite_db_path(":memory:")
            .model_cache_dir(std::env::temp_dir().join("impresspress-prepared-plan-tests-models"))
    }

    /// The exporter of `builder`'s runtime, built.
    pub(in crate::builder) fn exporter(builder: ImpresspressBuilder) -> PreparedPlanExporter {
        serviced(builder)
            .build_with_plan_exporter()
            .expect("the runtime builds")
            .1
    }

    /// The structure `builder`'s runtime exports.
    fn structure(builder: ImpresspressBuilder) -> PreparedRuntimeStructure {
        exporter(builder)
            .prepared_runtime_structure()
            .expect("the structure exports")
    }

    /// The plan `builder`'s runtime exports, bound to `lock`.
    fn plan(builder: ImpresspressBuilder, lock: &WaferLockIdentity) -> PreparedRuntimePlan {
        exporter(builder)
            .prepare_runtime_plan(
                "example",
                hash('b'),
                lock.clone(),
                PreparedReleaseAssets::absent(),
            )
            .expect("the plan exports")
    }

    /// `builder`, serviced, importing `plan`.
    fn apply(
        builder: ImpresspressBuilder,
        plan: &PreparedRuntimePlan,
        lock: &WaferLockIdentity,
    ) -> Result<AppliedPlanBuilder, PreparedPlanError> {
        serviced(builder).apply_prepared_plan(
            plan,
            "example",
            &hash('b'),
            lock,
            &PreparedReleaseAssets::absent(),
        )
    }

    /// The structure an imported plan's runtime exports, built.
    fn applied_structure(applied: AppliedPlanBuilder) -> PreparedRuntimeStructure {
        applied
            .build_with_plan_exporter()
            .expect("the runtime builds")
            .1
            .prepared_runtime_structure()
            .expect("the structure exports")
    }

    #[test]
    fn builder_export_import_round_trip_preserves_structure() {
        let mut settings = HashMap::new();
        settings.insert(
            "impresspress/products".to_string(),
            BlockState {
                enabled: false,
                ..BlockState::default()
            },
        );
        let source = ImpresspressBuilder::new()
            .block_settings(BlockSettings::from_blocks(settings))
            .block_config("wafer-run/web", serde_json::json!({ "web_root": "site" }))
            .add_route("/app/", "example/app", crate::routing::RouteAccess::Public);
        let lock = WaferLockIdentity::from_digest(2, hash('a')).unwrap();
        let plan = plan(source, &lock);

        // The target executes the same consumer registration before import.
        let target = ImpresspressBuilder::new().add_route(
            "/app/",
            "example/app",
            crate::routing::RouteAccess::Public,
        );
        let applied = apply(target, &plan, &lock).unwrap();
        assert_eq!(applied_structure(applied), plan.structure);
    }

    /// The plan's application block inventory is what the runtime that
    /// exported it registered on the application's behalf: the feature-block
    /// manifest, admin, llm and the consumer's extra blocks, and none of the
    /// runtime's own service, middleware, router or framework-auth blocks.
    #[test]
    fn the_inventory_is_the_application_blocks_the_runtime_registered() {
        let names: Vec<String> = structure(ImpresspressBuilder::new())
            .application_blocks
            .into_iter()
            .map(|block| block.name)
            .collect();
        let mut expected: Vec<String> = crate::blocks::all_block_infos()
            .into_iter()
            .map(|info| info.name)
            .collect();
        expected.sort();
        assert_eq!(names, expected);
    }

    /// A target whose compiled blocks differ from the plan's is refused when
    /// it builds, with the typed structure mismatch: the inventory is known
    /// only once registration has read it, and the import constructs no
    /// block.
    #[test]
    fn build_refuses_a_plan_whose_inventory_differs() {
        let lock = WaferLockIdentity::absent();
        let plan = plan(
            ImpresspressBuilder::new()
                .extra_block("example/counted", Arc::new(Counted(Arc::default()))),
            &lock,
        );
        assert!(plan
            .structure
            .application_blocks
            .iter()
            .any(|block| block.name == "example/counted"));

        let target = apply(ImpresspressBuilder::new(), &plan, &lock)
            .expect("the import compares no inventory");
        let Err(error) = target.build_with_plan_exporter() else {
            panic!("a runtime without the plan's example/counted must not build");
        };
        assert!(
            matches!(
                error,
                PreparedBuildError::Plan(PreparedPlanError::StructureMismatch(_))
            ),
            "{error:?}"
        );
    }

    /// A consumer block that counts the reads of its declaration.
    struct Counted(Arc<std::sync::atomic::AtomicUsize>);

    #[wafer_block::wafer_async_trait]
    impl wafer_run::Block for Counted {
        fn info(&self) -> wafer_run::BlockInfo {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            wafer_run::BlockInfo::new("example/counted", "0.0.1", "echo@v1", "counts info()")
        }
        async fn handle(
            &self,
            _ctx: &dyn wafer_run::context::Context,
            _msg: wafer_run::Message,
            _input: wafer_run::InputStream,
        ) -> wafer_run::OutputStream {
            crate::http::ResponseBuilder::new()
                .status(200)
                .body(Vec::new(), "text/plain")
        }
        async fn lifecycle(
            &self,
            _ctx: &dyn wafer_run::context::Context,
            _event: wafer_run::LifecycleEvent,
        ) -> Result<(), wafer_run::WaferError> {
            Ok(())
        }
    }

    /// The request-path build reads each block's declaration once, at its
    /// registration (wafer-run#478): importing a plan, building the runtime
    /// with its exporter, sealing, initializing the block and exporting the
    /// plan read none again — neither a consumer block's (the counted block)
    /// nor the feature blocks' (no `blocks::all_block_infos` sweep, through
    /// any wrapper). Before, the import and the exporter each constructed the
    /// application block inventory from `info()`. A full request boot is held
    /// to the same in `tests/d1_read_cost.rs`.
    #[tokio::test]
    async fn a_prepared_build_reads_each_declaration_once() {
        let lock = WaferLockIdentity::absent();
        let counted = |reads: &Arc<std::sync::atomic::AtomicUsize>| -> Arc<dyn wafer_run::Block> {
            Arc::new(Counted(Arc::clone(reads)))
        };
        let source_reads = Arc::default();
        let plan = plan(
            ImpresspressBuilder::new().extra_block("example/counted", counted(&source_reads)),
            &lock,
        );
        assert_eq!(
            source_reads.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "exporting a plan reads the declaration only at registration"
        );

        let reads = Arc::default();
        let target = apply(
            ImpresspressBuilder::new().extra_block("example/counted", counted(&reads)),
            &plan,
            &lock,
        )
        .expect("the plan applies");
        let sweeps = crate::blocks::block_set_sweeps();
        let (mut wafer, exporter) = target
            .build_with_plan_exporter()
            .expect("the runtime builds");
        wafer.seal().await.expect("the runtime seals");
        wafer
            .init_block("example/counted")
            .await
            .expect("the counted block initializes");
        exporter
            .prepare_runtime_plan(
                "example",
                hash('b'),
                lock.clone(),
                PreparedReleaseAssets::absent(),
            )
            .expect("the plan exports");
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(
            crate::blocks::block_set_sweeps(),
            sweeps,
            "a prepared build constructed the feature blocks again"
        );
    }

    #[test]
    fn import_rejects_registration_route_drift() {
        let source = ImpresspressBuilder::new().add_route(
            "/app/",
            "example/app",
            crate::routing::RouteAccess::Public,
        );
        let lock = WaferLockIdentity::from_digest(2, hash('a')).unwrap();
        let plan = plan(source, &lock);

        let Err(error) = apply(ImpresspressBuilder::new(), &plan, &lock) else {
            panic!("route drift should fail plan import");
        };
        assert!(matches!(error, PreparedPlanError::StructureMismatch(_)));
    }

    #[test]
    fn exporter_survives_builder_consumption_and_observes_seeded_settings() {
        // `build_with_plan_exporter` consumes the builder; the exporter keeps
        // its settings handle.
        let exporter = exporter(ImpresspressBuilder::new());

        let mut seeded = HashMap::new();
        seeded.insert(
            "impresspress/products".to_string(),
            BlockState {
                enabled: false,
                ..BlockState::default()
            },
        );
        exporter
            .publish_block_settings(BlockSettings::from_blocks(seeded))
            .unwrap();

        let structure = exporter.prepared_runtime_structure().unwrap();
        assert!(!structure.block_settings["impresspress/products"].enabled);
    }

    #[test]
    fn published_wrap_grants_round_trip_into_builder() {
        let exporter = exporter(ImpresspressBuilder::new());
        exporter
            .publish_wrap_grants(&[wafer_run::ResourceGrant::read_write(
                "example/app",
                "impresspress__products__*",
            )])
            .unwrap();
        let lock = WaferLockIdentity::absent();
        let plan = exporter
            .prepare_runtime_plan(
                "example",
                hash('b'),
                lock.clone(),
                PreparedReleaseAssets::absent(),
            )
            .unwrap();
        assert_eq!(plan.structure.deployment_wrap_grants.len(), 1);

        let applied = apply(ImpresspressBuilder::new(), &plan, &lock).unwrap();
        assert_eq!(applied.builder.deployment_wrap_grants.len(), 1);
        assert_eq!(applied_structure(applied), plan.structure);
    }

    /// An append-only grant crosses the plan as `"append"` and comes back
    /// append-only — never widened to read-write or narrowed to read-only —
    /// while the boolean spelling every earlier plan used still imports.
    #[test]
    fn an_append_grant_round_trips_through_the_plan() {
        let exporter = exporter(ImpresspressBuilder::new());
        exporter
            .publish_wrap_grants(&[wafer_run::ResourceGrant::append(
                "example/app",
                "impresspress__products__events",
            )])
            .unwrap();
        let lock = WaferLockIdentity::absent();
        let plan = exporter
            .prepare_runtime_plan(
                "example",
                hash('b'),
                lock.clone(),
                PreparedReleaseAssets::absent(),
            )
            .unwrap();
        let exported = serde_json::to_value(&plan).unwrap();
        assert_eq!(
            exported["structure"]["deployment_wrap_grants"][0]["write"],
            serde_json::json!("append")
        );

        let reimported = PreparedRuntimePlan::from_json(&plan.to_json_pretty().unwrap()).unwrap();
        let applied = apply(ImpresspressBuilder::new(), &reimported, &lock).unwrap();
        assert_eq!(
            applied.builder.deployment_wrap_grants[0].write,
            wafer_block::GrantWrite::Append
        );
        assert_eq!(
            applied.builder.deployment_wrap_grants[0].resource_type,
            Some(wafer_run::ResourceType::Db)
        );

        for (spelling, want) in [
            (serde_json::json!(false), PreparedGrantWrite::Read),
            (serde_json::json!(true), PreparedGrantWrite::ReadWrite),
            (serde_json::json!("append"), PreparedGrantWrite::Append),
        ] {
            let grant: PreparedResourceGrant = serde_json::from_value(serde_json::json!({
                "grantee": "example/app",
                "resource": "impresspress__products__events",
                "write": spelling,
            }))
            .unwrap();
            assert_eq!(grant.write, want);
        }
        assert!(
            serde_json::from_value::<PreparedResourceGrant>(serde_json::json!({
                "grantee": "example/app",
                "resource": "impresspress__products__events",
                "write": "full",
            }))
            .is_err()
        );
    }

    #[test]
    fn final_block_config_is_exported_applied_and_parity_checked() {
        let config = serde_json::json!({
            "routes": [{ "path": "/**", "block": "impresspress/router" }]
        });
        let lock = WaferLockIdentity::absent();
        let plan = plan(
            ImpresspressBuilder::new().final_block_config("wafer-run/router", config.clone()),
            &lock,
        );
        assert_eq!(
            plan.structure.final_block_configs["wafer-run/router"],
            config
        );

        let matching = apply(
            ImpresspressBuilder::new().final_block_config("wafer-run/router", config),
            &plan,
            &lock,
        )
        .unwrap();
        assert_eq!(applied_structure(matching), plan.structure);

        let drifted = ImpresspressBuilder::new()
            .final_block_config("wafer-run/router", serde_json::json!({ "routes": [] }));
        assert!(apply(drifted, &plan, &lock).is_err());
    }

    #[test]
    fn consumer_declared_wrap_grants_are_parity_checked_separately() {
        let declared =
            wafer_run::ResourceGrant::read("impresspress/router", "wafer_run__auth__api_keys");
        let lock = WaferLockIdentity::absent();
        let plan = plan(
            ImpresspressBuilder::new().wrap_grants(vec![declared.clone()]),
            &lock,
        );
        assert_eq!(plan.structure.wrap_grants.len(), 1);
        assert!(plan.structure.deployment_wrap_grants.is_empty());

        apply(
            ImpresspressBuilder::new().wrap_grants(vec![declared]),
            &plan,
            &lock,
        )
        .unwrap();
        assert!(apply(ImpresspressBuilder::new(), &plan, &lock).is_err());
    }
}
