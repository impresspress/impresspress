//! Stable, immutable deployment-plan data for no-I/O runtime hydration.
//!
//! This module deliberately describes application structure, not a live
//! [`wafer_run::Wafer`]. It contains no service trait objects, bindings,
//! requests, futures, locks, or initialized block state. Executable block
//! factories remain compiled Rust/Wasm code and are parity-checked by the
//! [`crate::builder::ImpresspressBuilder`] import boundary.
//!
//! `wafer.lock` remains the sole source of truth for remote package identity
//! and artifact integrity. The plan records only the exact lock artifact's
//! schema and digest; it never copies the lock's package list.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::features::BlockState;

/// Current deployment-plan schema. Import rejects every other version.
///
/// 2: `PreparedRoute` lost `router_final` (the router's per-path carve-out
/// flag, deleted with the carve-outs) and `refine_undeclared` became
/// required. A plan exported by a schema-1 build is refused rather than read
/// with a field missing: a real one spells `router_final`, which
/// `deny_unknown_fields` rejects during deserialization, and one that does
/// not is rejected by the version gate in `validate`.
pub const PREPARED_RUNTIME_PLAN_SCHEMA_VERSION: u32 = 2;

/// Fail-closed sentinel used by the backwards-compatible structure-only
/// constructor. Deploy tooling must use `new_with_config_generation` with the
/// exact post-funnel KV stamp; this sentinel cannot equal a real stamp.
pub const UNBOUND_CONFIG_GENERATION: &str = "unbound";

/// Worker vars binding the exact candidate/final artifact identities.
pub const PREPARED_APPLICATION_ID_VAR: &str = "IMPRESSPRESS_PREPARED_APPLICATION_ID";
pub const PREPARED_APPLICATION_BUILD_SHA256_VAR: &str =
    "IMPRESSPRESS_PREPARED_APPLICATION_BUILD_SHA256";
pub const PREPARED_WAFER_LOCK_IDENTITY_JSON_VAR: &str =
    "IMPRESSPRESS_PREPARED_WAFER_LOCK_IDENTITY_JSON";
pub const PREPARED_PLAN_HASH_VAR: &str = "IMPRESSPRESS_PREPARED_PLAN_HASH";
pub const PREPARED_PLAN_MODULE_SHA256_VAR: &str = "IMPRESSPRESS_PREPARED_PLAN_MODULE_SHA256";
pub const RELEASE_ASSET_MANIFEST_SHA256_VAR: &str = "IMPRESSPRESS_RELEASE_ASSET_MANIFEST_SHA256";
pub const RELEASE_ASSET_KEYS_SHA256_VAR: &str = "IMPRESSPRESS_RELEASE_ASSET_KEYS_SHA256";

/// Version-bound Worker vars exposing the immutable release asset set: the
/// deploy writes them into the generated `wrangler.toml`, the Worker reads
/// them to resolve release-managed keys without a mutable global pointer.
pub const RELEASE_ASSET_ID_VAR: &str = "IMPRESSPRESS_RELEASE_ASSET_ID";
pub const RELEASE_ASSET_PREFIX_VAR: &str = "IMPRESSPRESS_RELEASE_ASSET_PREFIX";
pub const RELEASE_ASSET_MANIFEST_VAR: &str = "IMPRESSPRESS_RELEASE_ASSET_MANIFEST";

const SHA256_PREFIX: &str = "sha256:";

/// Errors produced while preparing, decoding, or verifying a plan.
#[derive(Debug, thiserror::Error)]
pub enum PreparedPlanError {
    #[error("unsupported prepared-runtime-plan schema {actual}; expected {expected}")]
    UnsupportedSchema { actual: u32, expected: u32 },
    #[error("invalid prepared-runtime-plan field '{field}': {message}")]
    InvalidField {
        field: &'static str,
        message: String,
    },
    #[error("prepared-runtime-plan JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("prepared-runtime-plan hash mismatch: expected {expected}, computed {actual}")]
    PlanHashMismatch { expected: String, actual: String },
    #[error("application id mismatch: plan has '{plan}', runtime expects '{runtime}'")]
    ApplicationMismatch { plan: String, runtime: String },
    #[error("application Wasm/build mismatch: plan has {plan}, runtime expects {runtime}")]
    ApplicationBuildMismatch { plan: String, runtime: String },
    #[error("config generation mismatch: plan has {plan}, current state has {observed}")]
    ConfigGenerationMismatch { plan: String, observed: String },
    #[error("wafer.lock identity mismatch: plan has {plan:?}, runtime expects {runtime:?}")]
    DependencyLockMismatch {
        plan: WaferLockIdentity,
        runtime: WaferLockIdentity,
    },
    #[error("compiled application structure differs from the prepared plan: {0}")]
    StructureMismatch(String),
}

/// Application identity bound into a prepared plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedApplication {
    pub id: String,
    /// Digest of the final staged application Wasm/binary, including prefix.
    pub build_sha256: String,
}

/// Exact identity of the `wafer.lock` input used for preparation.
///
/// Package names, versions, sources, and `wasm_sha256` pins intentionally do
/// not appear here: those remain owned and verified by `wafer.lock` itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum WaferLockIdentity {
    /// The application intentionally has no `wafer.lock` artifact.
    Absent,
    /// An exact, present lock artifact. Package identity remains in the file.
    Present {
        schema_version: u32,
        /// SHA-256 of the exact lock artifact bytes, including prefix.
        sha256: String,
    },
}

impl WaferLockIdentity {
    /// Explicit identity for applications that do not use remote packages and
    /// have no `wafer.lock`. This is distinct from an empty current-schema
    /// lockfile, which is a present artifact and has its own digest.
    pub const fn absent() -> Self {
        Self::Absent
    }

    /// Bind an already parsed lockfile to the exact artifact bytes that were
    /// parsed. The caller remains responsible for parsing TOML; keeping TOML
    /// out of this crate preserves its wasm32-clean dependency surface.
    pub fn from_lockfile_bytes(
        lockfile: &wafer_block::lockfile::Lockfile,
        bytes: &[u8],
    ) -> Result<Self, PreparedPlanError> {
        validate_lockfile(lockfile)?;
        Ok(Self::Present {
            schema_version: lockfile.version,
            sha256: digest(bytes),
        })
    }

    /// Construct from a previously computed exact-artifact digest.
    pub fn from_digest(
        schema_version: u32,
        sha256: impl Into<String>,
    ) -> Result<Self, PreparedPlanError> {
        let identity = Self::Present {
            schema_version,
            sha256: sha256.into(),
        };
        identity.validate()?;
        Ok(identity)
    }

    /// Verify that `bytes` are the exact lock artifact named by this identity.
    pub fn verify_bytes(&self, bytes: &[u8]) -> Result<(), PreparedPlanError> {
        self.validate()?;
        let Self::Present { sha256, .. } = self else {
            return invalid(
                "dependency_lock",
                "cannot verify lock bytes when the declared lock identity is absent",
            );
        };
        let actual = digest(bytes);
        if actual != *sha256 {
            return Err(PreparedPlanError::InvalidField {
                field: "dependency_lock.sha256",
                message: format!("expected {sha256}, computed {actual}"),
            });
        }
        Ok(())
    }

    fn validate(&self) -> Result<(), PreparedPlanError> {
        match self {
            Self::Absent => Ok(()),
            Self::Present {
                schema_version,
                sha256,
            } => {
                if *schema_version != wafer_block::lockfile::SCHEMA_VERSION {
                    return Err(PreparedPlanError::InvalidField {
                        field: "dependency_lock.schema_version",
                        message: format!(
                            "unsupported wafer.lock schema {schema_version}; expected {}",
                            wafer_block::lockfile::SCHEMA_VERSION
                        ),
                    });
                }
                validate_digest("dependency_lock.sha256", sha256)
            }
        }
    }
}

/// Immutable release-asset identity bound into the plan and Worker version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum PreparedReleaseAssets {
    /// The application intentionally packages no external release assets.
    Absent,
    Present {
        /// Content hash/id of the complete logical asset set.
        asset_set_sha256: String,
        /// Immutable R2 prefix, e.g. `.impresspress/releases/v1/<hash>`.
        immutable_prefix: String,
        /// Immutable manifest object key under that prefix.
        manifest_key: String,
        /// Digest of the exact uploaded manifest bytes.
        manifest_sha256: String,
        /// Digest of the canonical logical-key inventory bound in Worker vars.
        logical_keys_sha256: String,
    },
}

impl PreparedReleaseAssets {
    pub const fn absent() -> Self {
        Self::Absent
    }

    pub fn present(
        asset_set_sha256: impl Into<String>,
        immutable_prefix: impl Into<String>,
        manifest_key: impl Into<String>,
        manifest_sha256: impl Into<String>,
        logical_keys_sha256: impl Into<String>,
    ) -> Result<Self, PreparedPlanError> {
        let assets = Self::Present {
            asset_set_sha256: asset_set_sha256.into(),
            immutable_prefix: immutable_prefix.into(),
            manifest_key: manifest_key.into(),
            manifest_sha256: manifest_sha256.into(),
            logical_keys_sha256: logical_keys_sha256.into(),
        };
        assets.validate()?;
        Ok(assets)
    }

    fn validate(&self) -> Result<(), PreparedPlanError> {
        let Self::Present {
            asset_set_sha256,
            immutable_prefix,
            manifest_key,
            manifest_sha256,
            logical_keys_sha256,
        } = self
        else {
            return Ok(());
        };
        validate_digest("release_assets.asset_set_sha256", asset_set_sha256)?;
        validate_digest("release_assets.manifest_sha256", manifest_sha256)?;
        validate_digest("release_assets.logical_keys_sha256", logical_keys_sha256)?;
        if immutable_prefix.trim().is_empty() || manifest_key.trim().is_empty() {
            return invalid(
                "release_assets",
                "immutable_prefix and manifest_key must not be empty",
            );
        }
        let expected_manifest = format!("{}/manifest.json", immutable_prefix.trim_end_matches('/'));
        if manifest_key != &expected_manifest {
            return invalid(
                "release_assets.manifest_key",
                format!("must be '{expected_manifest}'"),
            );
        }
        let raw_asset_hash = asset_set_sha256
            .strip_prefix(SHA256_PREFIX)
            .expect("validated digest prefix");
        if !immutable_prefix
            .trim_end_matches('/')
            .ends_with(raw_asset_hash)
        {
            return invalid(
                "release_assets.immutable_prefix",
                "must end with the raw asset-set digest",
            );
        }
        Ok(())
    }
}

/// Runtime kind used only to parity-check compiled implementation metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreparedBlockRuntime {
    Native,
    Wasm,
}

/// Stable identity of application block code compiled into the target.
///
/// Remote dependency identity is not represented by these entries; the
/// complete remote package set is bound through [`WaferLockIdentity`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedBlockImplementation {
    pub name: String,
    pub version: String,
    pub interface: String,
    pub runtime: PreparedBlockRuntime,
}

/// Serializable route access ladder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreparedRouteAccess {
    Public,
    Authenticated,
    Admin,
}

/// Stable resource-type spelling for deployment-owned WRAP grants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PreparedResourceType {
    Db,
    Config,
    Storage,
    Crypto,
    Network,
    Vector,
    Auth,
    Llm,
    Image,
    Embedding,
}

/// One deployment-owned WRAP grant.
///
/// Block-declared grants remain in each compiled block's `BlockInfo`; this
/// list carries only external/admin-created grants that would otherwise need
/// a D1 read before `Wafer::seal()`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedResourceGrant {
    pub grantee: String,
    pub resource: String,
    #[serde(default)]
    pub write: PreparedGrantWrite,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_type: Option<PreparedResourceType>,
}

/// What a [`PreparedResourceGrant`] lets its grantee do.
///
/// Encoded as `false` / `true` / `"append"` — the same spelling as
/// `wafer_block::types::GrantWrite` — so every plan exported before append
/// grants existed still imports unchanged, and a build that predates them
/// refuses a plan carrying `"append"` rather than reading it as read-only.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum PreparedGrantWrite {
    /// Read-only (`false`).
    #[default]
    Read,
    /// Read-write (`true`).
    ReadWrite,
    /// Append-only on a database collection (`"append"`).
    Append,
}

const PREPARED_GRANT_WRITE_APPEND: &str = "append";

impl Serialize for PreparedGrantWrite {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Read => serializer.serialize_bool(false),
            Self::ReadWrite => serializer.serialize_bool(true),
            Self::Append => serializer.serialize_str(PREPARED_GRANT_WRITE_APPEND),
        }
    }
}

impl<'de> Deserialize<'de> for PreparedGrantWrite {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;

        impl serde::de::Visitor<'_> for Visitor {
            type Value = PreparedGrantWrite;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "a boolean or the string `{PREPARED_GRANT_WRITE_APPEND}`")
            }

            fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<PreparedGrantWrite, E> {
                Ok(if v {
                    PreparedGrantWrite::ReadWrite
                } else {
                    PreparedGrantWrite::Read
                })
            }

            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<PreparedGrantWrite, E> {
                if v == PREPARED_GRANT_WRITE_APPEND {
                    Ok(PreparedGrantWrite::Append)
                } else {
                    Err(E::invalid_value(serde::de::Unexpected::Str(v), &self))
                }
            }
        }

        deserializer.deserialize_any(Visitor)
    }
}

/// One route in matching order. Route order is semantic and is not sorted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedRoute {
    pub prefix: String,
    pub access: PreparedRouteAccess,
    pub block: String,
    pub dispatch_to: String,
    /// Consumer routes only: whether a path the block has not declared an
    /// endpoint for falls back to `Authenticated` rather than standing at
    /// `access` (`routing::ExtraRoute::refined` vs `::new`). Always `false`
    /// for a built-in route, whose undeclared policy is `declared_access`'s
    /// own fail-closed default.
    pub refine_undeclared: bool,
}

/// Immutable, serializable ImpressPress structure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedRuntimeStructure {
    /// Application blocks described by compiled factories/registrations.
    /// Sorted by `(name, version, interface)` during preparation.
    pub application_blocks: Vec<PreparedBlockImplementation>,
    /// Built-in routes followed by consumer routes, preserving matcher order.
    pub routes: Vec<PreparedRoute>,
    /// Number of leading entries in `routes` owned by ImpressPress itself.
    pub built_in_route_count: usize,
    /// Deployment-owned per-block structural settings.
    pub block_settings: BTreeMap<String, BlockState>,
    /// Non-secret structural block configuration only.
    pub block_configs: BTreeMap<String, serde_json::Value>,
    /// Final overrides applied after built-in flow registration.
    pub final_block_configs: BTreeMap<String, serde_json::Value>,
    /// Consumer-declared external grants, sorted and de-duplicated.
    pub wrap_grants: Vec<PreparedResourceGrant>,
    /// Deploy-time admin/external grants captured from durable state.
    pub deployment_wrap_grants: Vec<PreparedResourceGrant>,
}

impl PreparedRuntimeStructure {
    pub(crate) fn normalize(&mut self) {
        self.application_blocks.sort_by(|a, b| {
            (&a.name, &a.version, &a.interface).cmp(&(&b.name, &b.version, &b.interface))
        });
        for value in self.block_configs.values_mut() {
            canonicalize_json(value);
        }
        for value in self.final_block_configs.values_mut() {
            canonicalize_json(value);
        }
        self.wrap_grants.sort();
        self.wrap_grants.dedup();
        self.deployment_wrap_grants.sort();
        self.deployment_wrap_grants.dedup();
    }

    pub fn validate(&self) -> Result<(), PreparedPlanError> {
        if self.built_in_route_count > self.routes.len() {
            return invalid(
                "structure.built_in_route_count",
                "cannot exceed route count",
            );
        }

        let mut block_names = BTreeSet::new();
        for block in &self.application_blocks {
            if block.name.trim().is_empty() {
                return invalid("structure.application_blocks.name", "must not be empty");
            }
            if block.version.trim().is_empty() {
                return invalid(
                    "structure.application_blocks.version",
                    format!("block '{}' has no version", block.name),
                );
            }
            if block.interface.trim().is_empty() {
                return invalid(
                    "structure.application_blocks.interface",
                    format!("block '{}' has no interface", block.name),
                );
            }
            if !block_names.insert(&block.name) {
                return invalid(
                    "structure.application_blocks.name",
                    format!("duplicate block '{}'", block.name),
                );
            }
        }

        let mut prefixes = BTreeSet::new();
        for route in &self.routes {
            if !route.prefix.starts_with('/') {
                return invalid(
                    "structure.routes.prefix",
                    format!("'{}' must start with '/'", route.prefix),
                );
            }
            if route.block.trim().is_empty() || route.dispatch_to.trim().is_empty() {
                return invalid(
                    "structure.routes.block",
                    format!(
                        "route '{}' has an empty block or dispatch target",
                        route.prefix
                    ),
                );
            }
            if !prefixes.insert(&route.prefix) {
                return invalid(
                    "structure.routes.prefix",
                    format!("duplicate route prefix '{}'", route.prefix),
                );
            }
        }

        for (name, value) in &self.block_configs {
            if name.trim().is_empty() {
                return invalid("structure.block_configs", "block name must not be empty");
            }
            reject_secret_values(value, &format!("structure.block_configs.{name}"))?;
        }
        for (name, value) in &self.final_block_configs {
            if name.trim().is_empty() {
                return invalid(
                    "structure.final_block_configs",
                    "block name must not be empty",
                );
            }
            reject_secret_values(value, &format!("structure.final_block_configs.{name}"))?;
        }
        for grant in self
            .wrap_grants
            .iter()
            .chain(self.deployment_wrap_grants.iter())
        {
            if grant.grantee.trim().is_empty() || grant.resource.trim().is_empty() {
                return invalid(
                    "structure.wrap_grants",
                    "grant grantee and resource must not be empty",
                );
            }
        }
        Ok(())
    }
}

/// Versioned, content-addressed deployment artifact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedRuntimePlan {
    pub schema_version: u32,
    pub plan_hash: String,
    /// Exact structural/config stamp persisted after deploy-time mutations.
    /// It is included in `plan_hash` and compared before prepared hydration.
    pub config_generation: String,
    pub application: PreparedApplication,
    pub dependency_lock: WaferLockIdentity,
    pub release_assets: PreparedReleaseAssets,
    pub structure: PreparedRuntimeStructure,
}

/// Mutation-free identity/status returned by a final candidate verification
/// endpoint after it parses and validates the packaged module.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedRuntimePlanSummary {
    pub schema_version: u32,
    pub plan_hash: String,
    pub config_generation: String,
    pub application: PreparedApplication,
    pub dependency_lock: WaferLockIdentity,
    pub release_assets: PreparedReleaseAssets,
    pub application_block_count: usize,
    pub route_count: usize,
    pub block_settings_count: usize,
    pub block_config_count: usize,
    pub final_block_config_count: usize,
    pub declared_wrap_grant_count: usize,
    pub deployment_wrap_grant_count: usize,
}

impl PreparedRuntimePlan {
    pub fn new(
        application_id: impl Into<String>,
        application_build_sha256: impl Into<String>,
        dependency_lock: WaferLockIdentity,
        release_assets: PreparedReleaseAssets,
        structure: PreparedRuntimeStructure,
    ) -> Result<Self, PreparedPlanError> {
        Self::new_with_config_generation(
            application_id,
            application_build_sha256,
            UNBOUND_CONFIG_GENERATION,
            dependency_lock,
            release_assets,
            structure,
        )
    }

    /// Create a deployable plan from the exact generation successfully
    /// persisted after the deploy funnel's final structural/config mutation.
    pub fn new_with_config_generation(
        application_id: impl Into<String>,
        application_build_sha256: impl Into<String>,
        config_generation: impl Into<String>,
        dependency_lock: WaferLockIdentity,
        release_assets: PreparedReleaseAssets,
        mut structure: PreparedRuntimeStructure,
    ) -> Result<Self, PreparedPlanError> {
        structure.normalize();
        let mut plan = Self {
            schema_version: PREPARED_RUNTIME_PLAN_SCHEMA_VERSION,
            plan_hash: String::new(),
            config_generation: config_generation.into(),
            application: PreparedApplication {
                id: application_id.into(),
                build_sha256: application_build_sha256.into(),
            },
            dependency_lock,
            release_assets,
            structure,
        };
        plan.validate_fields()?;
        plan.plan_hash = plan.computed_hash()?;
        Ok(plan)
    }

    /// Decode JSON and fail closed on schema, content, or hash mismatch.
    pub fn from_json(bytes: &[u8]) -> Result<Self, PreparedPlanError> {
        let plan: Self = serde_json::from_slice(bytes)?;
        plan.validate()?;
        Ok(plan)
    }

    /// Decode packaged plan-module bytes and verify both the internal content
    /// hash and the Worker-version metadata bound by the deploy CLI.
    pub fn from_packaged_json(
        bytes: &[u8],
        expected_plan_hash: &str,
        expected_module_sha256: &str,
    ) -> Result<Self, PreparedPlanError> {
        validate_digest("expected_plan_hash", expected_plan_hash)?;
        validate_digest("expected_module_sha256", expected_module_sha256)?;
        let actual_module_sha256 = digest(bytes);
        if actual_module_sha256 != expected_module_sha256 {
            return invalid(
                "prepared_plan_module",
                format!(
                    "module digest mismatch: expected {expected_module_sha256}, computed {actual_module_sha256}"
                ),
            );
        }
        let plan = Self::from_json(bytes)?;
        plan.validate_deployable()?;
        if plan.plan_hash != expected_plan_hash {
            return Err(PreparedPlanError::PlanHashMismatch {
                expected: expected_plan_hash.to_string(),
                actual: plan.plan_hash,
            });
        }
        Ok(plan)
    }

    /// Deterministic human-readable JSON. The terminal newline is part of the
    /// file representation, not the plan hash (which hashes canonical fields).
    pub fn to_json_pretty(&self) -> Result<Vec<u8>, PreparedPlanError> {
        self.validate()?;
        let mut bytes = serde_json::to_vec_pretty(self)?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    pub fn validate(&self) -> Result<(), PreparedPlanError> {
        self.validate_fields()?;
        let actual = self.computed_hash()?;
        if actual != self.plan_hash {
            return Err(PreparedPlanError::PlanHashMismatch {
                expected: self.plan_hash.clone(),
                actual,
            });
        }
        Ok(())
    }

    /// Validate an artifact for packaging/deployment. Structure-only callers
    /// may use [`Self::new`] with the unbound sentinel, but that artifact must
    /// never cross the deployment boundary.
    pub fn validate_deployable(&self) -> Result<(), PreparedPlanError> {
        self.validate()?;
        if self.config_generation == UNBOUND_CONFIG_GENERATION {
            return invalid(
                "config_generation",
                "packaged deployment plans must bind the exact post-funnel KV generation",
            );
        }
        Ok(())
    }

    /// Verify the current durable structural/config generation against this
    /// deployable plan. Callers must supply an existing stamp; this method
    /// performs no I/O and never mints one.
    pub fn verify_config_generation(
        &self,
        observed_generation: &str,
    ) -> Result<(), PreparedPlanError> {
        self.validate_deployable()?;
        if self.config_generation != observed_generation {
            return Err(PreparedPlanError::ConfigGenerationMismatch {
                plan: self.config_generation.clone(),
                observed: observed_generation.to_string(),
            });
        }
        Ok(())
    }

    /// Validate and return the immutable identity/count summary suitable for
    /// smoke verification. This never applies migrations, seeds, or services.
    pub fn summary(&self) -> Result<PreparedRuntimePlanSummary, PreparedPlanError> {
        self.validate()?;
        Ok(PreparedRuntimePlanSummary {
            schema_version: self.schema_version,
            plan_hash: self.plan_hash.clone(),
            config_generation: self.config_generation.clone(),
            application: self.application.clone(),
            dependency_lock: self.dependency_lock.clone(),
            release_assets: self.release_assets.clone(),
            application_block_count: self.structure.application_blocks.len(),
            route_count: self.structure.routes.len(),
            block_settings_count: self.structure.block_settings.len(),
            block_config_count: self.structure.block_configs.len(),
            final_block_config_count: self.structure.final_block_configs.len(),
            declared_wrap_grant_count: self.structure.wrap_grants.len(),
            deployment_wrap_grant_count: self.structure.deployment_wrap_grants.len(),
        })
    }

    /// Verify the target code and dependency artifact before applying the
    /// structure. This is pure and performs no filesystem or network I/O.
    pub fn verify_compatibility(
        &self,
        application_id: &str,
        application_build_sha256: &str,
        dependency_lock: &WaferLockIdentity,
        release_assets: &PreparedReleaseAssets,
    ) -> Result<(), PreparedPlanError> {
        self.validate()?;
        if self.application.id != application_id {
            return Err(PreparedPlanError::ApplicationMismatch {
                plan: self.application.id.clone(),
                runtime: application_id.to_string(),
            });
        }
        validate_digest("application_build_sha256", application_build_sha256)?;
        if self.application.build_sha256 != application_build_sha256 {
            return Err(PreparedPlanError::ApplicationBuildMismatch {
                plan: self.application.build_sha256.clone(),
                runtime: application_build_sha256.to_string(),
            });
        }
        dependency_lock.validate()?;
        if &self.dependency_lock != dependency_lock {
            return Err(PreparedPlanError::DependencyLockMismatch {
                plan: self.dependency_lock.clone(),
                runtime: dependency_lock.clone(),
            });
        }
        release_assets.validate()?;
        if &self.release_assets != release_assets {
            return Err(PreparedPlanError::StructureMismatch(format!(
                "release asset identity mismatch: plan has {:?}, runtime expects {:?}",
                self.release_assets, release_assets
            )));
        }
        Ok(())
    }

    fn validate_fields(&self) -> Result<(), PreparedPlanError> {
        if self.schema_version != PREPARED_RUNTIME_PLAN_SCHEMA_VERSION {
            return Err(PreparedPlanError::UnsupportedSchema {
                actual: self.schema_version,
                expected: PREPARED_RUNTIME_PLAN_SCHEMA_VERSION,
            });
        }
        if self.application.id.trim().is_empty() {
            return invalid("application.id", "must not be empty");
        }
        validate_config_generation(&self.config_generation)?;
        validate_digest("application.build_sha256", &self.application.build_sha256)?;
        if !self.plan_hash.is_empty() {
            validate_digest("plan_hash", &self.plan_hash)?;
        }
        self.dependency_lock.validate()?;
        self.release_assets.validate()?;
        self.structure.validate()
    }

    fn computed_hash(&self) -> Result<String, PreparedPlanError> {
        #[derive(Serialize)]
        struct HashMaterial<'a> {
            schema_version: u32,
            config_generation: &'a str,
            application: &'a PreparedApplication,
            dependency_lock: &'a WaferLockIdentity,
            release_assets: &'a PreparedReleaseAssets,
            structure: &'a PreparedRuntimeStructure,
        }

        let bytes = serde_json::to_vec(&HashMaterial {
            schema_version: self.schema_version,
            config_generation: &self.config_generation,
            application: &self.application,
            dependency_lock: &self.dependency_lock,
            release_assets: &self.release_assets,
            structure: &self.structure,
        })?;
        Ok(digest(&bytes))
    }
}

fn validate_config_generation(value: &str) -> Result<(), PreparedPlanError> {
    if value == UNBOUND_CONFIG_GENERATION {
        return Ok(());
    }
    if value.len() != 32
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return invalid(
            "config_generation",
            "must be the exact 32-character lowercase hexadecimal KV stamp",
        );
    }
    Ok(())
}

fn validate_lockfile(lockfile: &wafer_block::lockfile::Lockfile) -> Result<(), PreparedPlanError> {
    if lockfile.version != wafer_block::lockfile::SCHEMA_VERSION {
        return invalid(
            "wafer.lock.version",
            format!(
                "unsupported schema {}; expected {}",
                lockfile.version,
                wafer_block::lockfile::SCHEMA_VERSION
            ),
        );
    }
    let mut previous: Option<&str> = None;
    for package in &lockfile.packages {
        if let Some(prev) = previous {
            if package.name.as_str() <= prev {
                return invalid(
                    "wafer.lock.package",
                    "packages must be uniquely sorted by name",
                );
            }
        }
        previous = Some(&package.name);
        validate_raw_sha256("wafer.lock.package.sha256", &package.sha256)?;
        validate_raw_sha256("wafer.lock.package.wasm_sha256", &package.wasm_sha256)?;
    }
    Ok(())
}

fn reject_secret_values(value: &serde_json::Value, path: &str) -> Result<(), PreparedPlanError> {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                let normalized = key.to_ascii_lowercase().replace('-', "_");
                let secret_like = normalized
                    .split('_')
                    .any(|part| matches!(part, "secret" | "password" | "token" | "credential"))
                    || normalized.contains("private_key")
                    || normalized.contains("api_key")
                    || normalized.contains("signing_key");
                if secret_like {
                    return invalid(
                        "structure.block_configs",
                        format!(
                            "secret-like field '{path}.{key}' is forbidden; inject the value through request capabilities"
                        ),
                    );
                }
                reject_secret_values(child, &format!("{path}.{key}"))?;
            }
        }
        serde_json::Value::Array(values) => {
            for (index, child) in values.iter().enumerate() {
                reject_secret_values(child, &format!("{path}[{index}]"))?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn canonicalize_json(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for child in map.values_mut() {
                canonicalize_json(child);
            }
            let ordered: BTreeMap<_, _> = std::mem::take(map).into_iter().collect();
            map.extend(ordered);
        }
        serde_json::Value::Array(values) => {
            for child in values {
                canonicalize_json(child);
            }
        }
        _ => {}
    }
}

fn digest(bytes: &[u8]) -> String {
    format!(
        "{SHA256_PREFIX}{}",
        wafer_block::lockfile::sha256_hex(bytes)
    )
}

fn validate_digest(field: &'static str, value: &str) -> Result<(), PreparedPlanError> {
    let Some(raw) = value.strip_prefix(SHA256_PREFIX) else {
        return invalid(field, "must use the 'sha256:<64 lowercase hex>' form");
    };
    validate_raw_sha256(field, raw)
}

fn validate_raw_sha256(field: &'static str, value: &str) -> Result<(), PreparedPlanError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return invalid(field, "must be exactly 64 lowercase hexadecimal characters");
    }
    Ok(())
}

fn invalid<T>(field: &'static str, message: impl Into<String>) -> Result<T, PreparedPlanError> {
    Err(PreparedPlanError::InvalidField {
        field,
        message: message.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(ch: char) -> String {
        format!("sha256:{}", ch.to_string().repeat(64))
    }

    fn structure() -> PreparedRuntimeStructure {
        PreparedRuntimeStructure {
            application_blocks: vec![PreparedBlockImplementation {
                name: "impresspress/system".into(),
                version: "1.0.0".into(),
                interface: "middleware@v1".into(),
                runtime: PreparedBlockRuntime::Native,
            }],
            routes: vec![PreparedRoute {
                prefix: "/health".into(),
                access: PreparedRouteAccess::Public,
                block: "impresspress/system".into(),
                dispatch_to: "impresspress/system".into(),
                refine_undeclared: false,
            }],
            built_in_route_count: 1,
            block_settings: BTreeMap::new(),
            block_configs: BTreeMap::new(),
            final_block_configs: BTreeMap::new(),
            wrap_grants: Vec::new(),
            deployment_wrap_grants: Vec::new(),
        }
    }

    #[test]
    fn deterministic_json_and_hash_round_trip() {
        let lock = WaferLockIdentity::from_digest(2, hash('a')).unwrap();
        let plan = PreparedRuntimePlan::new(
            "example",
            hash('b'),
            lock,
            PreparedReleaseAssets::absent(),
            structure(),
        )
        .unwrap();
        let first = plan.to_json_pretty().unwrap();
        let decoded = PreparedRuntimePlan::from_json(&first).unwrap();
        assert_eq!(plan, decoded);
        assert_eq!(first, decoded.to_json_pretty().unwrap());
    }

    #[test]
    fn content_tamper_fails_hash_validation() {
        let lock = WaferLockIdentity::from_digest(2, hash('a')).unwrap();
        let plan = PreparedRuntimePlan::new(
            "example",
            hash('b'),
            lock,
            PreparedReleaseAssets::absent(),
            structure(),
        )
        .unwrap();
        let mut value = serde_json::to_value(plan).unwrap();
        value["application"]["id"] = serde_json::json!("tampered");
        let error =
            PreparedRuntimePlan::from_json(&serde_json::to_vec(&value).unwrap()).unwrap_err();
        assert!(matches!(error, PreparedPlanError::PlanHashMismatch { .. }));
    }

    #[test]
    fn unknown_schema_fails_closed() {
        let lock = WaferLockIdentity::from_digest(2, hash('a')).unwrap();
        let mut plan = PreparedRuntimePlan::new(
            "example",
            hash('b'),
            lock,
            PreparedReleaseAssets::absent(),
            structure(),
        )
        .unwrap();
        plan.schema_version = 99;
        let error = plan.validate().unwrap_err();
        assert!(matches!(
            error,
            PreparedPlanError::UnsupportedSchema { actual: 99, .. }
        ));
    }

    #[test]
    fn dependency_identity_uses_exact_lock_bytes_without_package_copy() {
        let lockfile = wafer_block::lockfile::Lockfile::new();
        let bytes = b"version = 2\n";
        let identity = WaferLockIdentity::from_lockfile_bytes(&lockfile, bytes).unwrap();
        identity.verify_bytes(bytes).unwrap();
        assert!(identity.verify_bytes(b"version=2\n").is_err());

        let json = serde_json::to_value(identity).unwrap();
        assert!(json.get("package").is_none());
        assert!(json.get("packages").is_none());
    }

    #[test]
    fn absent_lock_is_explicit_and_not_an_empty_lock_digest() {
        let absent = WaferLockIdentity::absent();
        absent.validate().unwrap();
        assert!(absent.verify_bytes(b"").is_err());
        assert_eq!(
            serde_json::to_value(absent).unwrap(),
            serde_json::json!({ "state": "absent" })
        );
    }

    #[test]
    fn secret_like_structural_config_is_rejected() {
        let mut structure = structure();
        structure.block_configs.insert(
            "example/block".into(),
            serde_json::json!({ "api_key": "must-not-be-packaged" }),
        );
        let lock = WaferLockIdentity::from_digest(2, hash('a')).unwrap();
        let error = PreparedRuntimePlan::new(
            "example",
            hash('b'),
            lock,
            PreparedReleaseAssets::absent(),
            structure,
        )
        .unwrap_err();
        assert!(error.to_string().contains("secret-like field"));
    }

    #[test]
    fn summary_reports_validated_artifact_identity_without_mutation() {
        let plan = PreparedRuntimePlan::new_with_config_generation(
            "example",
            hash('b'),
            "1".repeat(32),
            WaferLockIdentity::absent(),
            PreparedReleaseAssets::absent(),
            structure(),
        )
        .unwrap();
        let summary = plan.summary().unwrap();
        assert_eq!(summary.plan_hash, plan.plan_hash);
        assert_eq!(summary.config_generation, "1".repeat(32));
        assert_eq!(summary.route_count, 1);
        assert_eq!(summary.application_block_count, 1);
        assert_eq!(summary.declared_wrap_grant_count, 0);
        assert_eq!(summary.deployment_wrap_grant_count, 0);
    }

    #[test]
    fn packaged_module_digest_and_worker_plan_hash_are_verified() {
        let plan = PreparedRuntimePlan::new_with_config_generation(
            "example",
            hash('b'),
            "2".repeat(32),
            WaferLockIdentity::absent(),
            PreparedReleaseAssets::absent(),
            structure(),
        )
        .unwrap();
        let bytes = plan.to_json_pretty().unwrap();
        let module_hash = digest(&bytes);
        let decoded =
            PreparedRuntimePlan::from_packaged_json(&bytes, &plan.plan_hash, &module_hash).unwrap();
        assert_eq!(decoded.plan_hash, plan.plan_hash);
        assert!(
            PreparedRuntimePlan::from_packaged_json(&bytes, &plan.plan_hash, &hash('f')).is_err()
        );
    }

    #[test]
    fn config_generation_is_hashed_and_unbound_plan_is_not_deployable() {
        let plan = PreparedRuntimePlan::new_with_config_generation(
            "example",
            hash('b'),
            "3".repeat(32),
            WaferLockIdentity::absent(),
            PreparedReleaseAssets::absent(),
            structure(),
        )
        .unwrap();
        let mut tampered = serde_json::to_value(&plan).unwrap();
        tampered["config_generation"] = serde_json::json!("4".repeat(32));
        assert!(matches!(
            PreparedRuntimePlan::from_json(&serde_json::to_vec(&tampered).unwrap()),
            Err(PreparedPlanError::PlanHashMismatch { .. })
        ));

        let unbound = PreparedRuntimePlan::new(
            "example",
            hash('b'),
            WaferLockIdentity::absent(),
            PreparedReleaseAssets::absent(),
            structure(),
        )
        .unwrap();
        assert!(unbound.validate().is_ok());
        assert!(unbound.validate_deployable().is_err());

        plan.verify_config_generation(&"3".repeat(32)).unwrap();
        assert!(matches!(
            plan.verify_config_generation(&"4".repeat(32)),
            Err(PreparedPlanError::ConfigGenerationMismatch { .. })
        ));
    }

    #[test]
    fn release_asset_identity_is_structurally_bound() {
        let raw = "a".repeat(64);
        let prefix = format!(".impresspress/releases/v1/{raw}");
        let assets = PreparedReleaseAssets::present(
            format!("sha256:{raw}"),
            &prefix,
            format!("{prefix}/manifest.json"),
            hash('b'),
            hash('c'),
        )
        .unwrap();
        let plan = PreparedRuntimePlan::new(
            "example",
            hash('d'),
            WaferLockIdentity::absent(),
            assets.clone(),
            structure(),
        )
        .unwrap();
        plan.verify_compatibility("example", &hash('d'), &WaferLockIdentity::absent(), &assets)
            .unwrap();
        assert!(plan
            .verify_compatibility(
                "example",
                &hash('d'),
                &WaferLockIdentity::absent(),
                &PreparedReleaseAssets::absent(),
            )
            .is_err());
    }

    fn exported_plan() -> serde_json::Value {
        let plan = PreparedRuntimePlan::new(
            "example",
            hash('b'),
            WaferLockIdentity::absent(),
            PreparedReleaseAssets::absent(),
            structure(),
        )
        .unwrap();
        serde_json::to_value(plan).unwrap()
    }

    /// A plan exported by a build on schema 1 is refused by the version gate
    /// before any content or hash check, so the error names the version
    /// rather than a mismatch the operator cannot act on.
    #[test]
    fn a_version_1_plan_is_rejected_at_import() {
        let mut exported = exported_plan();
        exported["schema_version"] = serde_json::json!(1);
        let error =
            PreparedRuntimePlan::from_json(&serde_json::to_vec(&exported).unwrap()).unwrap_err();
        assert!(
            matches!(
                error,
                PreparedPlanError::UnsupportedSchema {
                    actual: 1,
                    expected: 2
                }
            ),
            "{error}"
        );
    }

    /// Schema 1 carried `PreparedRoute.router_final`, the router's per-path
    /// carve-out flag. A plan that still spells it is refused as a whole,
    /// and the refusal names the field.
    #[test]
    fn a_plan_exported_with_router_final_is_rejected_at_import() {
        let mut exported = exported_plan();
        exported["schema_version"] = serde_json::json!(1);
        for route in exported["structure"]["routes"].as_array_mut().unwrap() {
            route["router_final"] = serde_json::json!(false);
        }
        let error =
            PreparedRuntimePlan::from_json(&serde_json::to_vec(&exported).unwrap()).unwrap_err();
        assert!(matches!(error, PreparedPlanError::Json(_)), "{error}");
        assert!(error.to_string().contains("router_final"), "{error}");
    }

    #[test]
    fn a_version_2_plan_round_trips_and_carries_no_router_final() {
        assert_eq!(PREPARED_RUNTIME_PLAN_SCHEMA_VERSION, 2);
        let plan = PreparedRuntimePlan::new(
            "example",
            hash('b'),
            WaferLockIdentity::absent(),
            PreparedReleaseAssets::absent(),
            structure(),
        )
        .unwrap();
        assert_eq!(plan.schema_version, 2);

        let exported = serde_json::to_value(&plan).unwrap();
        for route in exported["structure"]["routes"].as_array().unwrap() {
            assert!(route.get("router_final").is_none(), "{route}");
            assert!(route.get("refine_undeclared").is_some(), "{route}");
        }

        let bytes = plan.to_json_pretty().unwrap();
        assert_eq!(PreparedRuntimePlan::from_json(&bytes).unwrap(), plan);
    }
}
