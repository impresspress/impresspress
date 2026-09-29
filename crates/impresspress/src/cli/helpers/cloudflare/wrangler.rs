//! wrangler.toml generation + override merging.
//!
//! [`CloudflareConfig`] is the *resolved* shape consumed by [`generate`].
//! Parsing impresspress.toml + applying env-var overlays lives in
//! [`super::env`], which produces this type.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
pub use impresspress_core::{
    config_vars::{D1_QUERIES_PER_INVOCATION_DEFAULT, D1_QUERIES_PER_INVOCATION_KEY},
    ui::assets::ASSET_BASE_URL_VAR,
    PREPARED_APPLICATION_BUILD_SHA256_VAR, PREPARED_APPLICATION_ID_VAR, PREPARED_PLAN_HASH_VAR,
    PREPARED_PLAN_MODULE_SHA256_VAR, RELEASE_ASSET_KEYS_SHA256_VAR,
    RELEASE_ASSET_MANIFEST_SHA256_VAR,
};

use super::{
    assets::{resolve_asset_base_url, ReleaseManifest},
    build::WORKER_BUILD_VERSION,
    prepared::{
        ApplicationArtifactIdentity, PreparedModule, PREPARED_MODULE_DIR, PREPARED_SHIM_FILE,
        PREPARED_TEXT_GLOB,
    },
};

/// Version-bound Worker variables exposing the immutable release asset set.
/// Existing applications may continue reading logical R2 keys during the
/// compatibility period; runtimes that understand these bindings can resolve
/// release-managed keys without consulting a mutable global pointer.
pub const RELEASE_ASSET_ID_VAR: &str = impresspress_core::prepared_plan::RELEASE_ASSET_ID_VAR;
pub const RELEASE_ASSET_PREFIX_VAR: &str =
    impresspress_core::prepared_plan::RELEASE_ASSET_PREFIX_VAR;
pub const RELEASE_ASSET_MANIFEST_VAR: &str =
    impresspress_core::prepared_plan::RELEASE_ASSET_MANIFEST_VAR;
pub const PREPARED_WAFER_LOCK_IDENTITY_VAR: &str =
    impresspress_core::PREPARED_WAFER_LOCK_IDENTITY_JSON_VAR;

/// Default `[triggers] crons` for a generated Worker config: **none**.
///
/// The scheduled sweep is opt-in, in two steps a consumer takes together:
///
/// 1. export a `scheduled` Worker entry point that calls
///    `impresspress_cloudflare::run_scheduled` (see
///    `examples/webmcp-demo/src/lib.rs`), and
/// 2. set `[cloudflare].crons` in `impresspress.toml`.
///
/// A default schedule would be a configuration-driven break of every existing
/// consumer: `run_scheduled` cannot be supplied by the adapter (it needs the
/// consumer's own registration hooks) and the CLI scaffolds no consumer source
/// file, so a Worker that changed no code of its own would take a daily failed
/// invocation the day the schedule started being applied. A sweep that does not
/// run by default is the smaller harm — every deployment already prunes
/// opportunistically on login, and the schedule only covers a deployment with
/// no logins.
///
/// [`SUGGESTED_SWEEP_CRON`] is the value to copy when turning it on.
pub const DEFAULT_CRONS: &[&str] = &[];

/// The schedule to copy into `[cloudflare].crons` when enabling the sweep.
///
/// One invocation a day, at an off-peak minute that is not `:00` — Cloudflare
/// schedules every account's `0 * * * *` at the same instant, and this work is
/// not urgent enough to join that queue. What runs on it is the auth retention
/// sweep (`auth.maintenance`), which is throttled to at most one pass an hour
/// anyway.
pub const SUGGESTED_SWEEP_CRON: &str = "17 3 * * *";

#[derive(Debug, Clone)]
pub struct CloudflareConfig {
    pub account_id: String,
    pub worker_name: String,
    pub compatibility_date: String,
    pub d1: D1Config,
    pub r2: R2Config,
    /// Path (relative to consumer repo root) to a TOML file whose contents
    /// are deep-merged over the generated defaults. None means "no overrides."
    pub wrangler_overrides_path: Option<PathBuf>,
    /// Workers Observability head sampling rate (`0.0..=1.0`), resolved by
    /// [`super::env::RawCloudflareConfig::resolve`] from
    /// `IMPRESSPRESS_CLOUDFLARE_HEAD_SAMPLING_RATE` / `impresspress.toml`'s
    /// `[cloudflare].head_sampling_rate`, defaulting to `1.0` (100%) when
    /// neither is set. Explicit and configurable rather than hardcoded, so a
    /// deployment that outgrows 100%-capture traffic doesn't have to reach
    /// for a `wrangler_overrides_path` file to dial it down.
    pub head_sampling_rate: f64,
    /// D1 queries one Worker invocation may run, written into `[vars]` as
    /// [`D1_QUERIES_PER_INVOCATION_KEY`], resolved by
    /// [`super::env::RawCloudflareConfig::resolve`] from `impresspress.toml`'s
    /// `[cloudflare].d1_queries_per_invocation`, defaulting to
    /// [`D1_QUERIES_PER_INVOCATION_DEFAULT`] (1000, which every plan allows).
    pub d1_queries_per_invocation: u64,
    /// Cloudflare cron expressions the Worker's `scheduled` handler runs on,
    /// resolved by [`super::env::RawCloudflareConfig::resolve`] from
    /// `impresspress.toml`'s `[cloudflare].crons`, defaulting to
    /// [`DEFAULT_CRONS`] (empty — the sweep is opt-in).
    ///
    /// Empty still emits `crons = []`, which is not the same as omitting the
    /// section: see [`ConfigRole`].
    pub crons: Vec<String>,
    /// Ordinary routes exercised by the bounded mixed-concurrency gate after
    /// final-version verification and before promotion. Resolution guarantees
    /// a non-empty collection of path-only values.
    pub deploy_smoke_paths: Vec<String>,
    /// The password-hasher Worker this Worker binds; see
    /// [`generate_password_hasher`].
    pub password_hasher: PasswordHasherConfig,
}

/// The resolved `[cloudflare.password_hasher]` section.
#[derive(Debug, Clone)]
pub struct PasswordHasherConfig {
    /// The hasher Worker's script name, which the main Worker's Durable Object
    /// binding names as its `script_name`.
    pub worker_name: String,
    /// Written into the main Worker's `[vars]` as
    /// `impresspress_password::protocol::SHARDS_VAR`.
    pub shards: u32,
    /// Written into the hasher Worker's `[vars]` as
    /// `IMPRESSPRESS_PASSWORD_PEPPER_REQUIRED`.
    pub pepper_required: bool,
}

/// The file [`generate_password_hasher`] writes into the output directory.
pub const PASSWORD_HASHER_CONFIG_FILE: &str = "wrangler-password-hasher.toml";

/// The hasher Worker's entry module, relative to the directory its config is
/// written to: the output of `worker-build` in the crate
/// [`super::password_hasher`] stages.
const PASSWORD_HASHER_MAIN: &str = "../impresspress-password-hasher/build/worker/shim.mjs";

/// The tag of the one Durable Object migration: the hasher's class, created
/// SQLite-backed (the only kind the Workers Free plan offers). Migrations are
/// applied in order and never edited; a later change to the class is a new
/// entry with a new tag.
pub const PASSWORD_HASHER_MIGRATION_TAG: &str = "v1";

#[derive(Debug, Clone)]
pub struct D1Config {
    pub binding: String,
    pub database_name: String,
    pub database_id: String,
}

#[derive(Debug, Clone)]
pub struct R2Config {
    pub binding: String,
    pub bucket_name: String,
    /// Optional repository-relative directory containing release-managed
    /// objects to upload to R2 before candidate initialization.
    pub release_assets_dir: Option<PathBuf>,
    /// R2 key prefix under which the contents of `release_assets_dir` land.
    /// Empty means the bucket root.
    pub release_assets_prefix: PathBuf,
    /// Compiled globs, matched against staged logical keys, whose files never
    /// enter the release asset set.
    ///
    /// The key inventory is bound into one 4 KB Worker variable, so files the
    /// runtime never reads are not merely wasted upload — they consume a hard,
    /// shared budget and eventually fail the deploy outright.
    pub release_assets_exclude: Vec<glob::Pattern>,
}

pub fn generate(cfg: &CloudflareConfig, repo_root: &Path, out_dir: &Path) -> Result<PathBuf> {
    generate_named(
        cfg,
        repo_root,
        out_dir,
        "wrangler.toml",
        ConfigRole::Build,
        VersionIdentities::default(),
    )
}

/// Generate the configuration used to upload an artifact that
/// [`super::build::run`] has already produced.
///
/// This deliberately omits Wrangler's custom `[build]` hook. `versions
/// upload` still bundles the staged `shim.mjs` and its modules, but it must
/// not compile the consumer crate a second time: doing so is both expensive
/// and breaks the guarantee that the locally inspected Wasm is the artifact
/// being uploaded.
pub fn generate_upload(
    cfg: &CloudflareConfig,
    repo_root: &Path,
    out_dir: &Path,
) -> Result<PathBuf> {
    generate_upload_with_release(cfg, repo_root, out_dir, None)
}

/// Generate an upload-only config and bind an immutable release asset identity
/// into the resulting Worker version.
pub fn generate_upload_with_release(
    cfg: &CloudflareConfig,
    repo_root: &Path,
    out_dir: &Path,
    release: Option<&ReleaseManifest>,
) -> Result<PathBuf> {
    generate_named(
        cfg,
        repo_root,
        out_dir,
        "wrangler-upload.toml",
        ConfigRole::Upload,
        VersionIdentities {
            release,
            artifact: None,
            prepared: None,
        },
    )
}

/// Upload-only dynamic candidate. It carries the same release identity as the
/// final version plus the Wasm/wafer.lock identity consumed by
/// `/_deploy/prepare`, but no prepared Text module.
pub fn generate_candidate_upload(
    cfg: &CloudflareConfig,
    repo_root: &Path,
    out_dir: &Path,
    release: &ReleaseManifest,
    identity: &ApplicationArtifactIdentity,
) -> Result<PathBuf> {
    generate_named(
        cfg,
        repo_root,
        out_dir,
        "wrangler-candidate.toml",
        ConfigRole::Upload,
        VersionIdentities {
            release: Some(release),
            artifact: Some(identity),
            prepared: None,
        },
    )
}

/// Upload-only final version. Uses the exact same Wasm artifact as the dynamic
/// candidate and changes only the entry shim, Text plan module, and final plan
/// identity vars.
pub fn generate_final_upload(
    cfg: &CloudflareConfig,
    repo_root: &Path,
    out_dir: &Path,
    release: &ReleaseManifest,
    identity: &ApplicationArtifactIdentity,
    prepared: &PreparedModule,
) -> Result<PathBuf> {
    generate_named(
        cfg,
        repo_root,
        out_dir,
        "wrangler-final.toml",
        ConfigRole::Upload,
        VersionIdentities {
            release: Some(release),
            artifact: Some(identity),
            prepared: Some(prepared),
        },
    )
}

/// Generate the configuration handed to `wrangler triggers deploy` after the
/// final version has been promoted.
///
/// Carries no release identity, artifact identity, or prepared plan: those are
/// *versioned* settings, already bound into the promoted version by
/// [`generate_final_upload`], and `triggers deploy` uploads no code. What it
/// does carry is the worker-level surface — the worker name, any routes or
/// custom domains from a consumer override file, `preview_urls`, and the
/// `[triggers]` section that is the whole reason this config exists.
pub fn generate_triggers(
    cfg: &CloudflareConfig,
    repo_root: &Path,
    out_dir: &Path,
) -> Result<PathBuf> {
    generate_named(
        cfg,
        repo_root,
        out_dir,
        "wrangler-triggers.toml",
        ConfigRole::WorkerSettings,
        VersionIdentities::default(),
    )
}

/// Which wrangler command a generated config is written for. Three commands,
/// three roles, and the role decides both of the things that differ between
/// them: whether wrangler compiles the crate, and whether the file carries the
/// `[triggers]` section.
///
/// The triggers half is the load-bearing one. Cron triggers are a
/// **worker-level** (unversioned) setting. `wrangler versions upload` accepts
/// the key without complaint — it is a first-class configuration field, not an
/// unknown one — and simply does not apply it; the command's own closing note
/// says so verbatim: *"Changes to triggers (routes, custom domains, cron
/// schedules, etc) must be applied with the command `wrangler triggers
/// deploy`"* (wrangler 4.72.0). So an upload config that carried `[triggers]`
/// would be a lie about what the upload does, which is exactly the shape of
/// bug the section exists to prevent: an operator editing the schedule, seeing
/// it in the generated file, and watching the live Worker never change.
///
/// The two roles that do emit it emit it **always**, an empty schedule list
/// included. That is not cosmetic: `wrangler triggers deploy` PUTs the schedule
/// set only when the config defines `triggers.crons` at all, so an absent
/// section leaves whatever schedules the Worker already has in place, while
/// `crons = []` clears them. A deployment that turns the sweep off has to be
/// able to actually turn it off.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ConfigRole {
    /// `wrangler dev`. Compiles the crate; applies worker-level settings.
    Build,
    /// `wrangler versions upload`. Consumes an already-built artifact and
    /// applies no worker-level setting.
    Upload,
    /// `wrangler triggers deploy`, run after promotion. Uploads no code and
    /// applies nothing but worker-level settings.
    WorkerSettings,
}

impl ConfigRole {
    fn runs_the_build_hook(self) -> bool {
        self == Self::Build
    }

    fn applies_worker_level_settings(self) -> bool {
        self != Self::Upload
    }
}

/// The identities a generated config binds into the Worker *version* it
/// uploads, as opposed to the worker-level settings [`ConfigRole`] decides.
///
/// Each is independently optional because the deploy pipeline uploads three
/// different versions and each one knows more than the last: the plain upload
/// carries a release asset set only, the dynamic candidate adds the Wasm +
/// wafer.lock identity `/_deploy/prepare` consumes, and the final version adds
/// the prepared Text plan module. Grouping them keeps that progression in one
/// place instead of three positional `None`s at every call site.
#[derive(Clone, Copy, Default)]
struct VersionIdentities<'a> {
    /// Immutable release asset set — [`RELEASE_ASSET_ID_VAR`] and friends.
    release: Option<&'a ReleaseManifest>,
    /// Built artifact identity — [`PREPARED_APPLICATION_ID_VAR`] and friends.
    artifact: Option<&'a ApplicationArtifactIdentity>,
    /// Prepared runtime plan. Also repoints `main` at the prepared shim and
    /// installs the Text module rule the plan is served through.
    prepared: Option<&'a PreparedModule>,
}

fn generate_named(
    cfg: &CloudflareConfig,
    repo_root: &Path,
    out_dir: &Path,
    file_name: &str,
    role: ConfigRole,
    identities: VersionIdentities<'_>,
) -> Result<PathBuf> {
    let VersionIdentities {
        release,
        artifact,
        prepared,
    } = identities;
    let mut value = base_toml(cfg, role);

    if let Some(rel) = cfg.wrangler_overrides_path.as_ref() {
        let abs = repo_root.join(rel);
        let raw = std::fs::read_to_string(&abs)
            .with_context(|| format!("read overrides {}", abs.display()))?;
        let overrides: toml::Value =
            toml::from_str(&raw).with_context(|| format!("parse overrides {}", abs.display()))?;
        deep_merge(&mut value, overrides);
    }

    refuse_secrets_in_vars(&value, cfg)?;

    // The crypto service reaches the hasher through this exact binding name,
    // so a consumer override that replaced `[durable_objects]` (to bind its
    // own classes, say) must not drop it: restore it, like the version
    // metadata binding below.
    install_password_hasher_binding(&mut value, &cfg.password_hasher)?;

    // Runtime code relies on this exact internal binding name. Restore it
    // after consumer overrides just like the upload-only build invariant
    // below, so a broad override cannot silently disable cache freshness.
    let mut version_metadata = toml::map::Map::new();
    version_metadata.insert(
        "binding".into(),
        toml::Value::String("CF_VERSION_METADATA".into()),
    );
    value
        .as_table_mut()
        .expect("base wrangler config is a table")
        .insert(
            "version_metadata".into(),
            toml::Value::Table(version_metadata),
        );

    if let Some(release) = release {
        let root = value
            .as_table_mut()
            .expect("base wrangler config is a table");
        let vars = root
            .get_mut("vars")
            .and_then(toml::Value::as_table_mut)
            .context("wrangler overrides replaced [vars] with a non-table")?;
        // These are deployment invariants, so install them after consumer
        // overrides. They are captured with the Worker version and therefore
        // restore the correct immutable asset identity on version rollback.
        vars.insert(
            RELEASE_ASSET_ID_VAR.into(),
            toml::Value::String(release.asset_set_sha256.clone()),
        );
        vars.insert(
            RELEASE_ASSET_PREFIX_VAR.into(),
            toml::Value::String(release.immutable_prefix.clone()),
        );
        vars.insert(
            RELEASE_ASSET_MANIFEST_VAR.into(),
            toml::Value::String(release.manifest_key()),
        );
        vars.insert(
            RELEASE_ASSET_MANIFEST_SHA256_VAR.into(),
            toml::Value::String(release.manifest_sha256()?),
        );
        vars.insert(
            RELEASE_ASSET_KEYS_SHA256_VAR.into(),
            toml::Value::String(release.logical_keys_sha256()?),
        );
    }

    if let Some(identity) = artifact {
        let vars = value
            .as_table_mut()
            .expect("base wrangler config is a table")
            .get_mut("vars")
            .and_then(toml::Value::as_table_mut)
            .context("wrangler overrides replaced [vars] with a non-table")?;
        vars.insert(
            PREPARED_APPLICATION_ID_VAR.into(),
            toml::Value::String(identity.application_id.clone()),
        );
        vars.insert(
            PREPARED_APPLICATION_BUILD_SHA256_VAR.into(),
            toml::Value::String(identity.application_build_sha256.clone()),
        );
        vars.insert(
            PREPARED_WAFER_LOCK_IDENTITY_VAR.into(),
            toml::Value::String(identity.dependency_lock_json()?),
        );
    }

    if let Some(prepared) = prepared {
        let root = value
            .as_table_mut()
            .expect("base wrangler config is a table");
        root.insert(
            "main".into(),
            toml::Value::String(format!("{PREPARED_MODULE_DIR}/{PREPARED_SHIM_FILE}")),
        );
        let vars = root
            .get_mut("vars")
            .and_then(toml::Value::as_table_mut)
            .context("wrangler overrides replaced [vars] with a non-table")?;
        vars.insert(
            PREPARED_PLAN_HASH_VAR.into(),
            toml::Value::String(prepared.plan_hash.clone()),
        );
        vars.insert(
            PREPARED_PLAN_MODULE_SHA256_VAR.into(),
            toml::Value::String(prepared.plan_module_sha256.clone()),
        );
        install_prepared_text_rule(root)?;
    }

    // Remove the hook *after* applying consumer overrides. An upload-only
    // config is an invariant of the deploy pipeline, not something an
    // override file may accidentally undo.
    if !role.runs_the_build_hook() {
        value
            .as_table_mut()
            .expect("base wrangler config is a table")
            .remove("build");
    }

    let body = toml::to_string_pretty(&value).context("serialize wrangler.toml")?;
    let header = match role {
        ConfigRole::Build => {
            "# Generated by `impresspress build --target cloudflare`. \
             Do not edit. Regenerated each build.\n\n"
        }
        ConfigRole::Upload => {
            "# Generated by `impresspress deploy --target cloudflare`. \
             Upload-only: consumes the already-built worker artifact. \
             Do not edit.\n\n"
        }
        ConfigRole::WorkerSettings => {
            "# Generated by `impresspress deploy --target cloudflare`. \
             Worker-level settings only, applied by `wrangler triggers deploy` \
             after promotion; uploads no code. Do not edit.\n\n"
        }
    };
    let path = out_dir.join(file_name);
    std::fs::write(
        &path,
        format!(
            "{header}{D1_QUERIES_NOTE}{}{body}",
            password_pepper_note(&cfg.password_hasher.worker_name)
        ),
    )
    .with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}

/// Generate the password-hasher Worker's config, deployed with plain
/// `wrangler deploy` BEFORE the main Worker's upload.
///
/// The hasher exports the Durable Object class
/// ([`impresspress_password::protocol::DURABLE_OBJECT_CLASS`]) the main
/// Worker's crypto service sends every password hash and verification to. It
/// is a Worker of its own because Cloudflare generates no version preview URLs
/// for a Worker that implements a Durable Object, and because a version that
/// changes a Durable Object class's lifecycle — the migration below — cannot be
/// uploaded with `wrangler versions upload`, only deployed. Neither restriction
/// touches the main Worker, which only binds the class.
///
/// No consumer override file applies here: the hasher has no application
/// settings. Its secrets (the password pepper) are set on it directly with
/// `wrangler secret put … --name <hasher>` and survive every deploy; its one
/// var comes from `[cloudflare.password_hasher].pepper_required`, because a
/// deploy replaces the vars the dashboard holds.
pub fn generate_password_hasher(cfg: &CloudflareConfig, out_dir: &Path) -> Result<PathBuf> {
    use impresspress_password::{pepper::PASSWORD_PEPPER_REQUIRED_VAR, protocol};
    use toml::Value;

    let hasher = &cfg.password_hasher;
    let mut root = toml::map::Map::new();
    root.insert("name".into(), Value::String(hasher.worker_name.clone()));
    root.insert("account_id".into(), Value::String(cfg.account_id.clone()));
    root.insert("main".into(), Value::String(PASSWORD_HASHER_MAIN.into()));
    root.insert(
        "compatibility_date".into(),
        Value::String(cfg.compatibility_date.clone()),
    );
    // Reached through the main Worker's Durable Object binding only: no
    // public URL, and no preview URLs (a Durable Object's Worker gets none).
    root.insert("workers_dev".into(), Value::Boolean(false));
    root.insert("preview_urls".into(), Value::Boolean(false));

    let mut migration = toml::map::Map::new();
    migration.insert(
        "tag".into(),
        Value::String(PASSWORD_HASHER_MIGRATION_TAG.into()),
    );
    migration.insert(
        "new_sqlite_classes".into(),
        Value::Array(vec![Value::String(protocol::DURABLE_OBJECT_CLASS.into())]),
    );
    root.insert(
        "migrations".into(),
        Value::Array(vec![Value::Table(migration)]),
    );

    let mut vars = toml::map::Map::new();
    vars.insert(
        PASSWORD_PEPPER_REQUIRED_VAR.into(),
        Value::String(hasher.pepper_required.to_string()),
    );
    root.insert("vars".into(), Value::Table(vars));

    let mut obs = toml::map::Map::new();
    obs.insert("enabled".into(), Value::Boolean(true));
    obs.insert(
        "head_sampling_rate".into(),
        Value::Float(cfg.head_sampling_rate),
    );
    root.insert("observability".into(), Value::Table(obs));

    let body = toml::to_string_pretty(&Value::Table(root)).context("serialize wrangler.toml")?;
    let path = out_dir.join(PASSWORD_HASHER_CONFIG_FILE);
    std::fs::write(
        &path,
        format!(
            "# Generated by `impresspress build --target cloudflare`: the password-hasher \
             Worker, deployed with `wrangler deploy` before the main Worker. Do not edit.\n\n{}{body}",
            password_pepper_note(&hasher.worker_name)
        ),
    )
    .with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}

/// Put the main Worker's binding to the hasher's Durable Object class into
/// `[[durable_objects.bindings]]`, replacing any entry of the same name and
/// keeping every other.
fn install_password_hasher_binding(
    value: &mut toml::Value,
    hasher: &PasswordHasherConfig,
) -> Result<()> {
    use impresspress_password::protocol;
    let root = value
        .as_table_mut()
        .expect("base wrangler config is a table");
    let durable_objects = root
        .entry("durable_objects")
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
        .as_table_mut()
        .context("wrangler overrides replaced [durable_objects] with a non-table")?;
    let bindings = durable_objects
        .entry("bindings")
        .or_insert_with(|| toml::Value::Array(Vec::new()))
        .as_array_mut()
        .context("wrangler overrides replaced durable_objects.bindings with a non-array")?;
    bindings.retain(|binding| {
        binding.get("name").and_then(toml::Value::as_str) != Some(protocol::BINDING)
    });
    let mut binding = toml::map::Map::new();
    binding.insert("name".into(), toml::Value::String(protocol::BINDING.into()));
    binding.insert(
        "class_name".into(),
        toml::Value::String(protocol::DURABLE_OBJECT_CLASS.into()),
    );
    binding.insert(
        "script_name".into(),
        toml::Value::String(hasher.worker_name.clone()),
    );
    bindings.push(toml::Value::Table(binding));
    Ok(())
}

/// What the generated `[vars]` value of [`D1_QUERIES_PER_INVOCATION_KEY`]
/// means and where it is set, for someone reading the file: the value alone
/// does not say when to change it.
const D1_QUERIES_NOTE: &str =
    "# IMPRESSPRESS_D1_QUERIES_PER_INVOCATION is how many D1 queries one \
invocation may run: 1000 on Workers Free and Paid alike. Lower it only for a Worker whose \
limits.subrequests is lower, with [cloudflare].d1_queries_per_invocation in impresspress.toml.\n\n";

/// Where the password pepper goes, for someone reading either Worker's file:
/// it is in neither, and it belongs to the password-hasher Worker.
fn password_pepper_note(hasher_worker_name: &str) -> String {
    format!(
        "# Password pepper (optional): it belongs to the password-hasher Worker, which does \
the hashing. Generate a key with `openssl rand -base64 32` and set it with `wrangler secret \
put IMPRESSPRESS_PASSWORD_PEPPER_KEY --name {hasher_worker_name}`, never in [vars]. Back it \
up outside Cloudflare: losing it locks out every account whose hash it peppered. \
IMPRESSPRESS_PASSWORD_PEPPER_PREVIOUS_KEYS (a secret too) holds rotated-out keys; \
IMPRESSPRESS_PASSWORD_PEPPER_REQUIRED comes from [cloudflare.password_hasher].pepper_required \
in impresspress.toml.\n\n"
    )
}

/// Refuse a config whose `[vars]`, or any `[env.<name>.vars]`, holds a
/// password pepper setting or a bootstrap-admin credential.
///
/// A value there is plain text — in this file, in the override file it came
/// from (which a consumer repo commits) and in the Cloudflare dashboard.
///
/// - The pepper settings: the main Worker does not read them, because the
///   pepper belongs to the password-hasher Worker. A key put here would
///   pepper nothing, and a `REQUIRED` here would require nothing.
/// - The bootstrap-admin email and password: a consumer that forwards them
///   to the auth block (the webmcp demo reads them with `Env::secret`, which
///   returns a `[vars]` entry of the same name as well) creates the first
///   admin from them, so the pair is an admin login and goes in as Worker
///   secrets.
///
/// The error names the var and where it goes, never the value.
fn refuse_secrets_in_vars(value: &toml::Value, cfg: &CloudflareConfig) -> Result<()> {
    use impresspress_core::blocks::auth::config::{
        BOOTSTRAP_ADMIN_EMAIL_KEY, BOOTSTRAP_ADMIN_PASSWORD_KEY,
    };
    use impresspress_password::pepper::{
        PASSWORD_PEPPER_KEY_VAR, PASSWORD_PEPPER_PREVIOUS_KEYS_VAR, PASSWORD_PEPPER_REQUIRED_VAR,
    };
    let hasher_worker_name = &cfg.password_hasher.worker_name;
    // The top-level `[vars]`, and each `[env.<name>.vars]`: wrangler does not
    // inherit `vars` into an environment, so an environment's own table is
    // where its deploy reads them from.
    let mut tables: Vec<(String, &toml::Value)> = Vec::new();
    if let Some(vars) = value.get("vars") {
        tables.push(("[vars]".to_string(), vars));
    }
    if let Some(envs) = value.get("env").and_then(toml::Value::as_table) {
        for (name, env) in envs {
            if let Some(vars) = env.get("vars") {
                tables.push((format!("[env.{name}.vars]"), vars));
            }
        }
    }
    for (table, vars) in tables {
        let Some(vars) = vars.as_table() else {
            continue;
        };
        for var in [PASSWORD_PEPPER_KEY_VAR, PASSWORD_PEPPER_PREVIOUS_KEYS_VAR] {
            if vars.contains_key(var) {
                anyhow::bail!(
                    "{var} is set under {table} in the wrangler overrides, where it is plain \
                     text (in the file and in the Cloudflare dashboard), and where the main \
                     Worker does not read it: the password-hasher Worker does the hashing. \
                     Remove it and set it with `wrangler secret put {var} --name \
                     {hasher_worker_name}`."
                );
            }
        }
        if vars.contains_key(PASSWORD_PEPPER_REQUIRED_VAR) {
            anyhow::bail!(
                "{PASSWORD_PEPPER_REQUIRED_VAR} is set under {table} in the wrangler overrides, \
                 where the main Worker does not read it: the password-hasher Worker does the \
                 hashing. Remove it and set [cloudflare.password_hasher].pepper_required in \
                 impresspress.toml."
            );
        }
        for var in [BOOTSTRAP_ADMIN_EMAIL_KEY, BOOTSTRAP_ADMIN_PASSWORD_KEY] {
            if vars.contains_key(var) {
                anyhow::bail!(
                    "{var} is set under {table} in the wrangler overrides, where it is plain \
                     text (in the file and in the Cloudflare dashboard), and together with its \
                     pair it is a login to the first admin account. Remove it and set it with \
                     `wrangler secret put {var} --name {worker_name}`.",
                    worker_name = cfg.worker_name,
                );
            }
        }
    }
    Ok(())
}

fn install_prepared_text_rule(root: &mut toml::map::Map<String, toml::Value>) -> Result<()> {
    let rules = root
        .entry("rules")
        .or_insert_with(|| toml::Value::Array(Vec::new()))
        .as_array_mut()
        .context("wrangler overrides replaced rules with a non-array")?;
    let already_present = rules.iter().any(|rule| {
        rule.as_table()
            .and_then(|table| table.get("globs"))
            .and_then(toml::Value::as_array)
            .is_some_and(|globs| {
                globs
                    .iter()
                    .any(|glob| glob.as_str() == Some(PREPARED_TEXT_GLOB))
            })
    });
    if !already_present {
        let mut text = toml::map::Map::new();
        text.insert("type".into(), toml::Value::String("Text".into()));
        text.insert(
            "globs".into(),
            toml::Value::Array(vec![toml::Value::String(PREPARED_TEXT_GLOB.into())]),
        );
        text.insert("fallthrough".into(), toml::Value::Boolean(true));
        rules.push(toml::Value::Table(text));
    }
    Ok(())
}

fn base_toml(cfg: &CloudflareConfig, role: ConfigRole) -> toml::Value {
    use toml::Value;

    let mut root = toml::map::Map::new();
    root.insert("name".into(), Value::String(cfg.worker_name.clone()));
    root.insert("account_id".into(), Value::String(cfg.account_id.clone()));
    root.insert(
        "main".into(),
        Value::String("../../build/worker/shim.mjs".into()),
    );
    root.insert(
        "compatibility_date".into(),
        Value::String(cfg.compatibility_date.clone()),
    );
    // Required so `wrangler versions upload` prints a "Version Preview
    // URL" line — `impresspress deploy` parses that URL to call the new
    // version preview URLs needed for `/_deploy/prepare`, final verification,
    // and health checks before promotion.
    root.insert("preview_urls".into(), Value::Boolean(true));

    // Gives the runtime a cheap, request-current identity for the complete
    // Worker version. Cloudflare may reuse an isolate when only bindings or
    // secrets change; comparing this ID before returning an isolate-cached
    // Wafer prevents services constructed from the previous request's Env
    // from surviving that change.
    let mut version_metadata = toml::map::Map::new();
    version_metadata.insert(
        "binding".into(),
        Value::String("CF_VERSION_METADATA".into()),
    );
    root.insert("version_metadata".into(), Value::Table(version_metadata));

    let mut build = toml::map::Map::new();
    // Pinned to an exact version (`WORKER_BUILD_VERSION`, shared with the
    // local `impresspress build --target cloudflare` path in
    // `build::ensure_worker_build_installed`) — worker-build 0.8.x rejects
    // consumers using `worker < 0.8` (hard version check) and changed its
    // output layout from `build/worker/shim.mjs` (which `main` points to)
    // to `build/index.js`. Until we upgrade the `worker` crate, lock the
    // toolchain to this exact version for both the local build and the
    // wrangler-driven rebuild during deploy.
    //
    // The `command -v ... && [ ... ] ||` guard skips `cargo install`
    // outright when the pinned version is already on `PATH` (e.g. a build
    // environment that persists `~/.cargo/bin` across runs), instead of
    // reinstalling unconditionally on every build.
    build.insert(
        "command".into(),
        Value::String(format!(
            "((command -v worker-build >/dev/null 2>&1 && \
             [ \"$(worker-build --version)\" = \"{WORKER_BUILD_VERSION}\" ]) || \
             cargo install worker-build --version \"={WORKER_BUILD_VERSION}\" --quiet) && \
             worker-build --no-default-features \
             --features target-cloudflare"
        )),
    );
    root.insert("build".into(), Value::Table(build));

    let mut d1_entry = toml::map::Map::new();
    d1_entry.insert("binding".into(), Value::String(cfg.d1.binding.clone()));
    d1_entry.insert(
        "database_name".into(),
        Value::String(cfg.d1.database_name.clone()),
    );
    d1_entry.insert(
        "database_id".into(),
        Value::String(cfg.d1.database_id.clone()),
    );
    root.insert(
        "d1_databases".into(),
        Value::Array(vec![Value::Table(d1_entry)]),
    );

    let mut r2_entry = toml::map::Map::new();
    r2_entry.insert("binding".into(), Value::String(cfg.r2.binding.clone()));
    r2_entry.insert(
        "bucket_name".into(),
        Value::String(cfg.r2.bucket_name.clone()),
    );
    root.insert(
        "r2_buckets".into(),
        Value::Array(vec![Value::Table(r2_entry)]),
    );

    // Plain worker vars (`env.var(...)`), read at runtime by the CF adapter.
    // STRICT_SCHEMA on: the D1 database service trusts its migrated schema and
    // skips the per-op table-exists probe + lazy ADD COLUMN, removing a network
    // round-trip per logical op on D1. This is a deploy-time operational
    // decision (not an admin-editable runtime toggle), so it lives here rather
    // than in the D1 `variables` table. The adapter's `build_runtime` threads
    // this into the config snapshot so the `wafer-run/database` block applies
    // it at Init. A consumer that needs the self-healing lazy-schema behavior
    // can flip it back off via `wrangler_overrides_path`.
    let mut vars = toml::map::Map::new();
    vars.insert(
        wafer_core::interfaces::database::handler::STRICT_SCHEMA_CONFIG_KEY.into(),
        Value::String("true".into()),
    );
    // Every generated config represents code that can actually run
    // (`wrangler dev`, the upload-only candidate, or the final promoted
    // version), so this belongs at the base level rather than only on the
    // release-bound variants: a lean (no `embed-assets`) Cloudflare build
    // has nowhere else to learn where its static assets live.
    vars.insert(
        ASSET_BASE_URL_VAR.into(),
        Value::String(resolve_asset_base_url(!cfg.r2.bucket_name.is_empty())),
    );
    // D1's per-invocation query limit, which the runtime's statement budget
    // admits every multi-statement write against. Always written, the
    // default included, so the generated config states the limit the Worker
    // will run under instead of leaving it to a runtime default.
    vars.insert(
        D1_QUERIES_PER_INVOCATION_KEY.into(),
        Value::String(cfg.d1_queries_per_invocation.to_string()),
    );
    // How many password-hasher Durable Object instances hashing is spread
    // across. Always written, the default included, like the D1 limit above.
    vars.insert(
        impresspress_password::protocol::SHARDS_VAR.into(),
        Value::String(cfg.password_hasher.shards.to_string()),
    );
    root.insert("vars".into(), Value::Table(vars));

    // Workers Logs (a.k.a. Workers Observability). Off by default at the
    // platform; we turn it on so dashboard logs + the `wrangler tail`
    // request envelope are populated. `head_sampling_rate` is an explicit,
    // configurable knob (`cfg.head_sampling_rate`, see
    // `CloudflareConfig::head_sampling_rate`) rather than hardcoded — set
    // `[cloudflare].head_sampling_rate` in impresspress.toml or
    // `IMPRESSPRESS_CLOUDFLARE_HEAD_SAMPLING_RATE` for a deployment whose
    // traffic has outgrown 100% capture.
    let mut obs = toml::map::Map::new();
    obs.insert("enabled".into(), Value::Boolean(true));
    obs.insert(
        "head_sampling_rate".into(),
        Value::Float(cfg.head_sampling_rate),
    );
    root.insert("observability".into(), Value::Table(obs));

    // Cron triggers. Cloudflare invokes the Worker's `scheduled` handler on
    // each of these; the adapter dispatches the auth retention sweep there and
    // nothing else (`impresspress_cloudflare::run_scheduled`).
    //
    // Written whenever this config is one a worker-level command reads (see
    // [`ConfigRole`]), including when the list is empty — `crons = []` and an
    // absent `[triggers]` do NOT mean the same thing to `wrangler triggers
    // deploy`: it PUTs the schedule set only when the config defines
    // `triggers.crons` at all, so the absent form would leave a schedule
    // registered by an earlier deploy running forever.
    if role.applies_worker_level_settings() {
        let mut section = toml::map::Map::new();
        section.insert(
            "crons".into(),
            Value::Array(
                cfg.crons
                    .iter()
                    .map(|cron| Value::String(cron.clone()))
                    .collect(),
            ),
        );
        root.insert("triggers".into(), Value::Table(section));
    }

    Value::Table(root)
}

/// Deep-merge `overrides` into `base`. Tables merge recursively;
/// arrays and primitives are replaced wholesale.
fn deep_merge(base: &mut toml::Value, overrides: toml::Value) {
    use toml::Value;
    match (base, overrides) {
        (Value::Table(b), Value::Table(o)) => {
            for (k, v) in o {
                match b.get_mut(&k) {
                    Some(existing) => deep_merge(existing, v),
                    None => {
                        b.insert(k, v);
                    }
                }
            }
        }
        (slot, replacement) => *slot = replacement,
    }
}
