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
        PreparedBlockImplementation, PreparedBlockRuntime, PreparedGrantWrite, PreparedPlanError,
        PreparedReleaseAssets, PreparedResourceGrant, PreparedResourceType, PreparedRoute,
        PreparedRouteAccess, PreparedRuntimePlan, PreparedRuntimeStructure, WaferLockIdentity,
    },
    routing::{ExtraRoute, RouteAccess, ROUTES},
};

/// Deploy-time plan producer detached from the consumed builder.
///
/// A Cloudflare candidate creates this immediately after executing the real
/// consumer registration closure, then consumes the builder with `build()`.
/// The exporter retains only immutable descriptor data plus the shared
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

impl ImpresspressBuilder {
    /// Capture an exporter before `build()` consumes this builder.
    ///
    /// This is the candidate-endpoint seam: execute consumer registrations,
    /// call this method, build+boot normally, then use the exporter after boot
    /// to package the final seeded structural snapshot.
    pub fn prepared_plan_exporter(&self) -> Result<PreparedPlanExporter, PreparedPlanError> {
        let mut application_blocks: Vec<_> = crate::blocks::all_block_infos()
            .into_iter()
            .chain(self.extra_blocks.iter().map(|(_, block)| block.info()))
            .map(|info| PreparedBlockImplementation {
                name: info.name,
                version: info.version,
                interface: info.interface,
                runtime: match info.runtime {
                    BlockRuntime::Native => PreparedBlockRuntime::Native,
                    BlockRuntime::Wasm => PreparedBlockRuntime::Wasm,
                },
            })
            .collect();
        application_blocks.sort_by(|a, b| {
            (&a.name, &a.version, &a.interface).cmp(&(&b.name, &b.version, &b.interface))
        });

        let built_in_route_count = ROUTES.len();
        let routes = ROUTES
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
            .collect();

        // Wafer's registration map has last-write-wins semantics. Mirror that
        // while moving from the builder Vec into deterministic BTreeMap order.
        let mut block_configs = BTreeMap::new();
        for (name, value) in &self.block_configs {
            block_configs.insert(name.clone(), value.clone());
        }
        let mut final_block_configs = BTreeMap::new();
        for (name, value) in &self.final_block_configs {
            final_block_configs.insert(name.clone(), value.clone());
        }

        let exporter = PreparedPlanExporter {
            application_blocks,
            routes,
            built_in_route_count,
            block_settings: self.block_settings.clone(),
            block_configs,
            final_block_configs,
            wrap_grants: self
                .wrap_grants
                .iter()
                .map(PreparedResourceGrant::from)
                .collect(),
            deployment_wrap_grants: Arc::new(RwLock::new(
                self.deployment_wrap_grants
                    .iter()
                    .map(PreparedResourceGrant::from)
                    .collect(),
            )),
        };
        // Fail early for descriptor/config errors, while still allowing the
        // settings handle to be updated after boot.
        exporter.prepared_runtime_structure()?;
        Ok(exporter)
    }

    /// Snapshot the builder's immutable application structure.
    ///
    /// This method performs no filesystem, database, KV, R2, or network I/O.
    /// Service implementations and executable factories are deliberately not
    /// serialized. Their declarative block identities are captured for parity
    /// checking by [`Self::apply_prepared_plan`].
    pub fn prepared_runtime_structure(
        &self,
    ) -> Result<PreparedRuntimeStructure, PreparedPlanError> {
        self.prepared_plan_exporter()?.prepared_runtime_structure()
    }

    /// Create a content-addressed deployment plan from this builder's actual
    /// registrations. The final staged application digest and exact
    /// `wafer.lock` identity are supplied by the deployment host.
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

    /// Create a deployable content-addressed plan bound to the exact
    /// post-funnel KV structural/config generation.
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

    /// Verify and apply a prepared plan to the normal builder path.
    ///
    /// Call the consumer's usual registration function first, then invoke
    /// this method, inject current platform services, and call `build()`. The
    /// application block inventory and complete route declaration must match
    /// the target compile. Deployment-owned block settings and non-secret
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
    ) -> Result<Self, PreparedPlanError> {
        plan.verify_compatibility(
            expected_application_id,
            expected_application_build_sha256,
            expected_dependency_lock,
            expected_release_assets,
        )?;

        let current = self.prepared_runtime_structure()?;
        if current.application_blocks != plan.structure.application_blocks {
            return Err(PreparedPlanError::StructureMismatch(
                "compiled application block inventory does not match".into(),
            ));
        }
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

        Ok(self)
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
mod tests {
    use super::*;
    use crate::features::BlockState;

    fn hash(ch: char) -> String {
        format!("sha256:{}", ch.to_string().repeat(64))
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
        let plan = source
            .prepare_runtime_plan(
                "example",
                hash('b'),
                lock.clone(),
                PreparedReleaseAssets::absent(),
            )
            .unwrap();

        // The target executes the same consumer registration before import.
        let target = ImpresspressBuilder::new().add_route(
            "/app/",
            "example/app",
            crate::routing::RouteAccess::Public,
        );
        let applied = target
            .apply_prepared_plan(
                &plan,
                "example",
                &hash('b'),
                &lock,
                &PreparedReleaseAssets::absent(),
            )
            .unwrap();
        assert_eq!(
            applied.prepared_runtime_structure().unwrap(),
            plan.structure
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
        let plan = source
            .prepare_runtime_plan(
                "example",
                hash('b'),
                lock.clone(),
                PreparedReleaseAssets::absent(),
            )
            .unwrap();

        let target = ImpresspressBuilder::new();
        let Err(error) = target.apply_prepared_plan(
            &plan,
            "example",
            &hash('b'),
            &lock,
            &PreparedReleaseAssets::absent(),
        ) else {
            panic!("route drift should fail plan import");
        };
        assert!(matches!(error, PreparedPlanError::StructureMismatch(_)));
    }

    #[test]
    fn exporter_survives_builder_consumption_and_observes_seeded_settings() {
        let builder = ImpresspressBuilder::new();
        let exporter = builder.prepared_plan_exporter().unwrap();
        drop(builder); // mirrors `build(self)` consuming the candidate builder

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
        let source = ImpresspressBuilder::new();
        let exporter = source.prepared_plan_exporter().unwrap();
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

        let applied = ImpresspressBuilder::new()
            .apply_prepared_plan(
                &plan,
                "example",
                &hash('b'),
                &lock,
                &PreparedReleaseAssets::absent(),
            )
            .unwrap();
        assert_eq!(applied.deployment_wrap_grants.len(), 1);
        assert_eq!(
            applied.prepared_runtime_structure().unwrap(),
            plan.structure
        );
    }

    /// An append-only grant crosses the plan as `"append"` and comes back
    /// append-only — never widened to read-write or narrowed to read-only —
    /// while the boolean spelling every earlier plan used still imports.
    #[test]
    fn an_append_grant_round_trips_through_the_plan() {
        let source = ImpresspressBuilder::new();
        let exporter = source.prepared_plan_exporter().unwrap();
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
        let applied = ImpresspressBuilder::new()
            .apply_prepared_plan(
                &reimported,
                "example",
                &hash('b'),
                &lock,
                &PreparedReleaseAssets::absent(),
            )
            .unwrap();
        assert_eq!(
            applied.deployment_wrap_grants[0].write,
            wafer_block::GrantWrite::Append
        );
        assert_eq!(
            applied.deployment_wrap_grants[0].resource_type,
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
        let source =
            ImpresspressBuilder::new().final_block_config("wafer-run/router", config.clone());
        let plan = source
            .prepare_runtime_plan(
                "example",
                hash('b'),
                lock.clone(),
                PreparedReleaseAssets::absent(),
            )
            .unwrap();
        assert_eq!(
            plan.structure.final_block_configs["wafer-run/router"],
            config
        );

        let matching = ImpresspressBuilder::new()
            .final_block_config("wafer-run/router", config)
            .apply_prepared_plan(
                &plan,
                "example",
                &hash('b'),
                &lock,
                &PreparedReleaseAssets::absent(),
            )
            .unwrap();
        assert_eq!(
            matching.prepared_runtime_structure().unwrap(),
            plan.structure
        );

        let drifted = ImpresspressBuilder::new()
            .final_block_config("wafer-run/router", serde_json::json!({ "routes": [] }));
        assert!(drifted
            .apply_prepared_plan(
                &plan,
                "example",
                &hash('b'),
                &lock,
                &PreparedReleaseAssets::absent(),
            )
            .is_err());
    }

    #[test]
    fn consumer_declared_wrap_grants_are_parity_checked_separately() {
        let declared =
            wafer_run::ResourceGrant::read("impresspress/router", "wafer_run__auth__api_keys");
        let lock = WaferLockIdentity::absent();
        let source = ImpresspressBuilder::new().wrap_grants(vec![declared.clone()]);
        let plan = source
            .prepare_runtime_plan(
                "example",
                hash('b'),
                lock.clone(),
                PreparedReleaseAssets::absent(),
            )
            .unwrap();
        assert_eq!(plan.structure.wrap_grants.len(), 1);
        assert!(plan.structure.deployment_wrap_grants.is_empty());

        ImpresspressBuilder::new()
            .wrap_grants(vec![declared])
            .apply_prepared_plan(
                &plan,
                "example",
                &hash('b'),
                &lock,
                &PreparedReleaseAssets::absent(),
            )
            .unwrap();
        assert!(ImpresspressBuilder::new()
            .apply_prepared_plan(
                &plan,
                "example",
                &hash('b'),
                &lock,
                &PreparedReleaseAssets::absent(),
            )
            .is_err());
    }
}
