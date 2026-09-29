//! [`CfEnvironment`] — every `worker::Env` var and secret this crate reads,
//! captured once.
//!
//! Cloudflare hands a Worker its configuration as a JS object with no
//! enumeration API, so every value has to be asked for by name. That made the
//! reads spread out: the JWT secret was read from four functions, the five
//! release-asset vars from three, and each reader decided independently what to
//! do when a binding was absent.
//!
//! The expensive consequence was the **runtime identity hash** — the value that
//! decides whether an isolate may keep serving its cached runtime after the
//! deployment's configuration changed. It was assembled from a hand-written
//! list of ten names living beside, but separate from, the reads themselves.
//! Eight of the eighteen keys the crate reads were missing from it, so rotating
//! the deploy token, moving the asset base URL, flipping the workers.dev opt-in
//! or re-pointing any of the five prepared-runtime vars changed the Worker's
//! behaviour while every warm isolate kept serving a runtime built from the old
//! values. (In practice a real deployment is covered by the
//! `CF_VERSION_METADATA` fast path below, which is why this was a latent hole
//! rather than a live outage: the hand list is only its fallback, for local
//! development and hand-written Wrangler configs that predate the binding.)
//!
//! Now there is one struct with one field per key, one [`capture`] that
//! performs every read, and an [`identity`] derived from the struct's own
//! fields by `serde`'s field enumeration. Adding a key is adding a field, and a
//! field cannot be left out of the identity because nobody writes the identity
//! list.
//!
//! # Security
//!
//! This struct holds raw secret material (`jwt_secret`, `deploy_token`). It
//! deliberately derives neither `Debug` nor `Deserialize`; the one thing that
//! serialises it is [`identity`], which hashes the bytes and drops them. Do not
//! log a `CfEnvironment`, and do not put one in a `ReadyRuntime` — the hash is
//! what is safe to keep.
//!
//! [`capture`]: CfEnvironment::capture
//! [`identity`]: CfEnvironment::identity

use std::collections::HashMap;

use crate::request_services;

/// Name emitted by the generated Wrangler `[version_metadata]` binding.
pub(crate) const VERSION_METADATA_BINDING: &str = "CF_VERSION_METADATA";

/// Worker var (`env.var`) that opts a consumer out of the `*.workers.dev`
/// preview-host lockdown in [`run`](crate::run). Set to `"1"` to serve the full
/// app on a `workers.dev` host (e.g. consumers with no custom domain).
pub(crate) const ALLOW_WORKERS_DEV_KEY: &str = "IMPRESSPRESS_ALLOW_WORKERS_DEV";

/// Worker var (`env.var`) that sets the Cloudflare console logger's minimum
/// emitted level at runtime (`debug`/`info`/`warn`/`error`, case-insensitive —
/// see [`impresspress_core::log_level::LogLevel::parse`]). Unset or unparseable
/// falls back to the compile-time default. See
/// [`make_console_logger`](crate::make_console_logger).
pub(crate) const CF_LOG_LEVEL_KEY: &str = "IMPRESSPRESS_CF_LOG_LEVEL";

/// Worker `Env` bindings that override D1 variables (set via
/// `wrangler secret put`). Most config belongs in D1 so admins can manage it
/// through the dashboard — this list stays short.
pub(crate) const PROTECTED_ENV_KEYS: &[&str] = &[impresspress_core::blocks::auth::JWT_SECRET_KEY];

/// The Worker var naming this deploy's D1 query limit and its Workers Paid
/// default, shared with the CLI that writes it into `wrangler.toml`. Every D1
/// service built in an invocation reports its statement budget against it
/// (see [`crate::database`]'s module docs).
pub(crate) use impresspress_core::config_vars::{
    D1_QUERIES_PER_INVOCATION_DEFAULT, D1_QUERIES_PER_INVOCATION_KEY,
};

/// Shared configuration consumed synchronously while the builder constructs
/// middleware, plus the two operational knobs a deploy sets in
/// `wrangler.toml`.
///
/// Most entries are here because they cannot be deferred to the D1-backed
/// `ConfigSource`: the flow (and the database service) already exist by the
/// time per-block config is resolvable.
///
/// `IMPRESSPRESS_REQUEST_LOG` is the exception, and that reason does **not**
/// apply to it — it is read per request, long after both exist. It is here
/// for a different reason: it is an infrastructure key
/// ([`impresspress_core::config_vars::is_infrastructure_key`]), so
/// `blocks::config` answers it from the boot map whatever the `variables`
/// table holds and `CONFIG_SET` refuses to write it. A Worker var is the only
/// channel it has.
pub(crate) const BUILDER_WORKER_VAR_KEYS: &[&str] = &[
    impresspress_core::config_vars::CORS_ALLOWED_ORIGINS_KEY,
    impresspress_core::config_vars::CSP_DIRECTIVES_KEY,
    impresspress_core::config_vars::REQUEST_LOG_CONFIG_KEY,
    wafer_core::interfaces::database::handler::STRICT_SCHEMA_CONFIG_KEY,
];

/// Every `worker::Env` var and secret this crate reads, as one value.
///
/// One field per key, `None` when the binding is absent. The field order is
/// load-bearing in exactly one place — [`identity`](Self::identity) hashes the
/// serialised struct, so reordering or renaming fields changes the hash and
/// costs every isolate one rebuild. That is the correct trade for the property
/// it buys: a new key cannot be added to the Worker's surface without also
/// entering the identity.
///
/// See the module docs for why this must not be logged.
#[derive(Clone, serde::Serialize)]
pub(crate) struct CfEnvironment {
    /// `CF_VERSION_METADATA`'s version id, and the one field that is not a
    /// var/secret read: Cloudflare versions capture bindings, secrets, code and
    /// compatibility settings together, so when it is present it *is* the
    /// identity and nothing else needs hashing. Empty ids are normalised to
    /// `None` — `"".starts_with(prefix)` is false for every prefix, which would
    /// make the version-preview lockdown fail open.
    worker_version: Option<String>,

    // ── application config that reaches a runtime's config surfaces ──────────
    jwt_secret: Option<String>,
    cors_allowed_origins: Option<String>,
    csp_directives: Option<String>,
    strict_schema: Option<String>,
    request_log: Option<String>,

    // ── operational knobs read by a service constructor or a request guard ───
    cf_log_level: Option<String>,
    asset_base_url: Option<String>,
    allow_workers_dev: Option<String>,
    deploy_token: Option<String>,
    d1_queries_per_invocation: Option<String>,

    // ── the password hasher: read into the crypto service only ──────────────
    // The pepper is not here: it is the password-hasher Worker's, read from
    // that Worker's own secrets (`impresspress_password::protocol`).
    password_hasher_shards: Option<String>,

    // ── the release-asset contract ──────────────────────────────────────────
    release_asset_id: Option<String>,
    release_asset_prefix: Option<String>,
    release_asset_manifest: Option<String>,
    release_asset_manifest_sha256: Option<String>,
    release_asset_keys_sha256: Option<String>,

    // ── the prepared-runtime contract ───────────────────────────────────────
    prepared_application_id: Option<String>,
    prepared_application_build_sha256: Option<String>,
    prepared_wafer_lock_identity_json: Option<String>,
    prepared_plan_hash: Option<String>,
    prepared_plan_module_sha256: Option<String>,
}

fn var(env: &worker::Env, name: &str) -> Option<String> {
    env.var(name).ok().map(|value| value.to_string())
}

fn secret(env: &worker::Env, name: &str) -> Option<String> {
    env.secret(name).ok().map(|value| value.to_string())
}

/// Interpret a bound `WAFER_RUN__DATABASE__STRICT_SCHEMA` value exactly as
/// `wafer-core` does.
///
/// This deliberately mirrors `wafer-core`'s private `config_flag_enabled`
/// (`interfaces/database/handler.rs`), which is what `strict_schema_from`
/// applies to the database block's `Init` config: trimmed, `"true"`
/// case-insensitively or `"1"`, and nothing else. Both readings are of the
/// *same* var — the constructor's, through
/// [`CfEnvironment::strict_schema_enabled`], and `Init`'s, through the
/// `ConfigSource` overlay this environment fills (`BUILDER_WORKER_VAR_KEYS`) —
/// so a value the two disagreed about would put a runtime's own D1 service and
/// its drain handle into different modes.
///
/// It is therefore NOT
/// [`impresspress_core::config_vars::is_truthy`], the repository's general
/// yes/no predicate: that one also accepts `"yes"` and `"on"`, which
/// `wafer-core` reads as disabled. The test
/// `strict_schema_reads_the_flag_the_way_wafer_core_does` in this module's
/// `tests` pins the difference.
fn strict_schema_flag_enabled(value: &str) -> bool {
    let value = value.trim();
    value.eq_ignore_ascii_case("true") || value == "1"
}

impl CfEnvironment {
    /// Read every var and secret this crate consumes, once.
    ///
    /// These are synchronous JS property lookups on the `Env` object — no I/O,
    /// no subrequests — so reading the whole set costs what reading a handful
    /// used to. Capture happens at each entry point that owns a `worker::Env`
    /// ([`run_with_config`](crate::run_with_config) and the two public
    /// constructors that take one) and the captured value travels from there;
    /// `worker::Env` itself keeps travelling alongside it for the D1/KV/R2
    /// *bindings*, which are not var reads.
    pub(crate) fn capture(env: &worker::Env) -> Self {
        Self {
            worker_version: env
                .get_binding::<worker::WorkerVersionMetadata>(VERSION_METADATA_BINDING)
                .ok()
                .map(|metadata| metadata.id())
                .filter(|id| !id.is_empty()),

            jwt_secret: secret(env, impresspress_core::blocks::auth::JWT_SECRET_KEY),
            cors_allowed_origins: var(
                env,
                impresspress_core::config_vars::CORS_ALLOWED_ORIGINS_KEY,
            ),
            csp_directives: var(env, impresspress_core::config_vars::CSP_DIRECTIVES_KEY),
            strict_schema: var(
                env,
                wafer_core::interfaces::database::handler::STRICT_SCHEMA_CONFIG_KEY,
            ),
            request_log: var(env, impresspress_core::config_vars::REQUEST_LOG_CONFIG_KEY),

            cf_log_level: var(env, CF_LOG_LEVEL_KEY),
            asset_base_url: var(env, impresspress_core::ui::assets::ASSET_BASE_URL_VAR),
            allow_workers_dev: var(env, ALLOW_WORKERS_DEV_KEY),
            deploy_token: secret(env, impresspress_core::config_vars::DEPLOY_TOKEN_KEY),
            d1_queries_per_invocation: var(env, D1_QUERIES_PER_INVOCATION_KEY),

            password_hasher_shards: var(env, impresspress_password::protocol::SHARDS_VAR),

            release_asset_id: var(env, request_services::RELEASE_ASSET_ID_VAR),
            release_asset_prefix: var(env, request_services::RELEASE_ASSET_PREFIX_VAR),
            release_asset_manifest: var(env, request_services::RELEASE_ASSET_MANIFEST_VAR),
            release_asset_manifest_sha256: var(
                env,
                impresspress_core::RELEASE_ASSET_MANIFEST_SHA256_VAR,
            ),
            release_asset_keys_sha256: var(env, impresspress_core::RELEASE_ASSET_KEYS_SHA256_VAR),

            prepared_application_id: var(env, impresspress_core::PREPARED_APPLICATION_ID_VAR),
            prepared_application_build_sha256: var(
                env,
                impresspress_core::PREPARED_APPLICATION_BUILD_SHA256_VAR,
            ),
            prepared_wafer_lock_identity_json: var(
                env,
                impresspress_core::PREPARED_WAFER_LOCK_IDENTITY_JSON_VAR,
            ),
            prepared_plan_hash: var(env, impresspress_core::PREPARED_PLAN_HASH_VAR),
            prepared_plan_module_sha256: var(
                env,
                impresspress_core::PREPARED_PLAN_MODULE_SHA256_VAR,
            ),
        }
    }

    /// Request-current identity for every environment value an isolate-cached
    /// runtime was constructed from, plus this request's explicit config.
    ///
    /// The Worker version id is the deployed fast path: a Cloudflare version
    /// captures bindings, secrets/config, code and compatibility settings
    /// together, so an ordinary warm request needs no hashing at all. The
    /// explicit value hash below is only a fallback for local development and
    /// hand-written Wrangler configs that have not adopted the metadata
    /// binding.
    ///
    /// The fallback hashes the serialised struct rather than a list of named
    /// components: `serde`'s derive enumerates the fields, so the set that is
    /// hashed is the set that exists. JSON's own quoting makes that half
    /// unambiguous, which is what the previous length-prefixed hand encoding
    /// was for, and the fixed-length request-config digest appended after it
    /// cannot be confused with it. Raw secret material never leaves this
    /// function.
    pub(crate) fn identity(&self, request_config: &HashMap<String, String>) -> String {
        let request_config_hash = config_identity_hash(request_config);
        if let Some(version_id) = &self.worker_version {
            return format!("worker-version:{version_id}:request-config:{request_config_hash}");
        }

        // Infallible: every field is an `Option<String>`, and `serde_json`
        // cannot fail on those. `expect` rather than a silent default because a
        // default here would give two different environments the same identity.
        let mut encoded =
            serde_json::to_vec(self).expect("CfEnvironment serialises to JSON infallibly");
        encoded.extend_from_slice(request_config_hash.as_bytes());
        impresspress_core::util::sha256_hex(&encoded)
    }

    /// The captured value of a config key this environment owns, or `None` when
    /// the binding is absent.
    ///
    /// "Owns" is [`PROTECTED_ENV_KEYS`] plus [`BUILDER_WORKER_VAR_KEYS`]: those
    /// two lists are what makes a key Env-derived rather than D1-derived, and
    /// they are checked in three places (the structural fill, the warm-request
    /// fill, and the guard that stops consumer request config shadowing a
    /// framework key). `environment_owned_config_keys_all_resolve` pins that
    /// every key on them reaches a field here.
    pub(crate) fn config_value(&self, key: &str) -> Option<&str> {
        let field = if key == impresspress_core::blocks::auth::JWT_SECRET_KEY {
            &self.jwt_secret
        } else if key == impresspress_core::config_vars::CORS_ALLOWED_ORIGINS_KEY {
            &self.cors_allowed_origins
        } else if key == impresspress_core::config_vars::CSP_DIRECTIVES_KEY {
            &self.csp_directives
        } else if key == wafer_core::interfaces::database::handler::STRICT_SCHEMA_CONFIG_KEY {
            &self.strict_schema
        } else if key == impresspress_core::config_vars::REQUEST_LOG_CONFIG_KEY {
            &self.request_log
        } else {
            return None;
        };
        field.as_deref()
    }

    /// Whether a config key comes from the Worker environment rather than from
    /// the D1 `variables` table.
    pub(crate) fn owns_config_key(key: &str) -> bool {
        PROTECTED_ENV_KEYS.contains(&key) || BUILDER_WORKER_VAR_KEYS.contains(&key)
    }

    /// Every config key this environment owns that is actually bound, at the
    /// value THIS request sees.
    ///
    /// Both the cold fill (`structural_config_inputs`) and the warm fill
    /// (`request_config_surfaces`) build their Env-derived half from this, so a
    /// binding that has been removed since a runtime was built is absent from
    /// both by construction rather than deleted back out by name.
    pub(crate) fn config_map(&self) -> HashMap<String, String> {
        PROTECTED_ENV_KEYS
            .iter()
            .chain(BUILDER_WORKER_VAR_KEYS)
            .filter_map(|key| {
                self.config_value(key)
                    .map(|value| ((*key).to_string(), value.to_string()))
            })
            .collect()
    }

    /// Bind the JWT secret. Test-only: production code fills every field
    /// through [`capture`](Self::capture) and nowhere else.
    #[cfg(test)]
    pub(crate) fn set_jwt_secret_for_test(&mut self, value: &str) {
        self.jwt_secret = Some(value.to_string());
    }

    /// Bind `IMPRESSPRESS_PASSWORD_HASHER_SHARDS` to the raw string a Worker
    /// var would carry. Test-only, same rule as above.
    #[cfg(test)]
    pub(crate) fn set_password_hasher_shards_for_test(&mut self, value: &str) {
        self.password_hasher_shards = Some(value.to_string());
    }

    /// Bind `WAFER_RUN__DATABASE__STRICT_SCHEMA` to the raw string a Worker
    /// var would carry. Test-only, same rule as above.
    #[cfg(test)]
    pub(crate) fn set_strict_schema_for_test(&mut self, value: &str) {
        self.strict_schema = Some(value.to_string());
    }

    /// The deployed Worker version id, when the `[version_metadata]` binding is
    /// configured and non-empty.
    pub(crate) fn worker_version(&self) -> Option<&str> {
        self.worker_version.as_deref()
    }

    /// Raw `IMPRESSPRESS_CF_LOG_LEVEL` value, if set.
    pub(crate) fn cf_log_level(&self) -> Option<&str> {
        self.cf_log_level.as_deref()
    }

    /// `IMPRESSPRESS_ASSET_BASE_URL`, the platform override for
    /// [`impresspress_core::ui::assets::base_url`]. `std::env` is stubbed to
    /// always-empty on `wasm32-unknown-unknown`, so `worker::Env` is the only
    /// channel that carries it.
    pub(crate) fn asset_base_url(&self) -> Option<String> {
        self.asset_base_url.clone()
    }

    /// Whether this consumer has opted out of the `*.workers.dev` lockdown for
    /// its canonical worker host. A *version preview* host stays locked either
    /// way — see [`host_policy`](crate::host_policy).
    pub(crate) fn allows_workers_dev(&self) -> bool {
        self.allow_workers_dev.as_deref() == Some("1")
    }

    /// This deploy's `WAFER_RUN__DATABASE__STRICT_SCHEMA` verdict — whether a
    /// SQL backend may trust its migrated schema and skip per-operation
    /// introspection.
    ///
    /// Every D1 service this crate constructs is born with this verdict
    /// already applied (see
    /// [`make_d1_database_service_concrete`](crate::services::make_d1_database_service_concrete)),
    /// because `wafer-run`'s own application of it — `handle_lifecycle`
    /// calling [`DatabaseService::set_strict_schema`] at `Init`, from the
    /// database block's `Init` config — only ever reaches the one service a
    /// *Wafer runtime* was built around. Two D1 services are built outside
    /// any runtime and so are never reached by it: the audit-row write's
    /// batch handle in
    /// `run_with_config` (constructed per request, used inside
    /// `ctx.wait_until`) and the handle `build_runtime` reads
    /// `block_settings` through before it has a runtime to run `Init` on.
    ///
    /// [`DatabaseService::set_strict_schema`]: wafer_core::interfaces::database::service::DatabaseService::set_strict_schema
    pub(crate) fn strict_schema_enabled(&self) -> bool {
        self.strict_schema
            .as_deref()
            .is_some_and(strict_schema_flag_enabled)
    }

    /// How many D1 queries one Worker invocation may run: the bound
    /// `IMPRESSPRESS_D1_QUERIES_PER_INVOCATION`, or
    /// [`D1_QUERIES_PER_INVOCATION_DEFAULT`] when it is unbound.
    ///
    /// A bound value out of
    /// [`parse_d1_queries_per_invocation`](impresspress_core::config_vars::parse_d1_queries_per_invocation)'s
    /// range is an error naming the var, not the default: a deploy that
    /// lowered it for a smaller `limits.subrequests` and mistyped the number
    /// would otherwise run with 1000 and meet the platform's own refusal
    /// part-way through a write instead of the budget's up front.
    pub(crate) fn d1_queries_per_invocation(&self) -> Result<u64, String> {
        let Some(raw) = self.d1_queries_per_invocation.as_deref() else {
            return Ok(D1_QUERIES_PER_INVOCATION_DEFAULT);
        };
        impresspress_core::config_vars::parse_d1_queries_per_invocation(raw)
    }

    /// Bind `IMPRESSPRESS_D1_QUERIES_PER_INVOCATION` to the raw string a
    /// Worker var would carry. Test-only: production code fills every field
    /// through [`capture`](Self::capture) and nowhere else.
    #[cfg(test)]
    pub(crate) fn set_d1_queries_per_invocation_for_test(&mut self, value: &str) {
        self.d1_queries_per_invocation = Some(value.to_string());
    }

    /// How many password-hasher Durable Object instances this Worker spreads
    /// hashing across, from the `IMPRESSPRESS_PASSWORD_HASHER_SHARDS` var the
    /// generated config writes (unset is the default). A value that does not
    /// parse is an error naming the var, and the runtime is not built.
    pub(crate) fn password_hasher_shards(&self) -> Result<u32, String> {
        impresspress_password::protocol::parse_shards(self.password_hasher_shards.as_deref())
    }

    /// The deploy-token secret. `None` disables the `/_deploy/*` control plane
    /// outright.
    pub(crate) fn deploy_token(&self) -> Option<&str> {
        self.deploy_token.as_deref()
    }

    /// The JWT signing secret, empty when unbound — the same "empty default on
    /// error" shape every reader of it used before, so a missing secret
    /// surfaces per-operation from `crypto_service` rather than at Worker boot.
    pub(crate) fn jwt_secret(&self) -> &str {
        self.jwt_secret.as_deref().unwrap_or_default()
    }

    /// The four vars that make up the release-asset routing contract, if any of
    /// them is bound.
    pub(crate) fn release_asset_vars(&self) -> ReleaseAssetVars<'_> {
        ReleaseAssetVars {
            id: self.release_asset_id.as_deref(),
            prefix: self.release_asset_prefix.as_deref(),
            manifest_key: self.release_asset_manifest.as_deref(),
            keys_sha256: self.release_asset_keys_sha256.as_deref(),
        }
    }

    /// The immutable identity a prepared runtime plan is verified against.
    ///
    /// Every var here is *required*: a Worker carrying a packaged plan and a
    /// half-configured identity must fail verification rather than hydrate
    /// against a contract it cannot check.
    pub(crate) fn prepared_runtime_identity(
        &self,
    ) -> Result<PreparedRuntimeIdentity, Box<dyn std::error::Error>> {
        let required = |name: &str,
                        value: Option<&String>|
         -> Result<String, Box<dyn std::error::Error>> {
            let value = value
                .ok_or_else(|| format!("required prepared-runtime Worker var {name} is not set"))?;
            if value.trim().is_empty() {
                return Err(format!("required prepared-runtime Worker var {name} is empty").into());
            }
            Ok(value.clone())
        };

        let application_id = required(
            impresspress_core::PREPARED_APPLICATION_ID_VAR,
            self.prepared_application_id.as_ref(),
        )?;
        let application_build_sha256 = required(
            impresspress_core::PREPARED_APPLICATION_BUILD_SHA256_VAR,
            self.prepared_application_build_sha256.as_ref(),
        )?;
        let dependency_lock = serde_json::from_str(&required(
            impresspress_core::PREPARED_WAFER_LOCK_IDENTITY_JSON_VAR,
            self.prepared_wafer_lock_identity_json.as_ref(),
        )?)?;

        let release_assets = match self.release_asset_id.as_deref() {
            Some(asset_id) if !asset_id.is_empty() => {
                let asset_set_sha256 = if asset_id.starts_with("sha256:") {
                    asset_id.to_string()
                } else {
                    format!("sha256:{asset_id}")
                };
                impresspress_core::PreparedReleaseAssets::present(
                    asset_set_sha256,
                    required(
                        request_services::RELEASE_ASSET_PREFIX_VAR,
                        self.release_asset_prefix.as_ref(),
                    )?,
                    required(
                        request_services::RELEASE_ASSET_MANIFEST_VAR,
                        self.release_asset_manifest.as_ref(),
                    )?,
                    required(
                        impresspress_core::RELEASE_ASSET_MANIFEST_SHA256_VAR,
                        self.release_asset_manifest_sha256.as_ref(),
                    )?,
                    required(
                        impresspress_core::RELEASE_ASSET_KEYS_SHA256_VAR,
                        self.release_asset_keys_sha256.as_ref(),
                    )?,
                )?
            }
            _ => impresspress_core::PreparedReleaseAssets::absent(),
        };

        Ok(PreparedRuntimeIdentity {
            application_id,
            application_build_sha256,
            dependency_lock,
            release_assets,
        })
    }
}

/// The four release-routing vars, borrowed out of a [`CfEnvironment`] so
/// `request_services` can parse them without a second set of reads.
pub(crate) struct ReleaseAssetVars<'a> {
    pub(crate) id: Option<&'a str>,
    pub(crate) prefix: Option<&'a str>,
    pub(crate) manifest_key: Option<&'a str>,
    pub(crate) keys_sha256: Option<&'a str>,
}

#[derive(Clone)]
pub(crate) struct PreparedRuntimeIdentity {
    pub(crate) application_id: String,
    pub(crate) application_build_sha256: String,
    pub(crate) dependency_lock: impresspress_core::WaferLockIdentity,
    pub(crate) release_assets: impresspress_core::PreparedReleaseAssets,
}

thread_local! {
    /// Verified immutable structure only. No Env, binding, or request I/O
    /// object enters this isolate-local cache.
    ///
    /// [`IdentityCache`] rather than a `RefCell`: this cell is read by every
    /// request that reaches the prepared path, and it is populated by the
    /// single most expensive synchronous step on a cold prepared request
    /// (pull the plan module out of JS, SHA-256 it, parse it, canonicalize
    /// it, re-hash it). Cloudflare can hard-stop a request anywhere in that
    /// window without running a destructor, and a `RefCell` borrow stranded
    /// that way stays set for the life of the isolate — turning every
    /// subsequent request in it into a `panic` → `abort` → wasm trap taken
    /// inside `poll`, whose response promise is never settled. See
    /// `impresspress_core::isolate_cell`'s module documentation for the full
    /// mechanism, and `runtime_cache`'s `BUILD_LEASE_MS` for the same
    /// premise applied to a `Cell<bool>`.
    static PREPARED_PLAN_CACHE: impresspress_core::IdentityCache<impresspress_core::PreparedRuntimePlan> =
        const { impresspress_core::IdentityCache::new() };
}

/// Read the immutable Text-module payload installed by the final Worker shim.
/// The candidate upload intentionally has no such global and returns `None`.
#[cfg(target_arch = "wasm32")]
pub(crate) fn packaged_prepared_runtime_plan(
    environment: &CfEnvironment,
) -> Result<Option<std::rc::Rc<impresspress_core::PreparedRuntimePlan>>, Box<dyn std::error::Error>>
{
    let value = js_sys::Reflect::get(
        &js_sys::global(),
        &wasm_bindgen::JsValue::from_str("__IMPRESSPRESS_PREPARED_RUNTIME_PLAN"),
    )
    .map_err(|e| format!("read prepared runtime Text module global: {e:?}"))?;
    // Candidate Workers have no Text-module global. Avoid requiring final-only
    // digest vars there, while stable final Workers can check their cheap
    // identity key before allocating the module string.
    if !value.is_string() {
        return Ok(None);
    }
    let expected_module_sha256 = environment
        .prepared_plan_module_sha256
        .clone()
        .ok_or("prepared plan module digest Worker var is not set")?;
    let expected_plan_hash = environment
        .prepared_plan_hash
        .clone()
        .ok_or("prepared plan hash Worker var is not set")?;
    let worker_version = environment
        .worker_version()
        .unwrap_or("no-version-metadata")
        .to_string();
    let cache_key = format!("{worker_version}\n{expected_plan_hash}\n{expected_module_sha256}");
    // Decode OUTSIDE the cache's critical section, exactly as
    // `ReleaseAssetIdentity::from_environment` does: look up, release, verify,
    // store. The integrity checks below are unchanged and unconditional — a
    // cache hit is only ever a value that already passed them under this same
    // Worker version, plan hash, and module digest.
    let plan = PREPARED_PLAN_CACHE.with(|cache| {
        cache.get_or_try_insert_with(cache_key, || {
            let json = value
                .as_string()
                .filter(|json| !json.trim().is_empty())
                .ok_or_else(|| "prepared runtime Text module is empty".to_string())?;
            impresspress_core::PreparedRuntimePlan::from_packaged_json(
                json.as_bytes(),
                &expected_plan_hash,
                &expected_module_sha256,
            )
            .map_err(|error| error.to_string())
        })
    })?;
    Ok(Some(plan))
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn packaged_prepared_runtime_plan(
    _environment: &CfEnvironment,
) -> Result<Option<std::rc::Rc<impresspress_core::PreparedRuntimePlan>>, Box<dyn std::error::Error>>
{
    Ok(None)
}

pub(crate) fn config_identity_hash(config: &HashMap<String, String>) -> String {
    let mut entries: Vec<_> = config.iter().collect();
    entries.sort_unstable_by(|left, right| left.0.cmp(right.0));
    let mut encoded = Vec::new();
    for (key, value) in entries {
        encoded.extend_from_slice(&(key.len() as u64).to_le_bytes());
        encoded.extend_from_slice(key.as_bytes());
        encoded.extend_from_slice(&(value.len() as u64).to_le_bytes());
        encoded.extend_from_slice(value.as_bytes());
    }
    impresspress_core::util::sha256_hex(&encoded)
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::CfEnvironment;

    /// A [`CfEnvironment`] with every field unbound, for tests that set one
    /// field at a time. Not `Default`: production code must obtain one from
    /// [`CfEnvironment::capture`] and nowhere else.
    pub(crate) fn empty_environment() -> CfEnvironment {
        CfEnvironment {
            worker_version: None,
            jwt_secret: None,
            cors_allowed_origins: None,
            csp_directives: None,
            strict_schema: None,
            request_log: None,
            cf_log_level: None,
            asset_base_url: None,
            allow_workers_dev: None,
            deploy_token: None,
            d1_queries_per_invocation: None,
            password_hasher_shards: None,
            release_asset_id: None,
            release_asset_prefix: None,
            release_asset_manifest: None,
            release_asset_manifest_sha256: None,
            release_asset_keys_sha256: None,
            prepared_application_id: None,
            prepared_application_build_sha256: None,
            prepared_wafer_lock_identity_json: None,
            prepared_plan_hash: None,
            prepared_plan_module_sha256: None,
        }
    }

    /// Every captured field, as a setter that stores a distinguishable value.
    ///
    /// This is the list `identity_changes_when_any_captured_field_changes`
    /// walks. It is written out by hand *on purpose*: it is the test's
    /// independent statement of what the struct holds, so a field added to
    /// `CfEnvironment` and forgotten here fails
    /// `every_captured_field_is_covered_by_the_identity_table` on the field
    /// count rather than passing silently.
    #[expect(
        clippy::type_complexity,
        reason = "the setter table is written out in full on purpose; a type alias \
                  would hide the shape the field-count assertion reads"
    )]
    pub(crate) fn mutators() -> Vec<(&'static str, fn(&mut CfEnvironment))> {
        vec![
            ("worker_version", |e| {
                e.worker_version = Some("v".to_string())
            }),
            ("jwt_secret", |e| e.jwt_secret = Some("v".to_string())),
            ("cors_allowed_origins", |e| {
                e.cors_allowed_origins = Some("v".to_string())
            }),
            ("csp_directives", |e| {
                e.csp_directives = Some("v".to_string())
            }),
            ("strict_schema", |e| e.strict_schema = Some("v".to_string())),
            ("request_log", |e| e.request_log = Some("v".to_string())),
            ("cf_log_level", |e| e.cf_log_level = Some("v".to_string())),
            ("asset_base_url", |e| {
                e.asset_base_url = Some("v".to_string())
            }),
            ("allow_workers_dev", |e| {
                e.allow_workers_dev = Some("v".to_string())
            }),
            ("deploy_token", |e| e.deploy_token = Some("v".to_string())),
            ("d1_queries_per_invocation", |e| {
                e.d1_queries_per_invocation = Some("v".to_string())
            }),
            ("password_hasher_shards", |e| {
                e.password_hasher_shards = Some("v".to_string())
            }),
            ("release_asset_id", |e| {
                e.release_asset_id = Some("v".to_string())
            }),
            ("release_asset_prefix", |e| {
                e.release_asset_prefix = Some("v".to_string())
            }),
            ("release_asset_manifest", |e| {
                e.release_asset_manifest = Some("v".to_string())
            }),
            ("release_asset_manifest_sha256", |e| {
                e.release_asset_manifest_sha256 = Some("v".to_string())
            }),
            ("release_asset_keys_sha256", |e| {
                e.release_asset_keys_sha256 = Some("v".to_string())
            }),
            ("prepared_application_id", |e| {
                e.prepared_application_id = Some("v".to_string())
            }),
            ("prepared_application_build_sha256", |e| {
                e.prepared_application_build_sha256 = Some("v".to_string())
            }),
            ("prepared_wafer_lock_identity_json", |e| {
                e.prepared_wafer_lock_identity_json = Some("v".to_string())
            }),
            ("prepared_plan_hash", |e| {
                e.prepared_plan_hash = Some("v".to_string())
            }),
            ("prepared_plan_module_sha256", |e| {
                e.prepared_plan_module_sha256 = Some("v".to_string())
            }),
        ]
    }
}

#[cfg(test)]
mod tests {
    use impresspress_password::pepper::{
        PASSWORD_PEPPER_KEY_VAR, PASSWORD_PEPPER_PREVIOUS_KEYS_VAR, PASSWORD_PEPPER_REQUIRED_VAR,
    };
    use wasm_bindgen::JsCast;
    use wasm_bindgen_test::wasm_bindgen_test;

    use super::{test_support::*, *};

    /// A `worker::Env` fake: a JS `Proxy` over an object of string bindings
    /// whose `get` trap counts every property lookup by name.
    ///
    /// `worker::Env` is a `#[wasm_bindgen] extern "C"` type — a `JsValue`
    /// newtype — and both `env.var` and `env.secret` resolve through
    /// `js_sys::Reflect::get`, so a `Proxy` sees every read this crate makes.
    struct RecordingEnv {
        env: worker::Env,
        reads: std::rc::Rc<std::cell::RefCell<Vec<String>>>,
        // Kept alive for as long as the Proxy can call it.
        _trap: wasm_bindgen::closure::Closure<
            dyn FnMut(js_sys::Object, wasm_bindgen::JsValue) -> wasm_bindgen::JsValue,
        >,
    }

    impl RecordingEnv {
        fn new(bindings: &[(&str, &str)]) -> Self {
            let target = js_sys::Object::new();
            for (name, value) in bindings {
                js_sys::Reflect::set(
                    &target,
                    &wasm_bindgen::JsValue::from_str(name),
                    &wasm_bindgen::JsValue::from_str(value),
                )
                .expect("set a property on a plain object");
            }

            let reads = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
            let recorded = reads.clone();
            let trap = wasm_bindgen::closure::Closure::new(
                move |obj: js_sys::Object, key: wasm_bindgen::JsValue| -> wasm_bindgen::JsValue {
                    if let Some(name) = key.as_string() {
                        recorded.borrow_mut().push(name);
                    }
                    js_sys::Reflect::get(&obj, &key).unwrap_or(wasm_bindgen::JsValue::UNDEFINED)
                },
            );

            let handler = js_sys::Object::new();
            js_sys::Reflect::set(
                &handler,
                &wasm_bindgen::JsValue::from_str("get"),
                trap.as_ref(),
            )
            .expect("install the get trap");

            let proxy = js_sys::Proxy::new(&target, &handler);
            Self {
                env: wasm_bindgen::JsValue::from(proxy).unchecked_into::<worker::Env>(),
                reads,
                _trap: trap,
            }
        }

        fn reads_of(&self, name: &str) -> usize {
            self.reads
                .borrow()
                .iter()
                .filter(|read| read.as_str() == name)
                .count()
        }
    }

    /// Every key the Worker reads, and a value to bind it to. The point of the
    /// list is that `capture` asks for each name EXACTLY once: the reads used
    /// to be spread over five functions, and the JWT secret alone was read four
    /// times on a cold `/_deploy/init`.
    fn every_key() -> Vec<(&'static str, &'static str)> {
        vec![
            (impresspress_core::blocks::auth::JWT_SECRET_KEY, "jwt"),
            (
                impresspress_core::config_vars::CORS_ALLOWED_ORIGINS_KEY,
                "https://example.test",
            ),
            (
                impresspress_core::config_vars::CSP_DIRECTIVES_KEY,
                "default-src 'self'",
            ),
            (
                wafer_core::interfaces::database::handler::STRICT_SCHEMA_CONFIG_KEY,
                "1",
            ),
            (
                impresspress_core::config_vars::REQUEST_LOG_CONFIG_KEY,
                "errors",
            ),
            (CF_LOG_LEVEL_KEY, "debug"),
            (
                impresspress_core::ui::assets::ASSET_BASE_URL_VAR,
                "https://cdn.example.test",
            ),
            (ALLOW_WORKERS_DEV_KEY, "1"),
            (impresspress_core::config_vars::DEPLOY_TOKEN_KEY, "token"),
            (D1_QUERIES_PER_INVOCATION_KEY, "50"),
            (impresspress_password::protocol::SHARDS_VAR, "5"),
            (
                request_services::RELEASE_ASSET_ID_VAR,
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            ),
            (
                request_services::RELEASE_ASSET_PREFIX_VAR,
                ".impresspress/releases/v1/immutable/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            ),
            (
                request_services::RELEASE_ASSET_MANIFEST_VAR,
                ".impresspress/releases/v1/immutable/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/manifest.json",
            ),
            (
                impresspress_core::RELEASE_ASSET_MANIFEST_SHA256_VAR,
                "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            ),
            (
                impresspress_core::RELEASE_ASSET_KEYS_SHA256_VAR,
                "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
            ),
            (impresspress_core::PREPARED_APPLICATION_ID_VAR, "app"),
            (
                impresspress_core::PREPARED_APPLICATION_BUILD_SHA256_VAR,
                "sha256:d",
            ),
            (
                impresspress_core::PREPARED_WAFER_LOCK_IDENTITY_JSON_VAR,
                r#"{"state":"absent"}"#,
            ),
            (impresspress_core::PREPARED_PLAN_HASH_VAR, "planhash"),
            (
                impresspress_core::PREPARED_PLAN_MODULE_SHA256_VAR,
                "modhash",
            ),
        ]
    }

    #[wasm_bindgen_test]
    fn capture_reads_every_key_exactly_once() {
        let bindings = every_key();
        let env = RecordingEnv::new(&bindings);
        let captured = CfEnvironment::capture(&env.env);

        for (name, _) in &bindings {
            assert_eq!(
                env.reads_of(name),
                1,
                "{name} must be read exactly once per capture",
            );
        }
        assert_eq!(
            env.reads_of(VERSION_METADATA_BINDING),
            1,
            "the version-metadata binding is also read once",
        );

        // Every read landed in a field: capture is what makes the struct the
        // single source, so a key read and then dropped would be worse than not
        // reading it.
        assert_eq!(captured.jwt_secret(), "jwt");
        assert_eq!(captured.cf_log_level(), Some("debug"));
        assert_eq!(
            captured.asset_base_url().as_deref(),
            Some("https://cdn.example.test")
        );
        assert!(captured.allows_workers_dev());
        assert_eq!(captured.deploy_token(), Some("token"));
        assert_eq!(captured.d1_queries_per_invocation(), Ok(50));
        assert_eq!(captured.password_hasher_shards(), Ok(5));
        assert_eq!(
            captured.release_asset_vars().prefix,
            Some(".impresspress/releases/v1/immutable/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );
        let prepared = captured
            .prepared_runtime_identity()
            .expect("a fully bound prepared-runtime contract parses");
        assert_eq!(prepared.application_id, "app");
    }

    /// The STRICT_SCHEMA verdict every D1 service is constructed with must be
    /// the one `wafer-core` would reach from the same string, because
    /// `handle_lifecycle` re-applies the var from the database block's `Init`
    /// config on the runtime's own service. If the two readings disagreed,
    /// that service and the audit-row write's handle would run in different
    /// modes off one var.
    ///
    /// `"yes"` and `"on"` are the rows that matter: they are true for
    /// [`impresspress_core::config_vars::is_truthy`], the repository's general
    /// yes/no predicate, and false for `wafer-core`'s `config_flag_enabled`.
    /// Swapping `strict_schema_flag_enabled` for `is_truthy` fails here.
    #[wasm_bindgen_test]
    fn strict_schema_reads_the_flag_the_way_wafer_core_does() {
        let key = wafer_core::interfaces::database::handler::STRICT_SCHEMA_CONFIG_KEY;
        let enabled_by = ["true", "TRUE", "True", " true ", "1", " 1 "];
        let disabled_by = ["", " ", "0", "false", "yes", "on", "YES", "ON", "enabled"];

        for value in enabled_by {
            let env = RecordingEnv::new(&[(key, value)]);
            assert!(
                CfEnvironment::capture(&env.env).strict_schema_enabled(),
                "{value:?} enables STRICT_SCHEMA for wafer-core, so it must here",
            );
        }
        for value in disabled_by {
            let env = RecordingEnv::new(&[(key, value)]);
            assert!(
                !CfEnvironment::capture(&env.env).strict_schema_enabled(),
                "{value:?} does NOT enable STRICT_SCHEMA for wafer-core, so it \
                 must not here",
            );
        }

        let unbound = RecordingEnv::new(&[]);
        assert!(
            !CfEnvironment::capture(&unbound.env).strict_schema_enabled(),
            "an unset var is off, matching wafer-core's `is_some_and`",
        );
    }

    /// The D1 query limit: unset is the default 1000, a bound number in range
    /// is taken as it is (a deploy that lowered it, here to 50), and anything
    /// else — not a whole number, at or below the audit-row reservation,
    /// above D1's maximum — is an error naming the var rather than a silent
    /// fallback to the default.
    #[wasm_bindgen_test]
    fn the_d1_query_limit_defaults_to_1000_and_refuses_a_malformed_value() {
        let unbound = RecordingEnv::new(&[]);
        assert_eq!(
            CfEnvironment::capture(&unbound.env).d1_queries_per_invocation(),
            Ok(D1_QUERIES_PER_INVOCATION_DEFAULT)
        );
        for (raw, limit) in [("50", 50), (" 50 ", 50), ("1000", 1000)] {
            let env = RecordingEnv::new(&[(D1_QUERIES_PER_INVOCATION_KEY, raw)]);
            assert_eq!(
                CfEnvironment::capture(&env.env).d1_queries_per_invocation(),
                Ok(limit),
                "{raw:?}"
            );
        }
        // Refused from the var itself, not only by the CLI: a
        // `wrangler_overrides_path` file sets it past the CLI's check. At or
        // below the audit-row reservation, or above D1's maximum of 1000.
        let reservation = impresspress_core::after_response::AUDIT_ROW_STATEMENTS.to_string();
        for raw in ["", "0", "-5", "5O", "fifty", "50.5", "1001", &reservation] {
            let env = RecordingEnv::new(&[(D1_QUERIES_PER_INVOCATION_KEY, raw)]);
            let err = CfEnvironment::capture(&env.env)
                .d1_queries_per_invocation()
                .expect_err(raw);
            assert!(
                err.contains(D1_QUERIES_PER_INVOCATION_KEY),
                "{raw:?}: {err}"
            );
        }
    }

    /// An absent binding is `None`, not an empty string, and reading it still
    /// costs exactly one lookup.
    #[wasm_bindgen_test]
    fn an_unbound_key_is_absent_rather_than_empty() {
        let env = RecordingEnv::new(&[]);
        let captured = CfEnvironment::capture(&env.env);

        assert_eq!(captured.cf_log_level(), None);
        assert_eq!(captured.deploy_token(), None);
        assert!(!captured.allows_workers_dev());
        assert_eq!(
            captured.config_map(),
            HashMap::new(),
            "no binding means no config key, so nothing stale can be inherited",
        );
        assert_eq!(
            env.reads_of(impresspress_core::blocks::auth::JWT_SECRET_KEY),
            1
        );
    }

    /// The whole point of the struct: a field that changes must move the
    /// identity, or an isolate keeps serving a runtime built from the old
    /// value.
    ///
    /// Eight rows of this table failed against the hand-written ten-component
    /// list this replaced: `deploy_token`, `asset_base_url`,
    /// `allow_workers_dev` and the five `prepared_*` vars.
    #[wasm_bindgen_test]
    fn identity_changes_when_any_captured_field_changes() {
        let request_config = HashMap::new();
        let base = empty_environment();
        let baseline = base.identity(&request_config);

        for (field, mutate) in mutators() {
            let mut changed = base.clone();
            mutate(&mut changed);
            assert_ne!(
                changed.identity(&request_config),
                baseline,
                "changing `{field}` must change the runtime identity, or a warm \
                 isolate keeps serving a runtime built from the old value",
            );
        }
    }

    /// The table above is only as good as its coverage, and it is written by
    /// hand. `serde` enumerates the real fields, so compare the two.
    #[wasm_bindgen_test]
    fn every_captured_field_is_covered_by_the_identity_table() {
        let serialised = serde_json::to_value(empty_environment()).expect("serialise");
        let fields: Vec<String> = serialised
            .as_object()
            .expect("CfEnvironment serialises as an object")
            .keys()
            .cloned()
            .collect();
        let covered: Vec<String> = mutators()
            .into_iter()
            .map(|(name, _)| name.to_string())
            .collect();

        let mut fields_sorted = fields;
        fields_sorted.sort();
        let mut covered_sorted = covered;
        covered_sorted.sort();
        assert_eq!(
            fields_sorted, covered_sorted,
            "every CfEnvironment field must appear in the identity table",
        );
    }

    /// The `CF_VERSION_METADATA` fast path stays exactly as it was: with a
    /// version id present the identity is the version plus the request-config
    /// hash, and nothing else is hashed. A version capture already covers every
    /// binding and secret, so this is the correct optimisation and the field
    /// hash is only its fallback.
    #[wasm_bindgen_test]
    fn a_worker_version_short_circuits_the_field_hash() {
        let request_config = HashMap::new();
        let mut versioned = empty_environment();
        versioned.worker_version = Some("abc123".to_string());

        let identity = versioned.identity(&request_config);
        assert!(
            identity.starts_with("worker-version:abc123:request-config:"),
            "got {identity}",
        );

        // A field that would move the fallback hash cannot move this one.
        let mut with_secret = versioned;
        with_secret.jwt_secret = Some("rotated".to_string());
        assert_eq!(with_secret.identity(&request_config), identity);
    }

    /// Request config is not a captured field, so it is folded in separately —
    /// and it must still be part of the identity on both branches.
    #[wasm_bindgen_test]
    fn request_config_moves_the_identity_on_both_branches() {
        let empty = HashMap::new();
        let one = HashMap::from([("A".to_string(), "1".to_string())]);

        let plain = empty_environment();
        assert_ne!(plain.identity(&empty), plain.identity(&one));

        let mut versioned = empty_environment();
        versioned.worker_version = Some("abc123".to_string());
        assert_ne!(versioned.identity(&empty), versioned.identity(&one));
    }

    /// `config_value` is an if-chain over key constants; a key added to either
    /// list without a matching arm would silently stop reaching the runtime.
    #[wasm_bindgen_test]
    fn environment_owned_config_keys_all_resolve() {
        let mut all_set = empty_environment();
        all_set.jwt_secret = Some("jwt".to_string());
        all_set.cors_allowed_origins = Some("cors".to_string());
        all_set.csp_directives = Some("csp".to_string());
        all_set.strict_schema = Some("1".to_string());
        all_set.request_log = Some("errors".to_string());

        for key in PROTECTED_ENV_KEYS.iter().chain(BUILDER_WORKER_VAR_KEYS) {
            assert!(
                all_set.config_value(key).is_some(),
                "{key} is declared Env-owned but reaches no CfEnvironment field",
            );
            assert!(CfEnvironment::owns_config_key(key));
        }
        assert_eq!(all_set.config_map().len(), 5);
        assert!(!CfEnvironment::owns_config_key(
            impresspress_core::features::BLOCK_SETTINGS_CONFIG_KEY
        ));
    }

    /// `IMPRESSPRESS_REQUEST_LOG` reaches the runtime's config surfaces from a
    /// `wrangler.toml` var, and a Worker var is the ONLY channel it has:
    /// `config_vars::is_infrastructure_key` makes it runtime-owned, so
    /// `blocks::config` answers it from the boot map whatever the `variables`
    /// table holds and `CONFIG_SET` refuses to write it. Dropped from
    /// `BUILDER_WORKER_VAR_KEYS`, `pipeline::write_request_log` would read
    /// nothing and every deployment would silently be back on `all`.
    #[wasm_bindgen_test]
    fn the_request_log_policy_reaches_config_from_a_worker_var() {
        let key = impresspress_core::config_vars::REQUEST_LOG_CONFIG_KEY;
        assert!(
            impresspress_core::config_vars::is_infrastructure_key(key),
            "the key must stay infrastructure-prefixed, or the variables table \
             could serve it",
        );

        let env = RecordingEnv::new(&[(key, "errors")]);
        let captured = CfEnvironment::capture(&env.env);
        assert_eq!(captured.config_value(key), Some("errors"));
        assert_eq!(
            captured.config_map().get(key).map(String::as_str),
            Some("errors"),
            "the value must reach the map both config surfaces are filled from",
        );

        let unbound = RecordingEnv::new(&[]);
        assert!(
            !CfEnvironment::capture(&unbound.env)
                .config_map()
                .contains_key(key),
            "an unbound var must leave the key absent, which `RequestLogPolicy` \
             reads as `all`",
        );
    }

    /// The main Worker does not hash, so it does not read the password pepper
    /// at all: the keys belong to the password-hasher Worker. Bound here by
    /// mistake, none is read, and none reaches a config surface — a block
    /// asking for one through the config client finds nothing, and
    /// `blocks::config` serves an infrastructure key from the boot map only,
    /// which is filled from `config_map`.
    #[wasm_bindgen_test]
    fn the_password_pepper_is_not_read_by_the_main_worker() {
        let pepper_vars = [
            PASSWORD_PEPPER_KEY_VAR,
            PASSWORD_PEPPER_PREVIOUS_KEYS_VAR,
            PASSWORD_PEPPER_REQUIRED_VAR,
        ];
        let mut bindings = every_key();
        let key = "KioqKioqKioqKioqKioqKioqKioqKioqKioqKioqKio=";
        bindings.extend(pepper_vars.iter().map(|var| (*var, key)));
        let env = RecordingEnv::new(&bindings);
        let captured = CfEnvironment::capture(&env.env);

        let map = captured.config_map();
        for var in pepper_vars {
            assert_eq!(env.reads_of(var), 0, "the main Worker read {var}");
            assert!(
                impresspress_core::config_vars::is_infrastructure_key(var),
                "{var} must stay infrastructure-prefixed, so the variables table \
                 cannot serve it either",
            );
            assert_eq!(captured.config_value(var), None, "{var}");
            assert!(!CfEnvironment::owns_config_key(var), "{var}");
            assert!(!map.contains_key(var), "{var} reached the config map");
        }
        for value in map.values() {
            assert!(!value.contains(key), "a pepper key reached the config map");
        }
    }

    /// `IMPRESSPRESS_PASSWORD_HASHER_SHARDS` is a whole number in range, and
    /// unset is the default.
    #[wasm_bindgen_test]
    fn the_shard_count_is_validated() {
        use impresspress_password::protocol::{DEFAULT_SHARDS, SHARDS_VAR};
        for bad in ["0", "65", "many", ""] {
            let env = RecordingEnv::new(&[(SHARDS_VAR, bad)]);
            let err = CfEnvironment::capture(&env.env)
                .password_hasher_shards()
                .expect_err(bad);
            assert!(err.contains(SHARDS_VAR), "{bad:?}: {err}");
        }
        let unbound = RecordingEnv::new(&[]);
        assert_eq!(
            CfEnvironment::capture(&unbound.env).password_hasher_shards(),
            Ok(DEFAULT_SHARDS)
        );
    }

    #[wasm_bindgen_test]
    fn verified_identity_cache_loads_once_and_invalidates_on_identity_change() {
        let cache: impresspress_core::IdentityCache<u32> = impresspress_core::IdentityCache::new();
        let loads = std::cell::Cell::new(0);
        let first = cache
            .get_or_try_insert_with("worker-a/plan-a".to_string(), || {
                loads.set(loads.get() + 1);
                Ok::<_, ()>(11)
            })
            .unwrap();
        let second = cache
            .get_or_try_insert_with("worker-a/plan-a".to_string(), || {
                loads.set(loads.get() + 1);
                Ok::<_, ()>(22)
            })
            .unwrap();
        assert!(std::rc::Rc::ptr_eq(&first, &second));
        assert_eq!(loads.get(), 1);

        let replacement = cache
            .get_or_try_insert_with("worker-b/plan-a".to_string(), || {
                loads.set(loads.get() + 1);
                Ok::<_, ()>(33)
            })
            .unwrap();
        assert_eq!(*replacement, 33);
        assert_eq!(loads.get(), 2);
    }

    #[wasm_bindgen_test]
    fn request_config_identity_is_order_independent_and_value_sensitive() {
        let left = HashMap::from([
            ("B".to_string(), "2".to_string()),
            ("A".to_string(), "1".to_string()),
        ]);
        let right = HashMap::from([
            ("A".to_string(), "1".to_string()),
            ("B".to_string(), "2".to_string()),
        ]);
        let changed = HashMap::from([
            ("A".to_string(), "1".to_string()),
            ("B".to_string(), "3".to_string()),
        ]);
        assert_eq!(config_identity_hash(&left), config_identity_hash(&right));
        assert_ne!(config_identity_hash(&left), config_identity_hash(&changed));
    }
}
