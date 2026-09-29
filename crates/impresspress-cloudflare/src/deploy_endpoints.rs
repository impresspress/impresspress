//! The `/_deploy/*` control plane: the operator-driven funnel a deploy POSTs,
//! and the two endpoints that report on (and deeply verify) a packaged prepared
//! runtime plan.
//!
//! Every endpoint here is gated on the deploy-token wrangler secret via
//! [`deploy_token_authorized`]; an unset secret disables the funnel outright.
//! They run before the `*.workers.dev` preview lockdown in
//! `run_with_config`, which is deliberate — the whole atomic deploy happens
//! against an unpromoted preview host.

use std::{collections::HashMap, sync::Arc};

use impresspress_core::{
    builder::{ImpresspressBuilder, DEPLOY_RESPONSE_SCHEMA_VERSION, PREPARE_RUNTIME_PLAN_KEY},
    release_inventory::{ReleaseManifest, RELEASE_MANIFEST_SCHEMA_VERSION},
};
use wafer_core::interfaces::storage::service::StorageService;

use crate::{
    environment::{packaged_prepared_runtime_plan, CfEnvironment},
    kv_cached_db, request_services, runner,
    runtime_build::{boot_deploy_runtime, build_runtime},
    services::{make_kv_backend, make_r2_storage_service},
};

/// Deploy-time init: runs the full migrate+seed funnel once, invoked by
/// `impresspress deploy` against the freshly-uploaded version (pre-promote).
/// Auth: sha256-compare of `X-Deploy-Token` against the
/// [`DEPLOY_TOKEN_KEY`](impresspress_core::config_vars::DEPLOY_TOKEN_KEY)
/// wrangler secret (hash-then-compare sidesteps timing on raw bytes).
#[expect(
    clippy::too_many_arguments,
    reason = "the invocation's captured environment and D1 statement count travel as \
              parameters from the Worker entry that owns them"
)]
pub(crate) async fn deploy_init_endpoint<F, G>(
    req: worker::Request,
    env: worker::Env,
    environment: CfEnvironment,
    queries: &crate::database::D1QueryCount,
    mut request_config: HashMap<String, String>,
    prepare_plan: bool,
    register_blocks: F,
    register_post_build: G,
) -> worker::Result<worker::Response>
where
    F: FnOnce(ImpresspressBuilder) -> Result<ImpresspressBuilder, Box<dyn std::error::Error>>,
    G: FnOnce(
        &mut wafer_run::Wafer,
        Arc<dyn StorageService>,
    ) -> Result<(), Box<dyn std::error::Error>>,
{
    if req.method() != worker::Method::Post {
        return worker::Response::error("method not allowed", 405);
    }
    if environment.deploy_token().is_none() {
        // Secret unset ⇒ endpoint disabled entirely.
        return worker::Response::error("not found", 404);
    }
    if !deploy_token_authorized(&req, &environment) {
        return worker::Response::error("unauthorized", 401);
    }

    // Deploy-time JWT guard: fail fast with an actionable error when the
    // JWT secret is missing or too short, rather than letting the funnel
    // run and every downstream auth op start failing. Reads the same captured
    // value `build_runtime` uses, which is empty when the secret is unbound.
    //
    // This is deliberately narrower than the request path: `crypto_service`'s
    // `jwt()` surfaces a missing/short secret per-operation instead of at
    // worker boot, because a broken-auth deployment beats a boot-looping
    // one (see crypto_service.rs:34-38). That adjudication is unchanged
    // here — this guard only runs inside the operator-driven `/_deploy/init`
    // funnel (a production deploy or `impresspress serve --target cloudflare`),
    // where the operator is watching and fail-fast is the native-parity
    // point; it never runs on the request path.
    let jwt_secret_len = environment.jwt_secret().len();
    if jwt_secret_len < wafer_block_crypto::primitives::MIN_JWT_SECRET_LEN {
        return worker::Response::error(
            format!(
                "{} is missing or too short ({jwt_secret_len} bytes, need at least {}); \
                 set it with: wrangler secret put {}",
                impresspress_core::blocks::auth::JWT_SECRET_KEY,
                wafer_block_crypto::primitives::MIN_JWT_SECRET_LEN,
                impresspress_core::blocks::auth::JWT_SECRET_KEY,
            ),
            500,
        );
    }

    if prepare_plan {
        request_config.insert(PREPARE_RUNTIME_PLAN_KEY.to_string(), "1".to_string());
    }

    // Fresh runtime (never the request cache) with run_migrations forced on,
    // so slot-cached pre-migration outcomes can't leak into the funnel.
    let out = async {
        let mut built = build_runtime(
            &env,
            &environment,
            queries,
            &request_config,
            None,
            register_blocks,
            register_post_build,
            true,
            kv_cached_db::CacheMode {
                read_through: true,
                bump_on_write: false,
            },
        )
        .await?;
        let report = boot_deploy_runtime(&mut built).await?;
        let plan_draft = if prepare_plan && report.ok {
            let final_settings =
                impresspress_core::platform_state::block_settings::load(&built.db).await?;
            let final_grants =
                impresspress_core::platform_state::wrap_grants::load(&built.db).await;
            built.plan_exporter.publish_block_settings(final_settings)?;
            built.plan_exporter.publish_wrap_grants(&final_grants)?;
            let identity = environment.prepared_runtime_identity()?;
            Some((built.plan_exporter.clone(), identity))
        } else {
            None
        };
        Ok::<_, Box<dyn std::error::Error>>((report, plan_draft))
    }
    .await;

    match out {
        Ok((report, plan_draft)) => {
            // Persist one exact generation after every successful funnel.
            // Plan hashing happens only after this write succeeds; there is
            // deliberately no later bump that could invalidate the plan at
            // the instant it is returned.
            let generation = match make_kv_backend(&env, runner::KV_BINDING) {
                Ok(kv) => match kv_cached_db::force_bump_config_version(kv.as_ref()).await {
                    Ok(generation) => generation,
                    Err(error) => {
                        worker::console_log!("post-funnel config-version bump failed: {error}");
                        return worker::Response::error(
                            "deploy_init: config generation persist failed",
                            500,
                        );
                    }
                },
                Err(error) => {
                    worker::console_log!("post-funnel bump failed (KV binding): {error}");
                    return worker::Response::error(
                        "deploy_init: config generation persist failed",
                        500,
                    );
                }
            };
            let plan = match plan_draft {
                Some((exporter, identity)) => match exporter
                    .prepare_runtime_plan_with_config_generation(
                        identity.application_id,
                        identity.application_build_sha256,
                        generation,
                        identity.dependency_lock,
                        identity.release_assets,
                    ) {
                    Ok(plan) => Some(plan),
                    Err(error) => {
                        worker::console_log!("prepared plan finalization failed: {error}");
                        return worker::Response::error(
                            "deploy_init: prepared plan finalization failed",
                            500,
                        );
                    }
                },
                None => None,
            };
            let status = if report.ok { 200 } else { 500 };
            let body = if let Some(plan) = plan {
                serde_json::to_string_pretty(&serde_json::json!({
                    "schema_version": DEPLOY_RESPONSE_SCHEMA_VERSION,
                    "init_report": report,
                    "plan": plan,
                }))
            } else {
                serde_json::to_string_pretty(&report)
            }
            .unwrap_or_else(|e| format!("{{\"serialize_error\":\"{e}\"}}"));
            Ok(worker::Response::ok(body)?.with_status(status))
        }
        Err(e) => {
            // A failed funnel may have committed a prefix of its mutations.
            // Invalidate every older plan even though no replacement plan is
            // emitted. This is the sole bump on this failure path.
            match make_kv_backend(&env, runner::KV_BINDING) {
                Ok(kv) => {
                    if let Err(error) = kv_cached_db::force_bump_config_version(kv.as_ref()).await {
                        worker::console_log!("failed-funnel config-version bump failed: {error}");
                    }
                }
                Err(error) => {
                    worker::console_log!("failed-funnel bump skipped (KV binding): {error}")
                }
            }
            worker::console_log!("deploy_init failed: {e}");
            worker::Response::error(format!("deploy_init: {e}"), 500)
        }
    }
}

/// Constant-time deploy-token authorization shared by control-plane endpoints
/// and the workers.dev preview bypass. The bypass applies to any normal route
/// only when the exact secret is supplied; unauthenticated preview traffic
/// remains a plain 404.
pub(crate) fn deploy_token_authorized(req: &worker::Request, environment: &CfEnvironment) -> bool {
    let presented = req
        .headers()
        .get("X-Deploy-Token")
        .ok()
        .flatten()
        .unwrap_or_default();
    let expected = environment.deploy_token().unwrap_or_default();
    if presented.is_empty() || expected.is_empty() {
        return false;
    }
    let presented_digest = wafer_run::sha256(presented.as_bytes());
    let expected_digest = wafer_run::sha256(expected.as_bytes());
    wafer_block_crypto::primitives::constant_time_eq(&presented_digest, &expected_digest)
}

pub(crate) fn prepared_status_endpoint(
    req: &worker::Request,
    environment: &CfEnvironment,
) -> worker::Result<worker::Response> {
    if req.method() != worker::Method::Get {
        return worker::Response::error("method not allowed", 405);
    }
    if !deploy_token_authorized(req, environment) {
        return worker::Response::error("not found", 404);
    }
    let result = (|| -> Result<_, Box<dyn std::error::Error>> {
        let plan = packaged_prepared_runtime_plan(environment)?
            .ok_or("prepared runtime plan Text module is not installed")?;
        let identity = environment.prepared_runtime_identity()?;
        plan.verify_compatibility(
            &identity.application_id,
            &identity.application_build_sha256,
            &identity.dependency_lock,
            &identity.release_assets,
        )?;
        Ok(plan.summary()?)
    })();
    match result {
        Ok(summary) => worker::Response::ok(serde_json::to_string_pretty(&summary)?),
        Err(error) => {
            worker::console_log!("prepared runtime verification failed: {error}");
            worker::Response::error("prepared runtime verification failed", 500)
        }
    }
}

pub(crate) async fn prepared_verify_endpoint(
    req: &worker::Request,
    env: &worker::Env,
    environment: &CfEnvironment,
) -> worker::Result<worker::Response> {
    if req.method() != worker::Method::Post {
        return worker::Response::error("method not allowed", 405);
    }
    if !deploy_token_authorized(req, environment) {
        return worker::Response::error("not found", 404);
    }
    let result = async {
        let plan = packaged_prepared_runtime_plan(environment)?
            .ok_or("prepared runtime plan Text module is not installed")?;
        let identity = environment.prepared_runtime_identity()?;
        plan.verify_compatibility(
            &identity.application_id,
            &identity.application_build_sha256,
            &identity.dependency_lock,
            &identity.release_assets,
        )?;
        // Verification is mutation-free: a missing generation is a hard
        // failure, never an invitation to mint one. This prevents a final
        // candidate from accepting a plan exported by an older isolate after
        // mutable structural/config state changed.
        let kv = make_kv_backend(env, runner::KV_BINDING)?;
        let observed_generation = kv
            .get(impresspress_core::cache_key::CONFIG_VERSION_KEY)
            .await
            .map_err(|error| format!("read current config generation: {error}"))?
            .ok_or("current config generation is missing")?;
        plan.verify_config_generation(&observed_generation)?;
        let summary = plan.summary()?;

        // Parse the O(1) release routing identity bound into the Worker
        // version. The key inventory itself is fetched and digest-verified
        // from R2 further below.
        let release_routing = request_services::ReleaseAssetIdentity::from_environment(environment)
            .map_err(|error| format!("release routing identity: {error}"))?;
        match (&identity.release_assets, release_routing.as_deref()) {
            (impresspress_core::PreparedReleaseAssets::Absent, None) => {}
            (
                impresspress_core::PreparedReleaseAssets::Present {
                    asset_set_sha256,
                    manifest_key,
                    ..
                },
                Some(routing),
            ) if asset_set_sha256.strip_prefix("sha256:") == Some(routing.id())
                && manifest_key == routing.manifest_key() => {}
            _ => return Err("release routing identity does not match prepared plan".into()),
        }

        let mut release_asset = serde_json::Value::Null;
        if let impresspress_core::PreparedReleaseAssets::Present {
            asset_set_sha256,
            immutable_prefix,
            manifest_key,
            manifest_sha256,
            logical_keys_sha256,
        } = &identity.release_assets
        {
            let storage = make_r2_storage_service(env, runner::R2_BINDING)?;
            let (manifest_folder, manifest_name) = manifest_key
                .rsplit_once('/')
                .ok_or("release manifest key has no folder component")?;
            let (manifest_bytes, _) = storage.get(manifest_folder, manifest_name).await?;
            let actual_manifest_sha256 = format!(
                "sha256:{}",
                impresspress_core::util::sha256_hex(&manifest_bytes)
            );
            if &actual_manifest_sha256 != manifest_sha256 {
                return Err(format!(
                    "release manifest digest mismatch: expected {manifest_sha256}, computed {actual_manifest_sha256}"
                )
                .into());
            }
            let manifest: ReleaseManifest = serde_json::from_slice(&manifest_bytes)?;
            if manifest.schema_version != RELEASE_MANIFEST_SCHEMA_VERSION
                || &manifest.immutable_prefix != immutable_prefix
                || manifest.canonical_asset_set_sha256() != *asset_set_sha256
                || !manifest.logical_keys_strictly_sorted()
            {
                return Err("release manifest identity/order mismatch".into());
            }
            // The manifest must not vouch for itself: re-derive the asset-set
            // digest from the parsed entries and compare against the plan's.
            if format!("sha256:{}", manifest.recomputed_asset_set_sha256()?) != *asset_set_sha256 {
                return Err("release manifest asset-set digest mismatch".into());
            }
            let logical_keys = manifest.logical_keys();
            let routing = release_routing
                .as_ref()
                .ok_or("prepared release has no Worker routing identity")?;
            // Parse the exact R2 KEYS_JSON inventory used by ordinary request
            // reads. Manifest verification alone is insufficient: a
            // self-consistent manifest cannot detect a different routing
            // inventory bound into the Worker version, or an R2 object that
            // has drifted from the digest the Worker is pinned to.
            if routing.keys_sha256() != logical_keys_sha256.as_str() {
                return Err("Worker keys digest differs from prepared plan".into());
            }
            let (keys_folder, keys_name) = routing.keys_location();
            let (keys_bytes, _) = storage.get(keys_folder, keys_name).await?;
            let inventory = impresspress_core::release_inventory::ReleaseInventory::from_json_bytes(
                &keys_bytes,
                logical_keys_sha256,
            )?;
            if inventory.logical_keys_sorted() != logical_keys {
                return Err("R2 key inventory differs from release manifest".into());
            }
            let logical_keys_json = serde_json::to_vec(&logical_keys)?;
            let computed_keys_sha256 = format!(
                "sha256:{}",
                impresspress_core::util::sha256_hex(&logical_keys_json)
            );
            if &computed_keys_sha256 != logical_keys_sha256 {
                return Err("release manifest logical-key digest mismatch".into());
            }

            if let Some(entry) = manifest.files.first() {
                let release = request_services::LoadedRelease {
                    identity: routing.clone(),
                    inventory: std::sync::Arc::new(inventory),
                };
                let immutable_key = release
                    .physical_object_key(&entry.logical_key)
                    .ok_or("representative asset is absent from the R2 key inventory")?;
                let (asset_folder, asset_name) = immutable_key
                    .rsplit_once('/')
                    .ok_or("representative immutable key has no folder component")?;
                let (bytes, _) = storage.get(asset_folder, asset_name).await?;
                let actual_sha256 = impresspress_core::util::sha256_hex(&bytes);
                if actual_sha256 != entry.sha256 {
                    return Err(format!(
                        "representative release asset digest mismatch: expected {}, computed {actual_sha256}",
                        entry.sha256
                    )
                    .into());
                }
                release_asset = serde_json::json!({
                    "logical_key": entry.logical_key,
                    "immutable_key": immutable_key,
                    "sha256": actual_sha256,
                });
            }
        }
        Ok::<_, Box<dyn std::error::Error>>(serde_json::json!({
            "schema_version": DEPLOY_RESPONSE_SCHEMA_VERSION,
            "ok": true,
            "summary": summary,
            "release_asset_verified": true,
            "release_asset": release_asset,
        }))
    }
    .await;

    match result {
        Ok(report) => worker::Response::ok(serde_json::to_string_pretty(&report)?),
        Err(error) => {
            worker::console_log!("prepared runtime deep verification failed: {error}");
            worker::Response::error("prepared runtime deep verification failed", 500)
        }
    }
}
