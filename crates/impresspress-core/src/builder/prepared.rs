//! Export/import boundary between [`ImpresspressBuilder`] registration and an
//! immutable [`PreparedRuntimePlan`](crate::PreparedRuntimePlan).
//!
//! Both preparation and hydration execute the consumer's normal builder
//! registration function. The plan replaces deployment-owned settings/config,
//! while compiled block factories and route declarations are parity-checked so
//! a target with a different feature set cannot silently accept the artifact.

use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
};

use wafer_run::BlockRuntime;

use super::ImpresspressBuilder;
use crate::{
    features::BlockSettings,
    prepared_plan::{
        PreparedBlockImplementation, PreparedBlockRuntime, PreparedGrantWrite, PreparedPlanError,
        PreparedReleaseAssets, PreparedResourceGrant, PreparedResourceType, PreparedRoute,
        PreparedRouteAccess, PreparedRuntimePlan, PreparedRuntimeStructure, WaferLockIdentity,
    },
    routing::{ExtraRoute, RouteAccess, ROUTES},
};

/// Deploy-time plan producer detached from the consumed builder.
///
/// A Cloudflare candidate gets this from
/// [`ImpresspressBuilder::build_with_plan_exporter`], after executing the real
/// consumer registration closure. The exporter retains the runtime's
/// structure as a plan stores it — normalized by
/// `PreparedRuntimeStructure::normalize`, the one place that decides it —
/// plus the shared `BlockSettings` snapshot handle and the deploy-time grants.
/// After deploy initialization seeds and republishes those settings,
/// `prepare_runtime_plan()` observes the final snapshot and can return the
/// plan through the candidate endpoint.
///
/// This type is control-plane state and is never serialized into the plan.
#[derive(Clone)]
pub struct PreparedPlanExporter {
    /// Everything but the two live parts below, normalized. Its own
    /// `block_settings` and `deployment_wrap_grants` are left empty: export
    /// fills them from those.
    structure: PreparedRuntimeStructure,
    block_settings: Arc<RwLock<BlockSettings>>,
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
        let mut structure = self.structure.clone();
        structure.block_settings = self
            .block_settings
            .read()
            .map_err(|_| {
                PreparedPlanError::StructureMismatch(
                    "BlockSettings lock poisoned during plan export".into(),
                )
            })?
            .to_sorted_map();
        structure.deployment_wrap_grants = self
            .deployment_wrap_grants
            .read()
            .map_err(|_| {
                PreparedPlanError::StructureMismatch(
                    "WRAP grant lock poisoned during plan export".into(),
                )
            })?
            .clone();
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
/// [`AppliedPlanBuilder::build`] produced no runtime.
///
/// The three variants are three different faults, and a caller that reacts
/// to one must not catch another: only [`Self::Plan`] says a prepared plan
/// does not fit this compile.
#[derive(Debug, thiserror::Error)]
pub enum PreparedBuildError {
    /// The runtime did not build (`ImpresspressBuilder::build`'s error).
    ///
    /// The cause is in the message and not also a `source()`: every caller
    /// renders this with `to_string()` (the request's 500 log,
    /// `/_deploy/init`'s report), and a chain-walker that also followed a
    /// source would print the cause twice.
    #[error("builder.build: {0}")]
    Build(wafer_run::RuntimeError),
    /// What this builder would put in a plan is invalid — a route, a
    /// structural block config, a grant, or an application block's
    /// declaration (no version, no interface, a duplicate) — so no plan could
    /// describe it. Refused before the deploy funnel migrates or seeds.
    #[error("invalid plan input: {0}")]
    InvalidInput(PreparedPlanError),
    /// The imported plan describes another application: the blocks this
    /// compile registered are not the plan's.
    #[error(transparent)]
    Plan(PreparedPlanError),
}

impl From<wafer_run::RuntimeError> for PreparedBuildError {
    fn from(error: wafer_run::RuntimeError) -> Self {
        Self::Build(error)
    }
}

/// A builder that imported a prepared plan
/// ([`ImpresspressBuilder::apply_prepared_plan`]).
///
/// It builds only through [`Self::build`], which refuses a
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
    /// The builder, its routes, configs and grants already the plan's.
    builder: ImpresspressBuilder,
    /// The plan's application block inventory, in a plan's order.
    application_blocks: Vec<PreparedBlockImplementation>,
    /// The plan's deployment-owned block settings. Written into the builder's
    /// shared settings handle only once the inventory has matched, so a plan
    /// this runtime refuses changes nothing a caller holds.
    block_settings: BlockSettings,
}

impl AppliedPlanBuilder {
    /// The settings handle the runtime will be built with; see
    /// [`ImpresspressBuilder::block_settings_handle`]. It holds the plan's
    /// settings once [`Self::build`] has succeeded.
    pub fn block_settings_handle(&self) -> Arc<RwLock<BlockSettings>> {
        self.builder.block_settings_handle()
    }

    /// Build the runtime with the plan's settings, configs, routes and
    /// grants, and refuse it unless the application blocks it registered are
    /// the plan's.
    ///
    /// The check runs right after registration — the inventory is not known
    /// before (see the type's doc) — and before anything a caller shares is
    /// changed: the plan's block settings reach the shared handle only after
    /// it passes. The compile's own routes, final configs and grants were
    /// compared with the plan's by the import, so nothing a plan alone needs
    /// is validated here.
    pub fn build(self) -> Result<wafer_run::Wafer, PreparedBuildError> {
        let handle = self.builder.block_settings_handle();
        let registered = self.builder.register()?;
        let mut inventory =
            application_inventory(&registered.declarations, &registered.application);
        crate::prepared_plan::sort_application_blocks(&mut inventory);
        if inventory != self.application_blocks {
            return Err(PreparedBuildError::Plan(
                PreparedPlanError::StructureMismatch(
                    "compiled application block inventory does not match".into(),
                ),
            ));
        }
        *handle.write().map_err(|_| {
            wafer_run::RuntimeError::Config(
                "BlockSettings lock poisoned while installing the plan's settings".into(),
            )
        })? = self.block_settings;
        Ok(registered.wafer)
    }
}

/// The implementations of the application blocks named in `application`, from
/// the runtime's registration-time declarations. Unsorted: a plan's order is
/// [`crate::prepared_plan::sort_application_blocks`].
///
/// `infos` is [`wafer_run::Wafer::block_infos`]: the `BlockInfo` each block's
/// registration read once, so this reads no block's `info()`.
fn application_inventory(
    infos: &[Arc<wafer_run::BlockInfo>],
    application: &[String],
) -> Vec<PreparedBlockImplementation> {
    let application: std::collections::HashSet<&str> =
        application.iter().map(String::as_str).collect();
    infos
        .iter()
        .filter(|info| application.contains(info.name.as_str()))
        .map(|info| PreparedBlockImplementation {
            name: info.name.clone(),
            version: info.version.clone(),
            interface: info.interface.clone(),
            runtime: match info.runtime {
                BlockRuntime::Native => PreparedBlockRuntime::Native,
                BlockRuntime::Wasm => PreparedBlockRuntime::Wasm,
            },
        })
        .collect()
}

/// A builder's block-config list by name. Wafer's registration map has
/// last-write-wins semantics, so a later entry for a name replaces an
/// earlier one here too; the values are canonicalized by `normalize`.
fn config_map(configs: &[(String, serde_json::Value)]) -> BTreeMap<String, serde_json::Value> {
    configs
        .iter()
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect()
}

impl ImpresspressBuilder {
    /// Build the runtime, and the exporter of the plan that describes it.
    ///
    /// This is the candidate-endpoint seam: execute consumer registrations,
    /// call this method, boot the runtime normally, then use the exporter
    /// after boot to package the final seeded structural snapshot. The
    /// exporter's application block inventory is the one `build()`
    /// registered, read from the runtime's registration-time `BlockInfo`s.
    ///
    /// What this builder will put in a plan is validated first, before any
    /// block is registered, so an invalid route, config or grant is refused
    /// before a migration or a seed can run.
    pub fn build_with_plan_exporter(
        self,
    ) -> Result<(wafer_run::Wafer, PreparedPlanExporter), PreparedBuildError> {
        let structure = self
            .descriptor()
            .map_err(PreparedBuildError::InvalidInput)?;
        self.register_with_exporter(structure)
    }

    /// Register the runtime, and an exporter of `structure` completed with
    /// the application blocks it registered.
    fn register_with_exporter(
        self,
        mut structure: PreparedRuntimeStructure,
    ) -> Result<(wafer_run::Wafer, PreparedPlanExporter), PreparedBuildError> {
        let block_settings = self.block_settings.clone();
        // Moved, not copied: the exporter's structure leaves these empty and
        // export reads the live copies.
        let deployment_wrap_grants = Arc::new(RwLock::new(std::mem::take(
            &mut structure.deployment_wrap_grants,
        )));
        let registered = self.register()?;
        structure.application_blocks =
            application_inventory(&registered.declarations, &registered.application);
        structure.normalize();
        // The application blocks are validated now that registration has read
        // them, still before the caller boots — and so before the deploy
        // funnel migrates or seeds anything.
        structure
            .validate()
            .map_err(PreparedBuildError::InvalidInput)?;
        Ok((
            registered.wafer,
            PreparedPlanExporter {
                structure,
                block_settings,
                deployment_wrap_grants,
            },
        ))
    }

    /// What this builder will put in a plan, except the application block
    /// inventory, which only registration knows: normalized and validated
    /// the way a plan is. No block is constructed.
    fn descriptor(&self) -> Result<PreparedRuntimeStructure, PreparedPlanError> {
        let mut structure = PreparedRuntimeStructure {
            application_blocks: Vec::new(),
            routes: self.prepared_routes(),
            built_in_route_count: ROUTES.len(),
            block_settings: BTreeMap::new(),
            block_configs: config_map(&self.block_configs),
            final_block_configs: config_map(&self.final_block_configs),
            wrap_grants: self
                .wrap_grants
                .iter()
                .map(PreparedResourceGrant::from)
                .collect(),
            deployment_wrap_grants: self
                .deployment_wrap_grants
                .iter()
                .map(PreparedResourceGrant::from)
                .collect(),
        };
        structure.normalize();
        structure.validate()?;
        Ok(structure)
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

    /// Verify and apply a prepared plan to the normal builder path.
    ///
    /// Inject the platform services and run the consumer's usual registration
    /// function first, then invoke this method, and build the runtime with
    /// the returned [`AppliedPlanBuilder::build`] — the only
    /// way an imported plan builds. The complete route declaration, final
    /// block configs and consumer grants must match the target compile, or
    /// this refuses the plan; the application block inventory must too, which
    /// `AppliedPlanBuilder::build` checks against the blocks it registers.
    /// Deployment-owned block settings, non-secret structural configs and
    /// deploy-time grants are then replaced from the plan without external
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
        // inventory is compared by `AppliedPlanBuilder::build`,
        // once registration has read it.
        //
        // The rest of the plan is the deployment's, not the compile's, and is
        // taken from it unread below: the block settings, the structural block
        // configs and the deploy-time grants replace this builder's.
        let current = self.descriptor()?;
        if current.routes != plan.structure.routes
            || current.built_in_route_count != plan.structure.built_in_route_count
        {
            return Err(PreparedPlanError::StructureMismatch(
                "compiled built-in/consumer routes do not match".into(),
            ));
        }
        if current.final_block_configs != plan.structure.final_block_configs {
            return Err(PreparedPlanError::StructureMismatch(
                "compiled final block-config overrides do not match".into(),
            ));
        }
        if current.wrap_grants != plan.structure.wrap_grants {
            return Err(PreparedPlanError::StructureMismatch(
                "consumer-declared WRAP grants do not match".into(),
            ));
        }

        // Not written into the shared settings handle here: that happens
        // only once `AppliedPlanBuilder::build` has matched the inventory.
        let block_settings = BlockSettings::from_blocks(
            plan.structure
                .block_settings
                .iter()
                .map(|(name, state)| (name.clone(), state.clone()))
                .collect(),
        );

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
            block_settings,
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
    use std::collections::HashMap;

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
        let database: Arc<dyn wafer_core::interfaces::database::service::DatabaseService> =
            Arc::new(
                wafer_block_sqlite::service::SQLiteDatabaseService::open_in_memory()
                    .expect("open in-memory sqlite"),
            );
        serviced_over(builder, database, false).0
    }

    /// [`serviced`] over `database`, answering the config service the
    /// runtime's boot publishes through. `run_migrations` puts the deploy
    /// funnel's migration switch on both config surfaces.
    fn serviced_over(
        builder: ImpresspressBuilder,
        database: Arc<dyn wafer_core::interfaces::database::service::DatabaseService>,
        run_migrations: bool,
    ) -> (
        ImpresspressBuilder,
        Arc<dyn wafer_core::interfaces::config::service::ConfigService>,
    ) {
        let mut config = crate::builder::RuntimeConfig::new();
        if run_migrations {
            config.both(crate::migration_helper::RUN_MIGRATIONS_KEY, "1");
        }
        let (builder, service) = config
            .install(
                builder.database(database.clone(), wafer_sql_utils::Backend::Sqlite),
                |map| {
                    let service = crate::builder::fill_config_service(
                        Arc::new(wafer_core::service_blocks::config::EnvConfigService::new()),
                        map,
                    );
                    (service.clone(), service)
                },
            )
            .expect("install the config surfaces");
        let builder = builder
            .storage(Arc::new(crate::test_support::InMemoryStorageService::new()))
            .crypto(Arc::new(crate::test_support::real_crypto_service()))
            .network(Arc::new(NoNetwork))
            .logger(Arc::new(wafer_core::service_blocks::logger::TracingLogger))
            .sqlite_db_path(":memory:")
            .model_cache_dir(std::env::temp_dir().join("impresspress-prepared-plan-tests-models"));
        // The hosted targets' config source, over the same database, so a
        // block's `Init` resolves the secrets the deploy funnel generated.
        let variables = Arc::new(crate::config_source::VariablesSnapshot::new(
            crate::config_source::RetryMode::Rebuild,
        ));
        let builder = builder
            .config_source(crate::builder::request_config_source(
                database,
                HashMap::new(),
                variables.clone(),
            ))
            .variables_snapshot(variables);
        (builder, service)
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

    /// The structure an imported plan's runtime is built from — its routes,
    /// configs and grants, the inventory it matched, and the settings the
    /// build left in the shared handle — after building it.
    fn applied_structure(applied: AppliedPlanBuilder) -> PreparedRuntimeStructure {
        let mut structure = applied
            .builder
            .descriptor()
            .expect("the applied builder is valid");
        structure.application_blocks = applied.application_blocks.clone();
        let handle = applied.block_settings_handle();
        applied.build().expect("the runtime builds");
        structure.block_settings = handle.read().expect("settings lock").to_sorted_map();
        structure.normalize();
        structure
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
        let Err(error) = target.build() else {
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
    /// registration (wafer-run#478): importing a plan, building the runtime,
    /// and booting it as a cold isolate does — seal and every block's
    /// `Init` — read none again:
    /// neither a consumer block's (the counted block) nor the feature blocks'
    /// (no use of the block set `blocks::all_block_infos` constructs, through
    /// any wrapper; see `blocks::block_set_sweeps`, which counts this test's
    /// own thread — the build and boot run on it). The plan comes from the
    /// deploy funnel over the same database, as on Cloudflare. Before, the
    /// import and the exporter each constructed the application block
    /// inventory from `info()`.
    #[tokio::test]
    async fn a_prepared_build_reads_each_declaration_once() {
        let database: Arc<dyn wafer_core::interfaces::database::service::DatabaseService> =
            Arc::new(
                wafer_block_sqlite::service::SQLiteDatabaseService::open_in_memory()
                    .expect("open in-memory sqlite"),
            );
        let lock = WaferLockIdentity::absent();
        let counted = |reads: &Arc<std::sync::atomic::AtomicUsize>| -> Arc<dyn wafer_run::Block> {
            Arc::new(Counted(Arc::clone(reads)))
        };

        // The deploy funnel: migrate, seed, and seal the plan.
        let source_reads = Arc::default();
        let (source, config) = serviced_over(
            ImpresspressBuilder::new().extra_block("example/counted", counted(&source_reads)),
            database.clone(),
            true,
        );
        let settings = source.block_settings_handle();
        let (mut wafer, exporter) = source
            .build_with_plan_exporter()
            .expect("the deploy runtime builds");
        let report = crate::builder::boot_deploy_runtime(&mut wafer, &database, &settings, config)
            .await
            .expect("the deploy funnel runs");
        assert!(report.ok, "provisioning must succeed: {report:#?}");
        crate::builder::seal_prepared_plan(&database, &exporter)
            .await
            .expect("the plan seals");
        let plan = exporter
            .prepare_runtime_plan_with_config_generation(
                "example",
                hash('b'),
                "0".repeat(32),
                lock.clone(),
                PreparedReleaseAssets::absent(),
            )
            .expect("the plan exports");
        drop(wafer);
        assert_eq!(
            source_reads.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the deploy funnel reads the declaration only at registration"
        );

        // A cold isolate: import, build, boot, export.
        let reads = Arc::default();
        let sweeps = crate::blocks::block_set_sweeps();
        let (target, config) = serviced_over(
            ImpresspressBuilder::new().extra_block("example/counted", counted(&reads)),
            database.clone(),
            false,
        );
        let target = target
            // As a Worker's request path builds it: its `Serving` boot
            // requires it.
            .init_wait(wafer_run::InitWait::Refuse)
            .apply_prepared_plan(
                &plan,
                "example",
                &hash('b'),
                &lock,
                &PreparedReleaseAssets::absent(),
            )
            .expect("the plan applies");
        let settings = target.block_settings_handle();
        let mut wafer = target.build().expect("the runtime builds");
        crate::builder::boot_request_runtime(
            &mut wafer,
            &database,
            &settings,
            config,
            crate::builder::RequestBuild::Prepared,
        )
        .await
        .expect("the prepared runtime boots");
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(
            crate::blocks::block_set_sweeps(),
            sweeps,
            "a prepared build and boot used the constructed block set"
        );
    }

    /// What a builder would put in a plan is validated before any block is
    /// registered: a secret in a structural block config is refused before
    /// a block is constructed, let alone migrated or seeded.
    #[test]
    fn an_invalid_plan_input_is_refused_before_registration() {
        let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let builder = serviced(
            ImpresspressBuilder::new()
                .block_config("example/counted", serde_json::json!({ "api_key": "k" }))
                .extra_block("example/counted", Arc::new(Counted(Arc::clone(&reads)))),
        );
        let Err(error) = builder.build_with_plan_exporter() else {
            panic!("a secret in a plan's block config must be refused");
        };
        assert!(
            matches!(
                error,
                PreparedBuildError::InvalidInput(PreparedPlanError::InvalidField { .. })
            ),
            "{error:?}"
        );
        assert_eq!(
            reads.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "no block was registered"
        );
    }

    /// A block with no version cannot be in a plan's application inventory.
    /// The deploy funnel's build refuses it once registration has read the
    /// declaration — before it hands back a runtime, so before any boot, any
    /// migration and any seed.
    #[tokio::test]
    async fn a_deploy_build_refuses_an_invalid_application_block_before_migrating() {
        struct Unversioned;

        #[wafer_block::wafer_async_trait]
        impl wafer_run::Block for Unversioned {
            fn info(&self) -> wafer_run::BlockInfo {
                wafer_run::BlockInfo::new("example/unversioned", "", "http-handler@v1", "")
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

        let database: Arc<dyn wafer_core::interfaces::database::service::DatabaseService> =
            Arc::new(
                wafer_block_sqlite::service::SQLiteDatabaseService::open_in_memory()
                    .expect("open in-memory sqlite"),
            );
        let (builder, _config) = serviced_over(
            ImpresspressBuilder::new().extra_block("example/unversioned", Arc::new(Unversioned)),
            database.clone(),
            true,
        );
        let Err(error) = builder.build_with_plan_exporter() else {
            panic!("a block with no version must not reach a plan");
        };
        assert!(
            matches!(
                error,
                PreparedBuildError::InvalidInput(PreparedPlanError::InvalidField { .. })
            ),
            "{error:?}"
        );
        // Test-fixture introspection: the database is as it was given — no
        // migration created a table.
        let tables = database
            .query_raw("SELECT name FROM sqlite_master WHERE type = 'table'", &[])
            .await
            .expect("list tables");
        assert!(tables.is_empty(), "nothing migrated: {tables:?}");
    }

    /// A build failure names its cause in its message, once: every caller
    /// renders it with `to_string()`, and it carries no separate `source()`
    /// a chain-walker would print the cause from again.
    #[test]
    fn a_build_failure_names_its_cause_once() {
        let error = PreparedBuildError::from(wafer_run::RuntimeError::Config(
            "the database service is missing".into(),
        ));
        let rendered = error.to_string();
        assert!(
            rendered.contains("the database service is missing"),
            "{rendered}"
        );
        assert!(std::error::Error::source(&error).is_none());
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
