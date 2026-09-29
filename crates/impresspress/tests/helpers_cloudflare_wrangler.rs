use std::fs;

use impresspress::cli::helpers::cloudflare::{
    assets::release_manifest_from_staged_dir,
    build::WORKER_BUILD_VERSION,
    prepared::{stage_prepared_module, ApplicationArtifactIdentity, PREPARED_TEXT_GLOB},
    wrangler::{
        generate, generate_candidate_upload, generate_final_upload, generate_password_hasher,
        generate_triggers, generate_upload, generate_upload_with_release, CloudflareConfig,
        D1Config, PasswordHasherConfig, R2Config, ASSET_BASE_URL_VAR,
        D1_QUERIES_PER_INVOCATION_DEFAULT, D1_QUERIES_PER_INVOCATION_KEY, DEFAULT_CRONS,
        PASSWORD_HASHER_CONFIG_FILE, PREPARED_APPLICATION_BUILD_SHA256_VAR,
        PREPARED_APPLICATION_ID_VAR, PREPARED_PLAN_HASH_VAR, PREPARED_PLAN_MODULE_SHA256_VAR,
        PREPARED_WAFER_LOCK_IDENTITY_VAR, RELEASE_ASSET_ID_VAR, RELEASE_ASSET_KEYS_SHA256_VAR,
        RELEASE_ASSET_MANIFEST_SHA256_VAR, RELEASE_ASSET_MANIFEST_VAR, RELEASE_ASSET_PREFIX_VAR,
        SUGGESTED_SWEEP_CRON,
    },
};
use impresspress_core::{PreparedRuntimePlan, PreparedRuntimeStructure, WaferLockIdentity};
use tempfile::tempdir;

fn sample_cfg() -> CloudflareConfig {
    CloudflareConfig {
        account_id: "test-acct".into(),
        worker_name: "wafer-site".into(),
        compatibility_date: "2026-05-01".into(),
        d1: D1Config {
            binding: "DB".into(),
            database_name: "wafer-site-prod".into(),
            database_id: "00000000-0000-0000-0000-000000000000".into(),
        },
        r2: R2Config {
            binding: "STORAGE".into(),
            bucket_name: "wafer-site-assets".into(),
            release_assets_dir: None,
            release_assets_prefix: Default::default(),
            release_assets_exclude: Vec::new(),
        },
        wrangler_overrides_path: None,
        head_sampling_rate: 1.0,
        d1_queries_per_invocation: D1_QUERIES_PER_INVOCATION_DEFAULT,
        crons: DEFAULT_CRONS.iter().map(|s| s.to_string()).collect(),
        deploy_smoke_paths: vec!["/health".into()],
        password_hasher: PasswordHasherConfig {
            worker_name: "wafer-site-password-hasher".into(),
            shards: 8,
            pepper_required: false,
        },
    }
}

fn sample_identity() -> ApplicationArtifactIdentity {
    ApplicationArtifactIdentity {
        application_id: "wafer-site".into(),
        application_build_sha256: format!("sha256:{}", "a".repeat(64)),
        dependency_lock: WaferLockIdentity::absent(),
    }
}

fn sample_plan(
    release: &impresspress::cli::helpers::cloudflare::assets::ReleaseManifest,
    identity: &ApplicationArtifactIdentity,
) -> PreparedRuntimePlan {
    PreparedRuntimePlan::new_with_config_generation(
        "wafer-site",
        identity.application_build_sha256.clone(),
        "3".repeat(32),
        identity.dependency_lock.clone(),
        release.prepared_identity().unwrap(),
        PreparedRuntimeStructure {
            application_blocks: Vec::new(),
            routes: Vec::new(),
            built_in_route_count: 0,
            block_settings: Default::default(),
            block_configs: Default::default(),
            final_block_configs: Default::default(),
            wrap_grants: Vec::new(),
            deployment_wrap_grants: Vec::new(),
        },
    )
    .unwrap()
}

#[test]
fn generate_writes_wrangler_toml_with_required_fields() {
    let tmp = tempdir().unwrap();
    let repo_root = tmp.path();
    let out = repo_root.join("target/impresspress-cloudflare");
    fs::create_dir_all(&out).unwrap();

    let path = generate(&sample_cfg(), repo_root, &out).unwrap();
    assert!(path.exists(), "wrangler.toml should be created");
    let body = fs::read_to_string(&path).unwrap();

    assert!(body.contains(r#"name = "wafer-site""#));
    assert!(body.contains(r#"account_id = "test-acct""#));
    assert!(body.contains(r#"compatibility_date = "2026-05-01""#));
    assert!(body.contains(r#"binding = "DB""#));
    assert!(body.contains(r#"database_name = "wafer-site-prod""#));
    assert!(body.contains(r#"binding = "STORAGE""#));
    assert!(body.contains(r#"bucket_name = "wafer-site-assets""#));
    assert!(body.contains(r#"main = "../../build/worker/shim.mjs""#));
    assert!(
        body.contains(r#"binding = "CF_VERSION_METADATA""#),
        "runtime cache invalidation needs the Worker version identity:\n{body}"
    );
    assert!(
        body.contains("preview_urls = true"),
        "preview_urls must be enabled so `wrangler versions upload` prints a \
         Version Preview URL for `impresspress deploy` to parse:\n{body}"
    );
    assert!(
        !body.contains("migrations_dir"),
        "deploy toml must not declare a wrangler migrations ledger — \
         schema funnels through /_deploy/init"
    );
    assert!(
        body.contains(r#"WAFER_RUN__DATABASE__STRICT_SCHEMA = "true""#),
        "deploy toml must enable STRICT_SCHEMA so the D1 adapter trusts its \
         migrated schema and skips per-op introspection round-trips:\n{body}"
    );
}

#[test]
fn generate_points_asset_base_url_at_own_origin_when_r2_is_configured() {
    let tmp = tempdir().unwrap();
    let repo_root = tmp.path();
    let out = repo_root.join("target/impresspress-cloudflare");
    fs::create_dir_all(&out).unwrap();

    // sample_cfg() always sets a non-empty r2.bucket_name.
    let path = generate(&sample_cfg(), repo_root, &out).unwrap();
    let body = fs::read_to_string(&path).unwrap();

    assert!(
        body.contains(&format!(r#"{ASSET_BASE_URL_VAR} = "/b/static/""#)),
        "expected same-origin asset base URL when R2 is configured:\n{body}"
    );
}

#[test]
fn generate_falls_back_asset_base_url_to_cdn_without_r2() {
    let tmp = tempdir().unwrap();
    let repo_root = tmp.path();
    let out = repo_root.join("target/impresspress-cloudflare");
    fs::create_dir_all(&out).unwrap();

    let mut cfg = sample_cfg();
    cfg.r2.bucket_name = String::new();
    let path = generate(&cfg, repo_root, &out).unwrap();
    let body = fs::read_to_string(&path).unwrap();

    assert!(
        body.contains(&format!(
            r#"{ASSET_BASE_URL_VAR} = "https://cdn.impresspress.org/ui/v"#
        )),
        "expected the versioned CDN fallback without R2:\n{body}"
    );
}

#[test]
fn generate_writes_configured_head_sampling_rate() {
    let tmp = tempdir().unwrap();
    let repo_root = tmp.path();
    let out = repo_root.join("target/impresspress-cloudflare");
    fs::create_dir_all(&out).unwrap();

    let mut cfg = sample_cfg();
    cfg.head_sampling_rate = 0.1;
    let path = generate(&cfg, repo_root, &out).unwrap();
    let body = fs::read_to_string(&path).unwrap();

    assert!(
        body.contains("head_sampling_rate = 0.1"),
        "generated toml should reflect the configured sampling rate, not a \
         hardcoded 1.0:\n{body}"
    );
}

/// Golden for the D1 query limit a generated config states. The runtime's
/// statement budget admits every multi-statement write against this var, so
/// a Worker that ran on a limit above its platform's would be admitted writes
/// D1 then refuses part-way. Every config that uploads code must carry it —
/// the default included — along with the note that says when to change it.
#[test]
fn every_generated_config_states_the_d1_query_limit_and_what_it_depends_on() {
    let tmp = tempdir().unwrap();
    let repo_root = tmp.path();
    let out = repo_root.join("target/impresspress-cloudflare");
    fs::create_dir_all(&out).unwrap();

    assert_eq!(
        D1_QUERIES_PER_INVOCATION_KEY,
        "IMPRESSPRESS_D1_QUERIES_PER_INVOCATION"
    );
    assert_eq!(D1_QUERIES_PER_INVOCATION_DEFAULT, 1000);
    let note = "# IMPRESSPRESS_D1_QUERIES_PER_INVOCATION is how many D1 queries one \
                invocation may run: 1000 on Workers Free and Paid alike. Lower it only for a \
                Worker whose limits.subrequests is lower, with \
                [cloudflare].d1_queries_per_invocation in impresspress.toml.\n\n";

    for (limit, line) in [
        (
            D1_QUERIES_PER_INVOCATION_DEFAULT,
            "IMPRESSPRESS_D1_QUERIES_PER_INVOCATION = \"1000\"\n",
        ),
        (50, "IMPRESSPRESS_D1_QUERIES_PER_INVOCATION = \"50\"\n"),
    ] {
        let mut cfg = sample_cfg();
        cfg.d1_queries_per_invocation = limit;
        for path in [
            generate(&cfg, repo_root, &out).unwrap(),
            generate_upload(&cfg, repo_root, &out).unwrap(),
        ] {
            let body = fs::read_to_string(&path).unwrap();
            assert!(
                body.contains(line),
                "{} should state a limit of {limit}:\n{body}",
                path.display()
            );
            assert!(
                body.contains(note),
                "{} should say what the limit depends on:\n{body}",
                path.display()
            );
        }
    }
}

/// The scheduled sweep is opt-in. A generated config carries an *empty*
/// schedule list by default, which is the shape that makes a deploy converge:
/// `wrangler triggers deploy` PUTs the schedule set only when the config
/// defines `triggers.crons`, so `crons = []` clears a schedule an earlier
/// deploy registered while an absent section would leave it running.
#[test]
fn the_default_schedule_is_empty_and_still_writes_the_section() {
    let tmp = tempdir().unwrap();
    let repo_root = tmp.path();
    let out = repo_root.join("target/impresspress-cloudflare");
    fs::create_dir_all(&out).unwrap();

    assert!(
        DEFAULT_CRONS.is_empty(),
        "the sweep must not be scheduled for a consumer that exported no \
         `scheduled` handler; it is enabled by `[cloudflare].crons`"
    );

    for path in [
        generate(&sample_cfg(), repo_root, &out).unwrap(),
        generate_triggers(&sample_cfg(), repo_root, &out).unwrap(),
    ] {
        let body = fs::read_to_string(&path).unwrap();
        assert!(
            body.contains("[triggers]\ncrons = []\n"),
            "{} should say there is no schedule rather than stay silent \
             about it:\n{body}",
            path.display()
        );
    }
}

/// Golden for the generated `[triggers]` section. It is what makes the
/// Worker's `scheduled` handler run at all, so the exact rendered text is the
/// reviewed artifact — not "somewhere in the file there is a cron".
#[test]
fn generate_and_generate_triggers_write_the_configured_schedule_verbatim() {
    let tmp = tempdir().unwrap();
    let repo_root = tmp.path();
    let out = repo_root.join("target/impresspress-cloudflare");
    fs::create_dir_all(&out).unwrap();

    let mut cfg = sample_cfg();
    cfg.crons = vec![SUGGESTED_SWEEP_CRON.to_string()];
    let body = fs::read_to_string(generate(&cfg, repo_root, &out).unwrap()).unwrap();
    assert!(
        body.contains("[triggers]\ncrons = [\"17 3 * * *\"]\n"),
        "expected the suggested daily sweep verbatim:\n{body}"
    );
    assert_eq!(
        SUGGESTED_SWEEP_CRON, "17 3 * * *",
        "the golden above spells the suggested schedule out; keep them in step"
    );

    let mut cfg = sample_cfg();
    cfg.crons = vec!["0 * * * *".into(), "30 4 * * 1".into()];
    // `toml::to_string_pretty` breaks a multi-element array across lines; a
    // single-element one stays inline. Both forms are in the golden so a
    // change to either rendering is a reviewed diff.
    let multi = "[triggers]\ncrons = [\n    \"0 * * * *\",\n    \"30 4 * * 1\",\n]\n";
    let body = fs::read_to_string(generate(&cfg, repo_root, &out).unwrap()).unwrap();
    assert!(
        body.contains(multi),
        "expected the configured schedules verbatim:\n{body}"
    );
    // The config actually handed to `wrangler triggers deploy` is the one that
    // has to carry them — the build-time file is never passed to it.
    let body = fs::read_to_string(generate_triggers(&cfg, repo_root, &out).unwrap()).unwrap();
    assert!(
        body.contains(multi),
        "the triggers config is what applies the schedule:\n{body}"
    );
}

/// Cron triggers are a worker-level setting. `wrangler versions upload` reads
/// `[triggers]` without complaint and does not apply it — its own closing note
/// says so — so an upload config that carried the section would be a lie about
/// what the upload does. `impresspress deploy` is two `versions` commands, so
/// none of the three upload configs may carry it.
#[test]
fn no_upload_config_carries_the_worker_level_triggers_section() {
    let tmp = tempdir().unwrap();
    let repo_root = tmp.path();
    let out = repo_root.join("target/impresspress-cloudflare");
    let staged = out.join("assets");
    fs::create_dir_all(&staged).unwrap();
    fs::write(staged.join("app.js"), b"app").unwrap();
    let release = release_manifest_from_staged_dir(&staged).unwrap();
    let identity = sample_identity();
    let prepared = stage_prepared_module(&out, &sample_plan(&release, &identity)).unwrap();

    let mut cfg = sample_cfg();
    cfg.crons = vec![SUGGESTED_SWEEP_CRON.to_string()];

    for path in [
        generate_upload_with_release(&cfg, repo_root, &out, Some(&release)).unwrap(),
        generate_candidate_upload(&cfg, repo_root, &out, &release, &identity).unwrap(),
        generate_final_upload(&cfg, repo_root, &out, &release, &identity, &prepared).unwrap(),
    ] {
        let body = fs::read_to_string(&path).unwrap();
        assert!(
            !body.contains("triggers"),
            "{} must not claim to carry a schedule it cannot apply:\n{body}",
            path.display()
        );
    }
}

#[test]
fn generate_pins_worker_build_and_skips_reinstall_when_present() {
    let tmp = tempdir().unwrap();
    let repo_root = tmp.path();
    let out = repo_root.join("target/impresspress-cloudflare");
    fs::create_dir_all(&out).unwrap();

    let path = generate(&sample_cfg(), repo_root, &out).unwrap();
    let body = fs::read_to_string(&path).unwrap();

    let expected_pin = format!(r#"--version "={WORKER_BUILD_VERSION}""#);
    assert!(
        body.contains(&expected_pin),
        "build command must pin the exact worker-build version, not a \
         floating semver range:\n{body}"
    );
    assert!(
        body.contains(&format!(r#"= "{WORKER_BUILD_VERSION}""#)),
        "build command must check the installed version before deciding \
         whether to reinstall:\n{body}"
    );
    assert!(
        body.contains("worker-build --no-default-features"),
        "build command must still run worker-build after the check:\n{body}"
    );
}

#[test]
fn generate_merges_overrides_with_consumer_winning() {
    let tmp = tempdir().unwrap();
    let repo_root = tmp.path();
    let out = repo_root.join("target/impresspress-cloudflare");
    fs::create_dir_all(&out).unwrap();

    let overrides_path = repo_root.join("wrangler.overrides.toml");
    fs::write(
        &overrides_path,
        r#"
compatibility_date = "2099-01-01"

[[routes]]
pattern = "wafer.run/*"
zone_name = "wafer.run"
"#,
    )
    .unwrap();

    let mut cfg = sample_cfg();
    cfg.wrangler_overrides_path = Some(
        overrides_path
            .strip_prefix(repo_root)
            .unwrap()
            .to_path_buf(),
    );

    let path = generate(&cfg, repo_root, &out).unwrap();
    let body = fs::read_to_string(&path).unwrap();

    assert!(
        body.contains(r#"compatibility_date = "2099-01-01""#),
        "override primitive should win:\n{body}"
    );
    assert!(
        body.contains(r#"pattern = "wafer.run/*""#),
        "new array entry should be present:\n{body}"
    );
    assert!(
        body.contains(r#"name = "wafer-site""#),
        "non-overridden default should remain:\n{body}"
    );
    assert!(
        body.contains(r#"binding = "DB""#),
        "non-overridden d1 binding should remain:\n{body}"
    );
}

/// The bootstrap-admin email and password are an admin login: a consumer
/// that forwards them to the auth block creates the first admin from them.
/// Under `[vars]` they would be plain text in a committed override file and
/// in the dashboard, so the build refuses each, in `[vars]` and in an
/// environment's own vars, naming where it goes and never echoing the value.
#[test]
fn a_bootstrap_admin_credential_in_the_main_workers_vars_is_refused() {
    use impresspress_core::blocks::auth::config::{
        BOOTSTRAP_ADMIN_EMAIL_KEY, BOOTSTRAP_ADMIN_PASSWORD_KEY,
    };
    let tmp = tempdir().unwrap();
    let repo_root = tmp.path();
    let out = repo_root.join("target/impresspress-cloudflare");
    fs::create_dir_all(&out).unwrap();
    let overrides_path = repo_root.join("wrangler.overrides.toml");
    let mut cfg = sample_cfg();
    cfg.wrangler_overrides_path = Some("wrangler.overrides.toml".into());
    let value = "correct-horse-battery-staple";

    for var in [BOOTSTRAP_ADMIN_EMAIL_KEY, BOOTSTRAP_ADMIN_PASSWORD_KEY] {
        for table in ["[vars]", "[env.staging.vars]"] {
            fs::write(&overrides_path, format!("{table}\n{var} = \"{value}\"\n")).unwrap();
            for result in [
                generate(&cfg, repo_root, &out),
                generate_upload(&cfg, repo_root, &out),
            ] {
                let err = format!("{:#}", result.expect_err(var));
                assert!(
                    err.contains(var)
                        && err.contains(table)
                        && err.contains(&format!("wrangler secret put {var} --name wafer-site`")),
                    "{err}"
                );
                assert!(!err.contains(value), "the error echoed the value: {err}");
            }
        }
    }
}

/// The pepper belongs to the password-hasher Worker, and the main Worker
/// reads none of its settings. Put under the main Worker's `[vars]` through an
/// override file, a key would be plain text in a committed file and in the
/// dashboard and pepper nothing, and `REQUIRED` would require nothing. The
/// build refuses each, naming the var and where it goes, never the value.
#[test]
fn a_pepper_setting_in_the_main_workers_vars_is_refused() {
    use impresspress_password::pepper::{
        PASSWORD_PEPPER_KEY_VAR, PASSWORD_PEPPER_PREVIOUS_KEYS_VAR, PASSWORD_PEPPER_REQUIRED_VAR,
    };
    let tmp = tempdir().unwrap();
    let repo_root = tmp.path();
    let out = repo_root.join("target/impresspress-cloudflare");
    fs::create_dir_all(&out).unwrap();
    let overrides_path = repo_root.join("wrangler.overrides.toml");
    let mut cfg = sample_cfg();
    cfg.wrangler_overrides_path = Some("wrangler.overrides.toml".into());
    let key = "KioqKioqKioqKioqKioqKioqKioqKioqKioqKioqKio=";

    for var in [PASSWORD_PEPPER_KEY_VAR, PASSWORD_PEPPER_PREVIOUS_KEYS_VAR] {
        fs::write(&overrides_path, format!("[vars]\n{var} = \"{key}\"\n")).unwrap();
        for result in [
            generate(&cfg, repo_root, &out),
            generate_upload(&cfg, repo_root, &out),
        ] {
            let err = format!("{:#}", result.expect_err(var));
            assert!(
                err.contains(var)
                    && err.contains("wrangler secret put")
                    && err.contains("--name wafer-site-password-hasher"),
                "{err}"
            );
            assert!(!err.contains(key), "the error echoed the key: {err}");
        }
    }

    // An environment's own vars are read by its deploy just the same.
    fs::write(
        &overrides_path,
        format!("[env.staging.vars]\n{PASSWORD_PEPPER_KEY_VAR} = \"{key}\"\n"),
    )
    .unwrap();
    let err = format!(
        "{:#}",
        generate(&cfg, repo_root, &out).expect_err("env vars")
    );
    assert!(
        err.contains("[env.staging.vars]") && err.contains(PASSWORD_PEPPER_KEY_VAR),
        "{err}"
    );
    assert!(!err.contains(key), "the error echoed the key: {err}");

    fs::write(
        &overrides_path,
        format!("[vars]\n{PASSWORD_PEPPER_REQUIRED_VAR} = \"true\"\n"),
    )
    .unwrap();
    let err = format!(
        "{:#}",
        generate(&cfg, repo_root, &out).expect_err("REQUIRED")
    );
    assert!(
        err.contains(PASSWORD_PEPPER_REQUIRED_VAR) && err.contains("pepper_required"),
        "{err}"
    );

    fs::remove_file(&overrides_path).unwrap();
    cfg.wrangler_overrides_path = None;
    let body = fs::read_to_string(generate(&cfg, repo_root, &out).unwrap()).unwrap();
    assert!(
        body.contains(
            "wrangler secret put IMPRESSPRESS_PASSWORD_PEPPER_KEY --name wafer-site-password-hasher"
        ),
        "the generated file says where the key goes:\n{body}"
    );
}

/// Every config the main Worker is generated with — dev/first deploy, the
/// upload-only candidate and final versions, and the worker-level one — binds
/// the password-hasher's Durable Object class by script name, and writes the
/// shard count. None of them declares a migration: the main Worker implements
/// no class, which is what keeps its version preview URLs.
#[test]
fn every_main_worker_config_binds_the_password_hasher_by_script_name() {
    let tmp = tempdir().unwrap();
    let repo_root = tmp.path();
    let out = repo_root.join("target/impresspress-cloudflare");
    fs::create_dir_all(&out).unwrap();
    let mut cfg = sample_cfg();
    cfg.password_hasher.shards = 5;

    for path in [
        generate(&cfg, repo_root, &out).unwrap(),
        generate_upload(&cfg, repo_root, &out).unwrap(),
        generate_triggers(&cfg, repo_root, &out).unwrap(),
    ] {
        let body = fs::read_to_string(&path).unwrap();
        let parsed: toml::Value = toml::from_str(&body).unwrap();
        assert_eq!(
            parsed["durable_objects"]["bindings"],
            toml::from_str::<toml::Value>(
                r#"bindings = [{ name = "IMPRESSPRESS_PASSWORD_HASHER", class_name = "ImpresspressPasswordHasher", script_name = "wafer-site-password-hasher" }]"#
            )
            .unwrap()["bindings"],
            "{}:\n{body}",
            path.display()
        );
        assert_eq!(
            parsed["vars"]["IMPRESSPRESS_PASSWORD_HASHER_SHARDS"].as_str(),
            Some("5"),
            "{body}"
        );
        assert!(parsed.get("migrations").is_none(), "{body}");
    }
}

/// A consumer override that binds Durable Objects of its own keeps them, and
/// cannot drop or redirect the hasher binding the crypto service needs.
#[test]
fn overrides_cannot_drop_the_password_hasher_binding() {
    let tmp = tempdir().unwrap();
    let repo_root = tmp.path();
    let out = repo_root.join("target/impresspress-cloudflare");
    fs::create_dir_all(&out).unwrap();
    fs::write(
        repo_root.join("wrangler.overrides.toml"),
        r#"[[durable_objects.bindings]]
name = "ROOMS"
class_name = "Room"
script_name = "rooms"

[[durable_objects.bindings]]
name = "IMPRESSPRESS_PASSWORD_HASHER"
class_name = "Elsewhere"
script_name = "elsewhere"
"#,
    )
    .unwrap();
    let mut cfg = sample_cfg();
    cfg.wrangler_overrides_path = Some("wrangler.overrides.toml".into());
    let body = fs::read_to_string(generate_upload(&cfg, repo_root, &out).unwrap()).unwrap();
    let parsed: toml::Value = toml::from_str(&body).unwrap();
    let bindings = parsed["durable_objects"]["bindings"].as_array().unwrap();
    assert_eq!(bindings.len(), 2, "{body}");
    assert!(bindings.iter().any(|b| b["name"].as_str() == Some("ROOMS")));
    let hasher = bindings
        .iter()
        .find(|b| b["name"].as_str() == Some("IMPRESSPRESS_PASSWORD_HASHER"))
        .unwrap();
    assert_eq!(
        hasher["class_name"].as_str(),
        Some("ImpresspressPasswordHasher")
    );
    assert_eq!(
        hasher["script_name"].as_str(),
        Some("wafer-site-password-hasher")
    );
}

/// The password-hasher Worker's config, golden: the SQLite-backed class
/// migration (the only kind the Free plan offers), no public or preview URL,
/// the pepper requirement as its one var, and no D1, R2 or build hook — it is
/// deployed with plain `wrangler deploy` from the artifact the CLI built.
#[test]
fn the_password_hasher_config_is_golden() {
    let tmp = tempdir().unwrap();
    let out = tmp.path().join("target/impresspress-cloudflare");
    fs::create_dir_all(&out).unwrap();
    let mut cfg = sample_cfg();
    cfg.password_hasher.pepper_required = true;
    cfg.head_sampling_rate = 0.25;
    let path = generate_password_hasher(&cfg, &out).unwrap();
    assert_eq!(path, out.join(PASSWORD_HASHER_CONFIG_FILE));
    let body = fs::read_to_string(&path).unwrap();
    let parsed: toml::Value = toml::from_str(&body).unwrap();
    let expected: toml::Value = toml::from_str(
        r#"
name = "wafer-site-password-hasher"
account_id = "test-acct"
main = "../impresspress-password-hasher/build/worker/shim.mjs"
compatibility_date = "2026-05-01"
workers_dev = false
preview_urls = false

[[migrations]]
tag = "v1"
new_sqlite_classes = ["ImpresspressPasswordHasher"]

[vars]
IMPRESSPRESS_PASSWORD_PEPPER_REQUIRED = "true"

[observability]
enabled = true
head_sampling_rate = 0.25
"#,
    )
    .unwrap();
    assert_eq!(parsed, expected, "{body}");
    assert!(
        body.contains("--name wafer-site-password-hasher"),
        "the file says where the pepper secrets go:\n{body}"
    );
}

#[test]
fn generate_upload_uses_prebuilt_artifact_without_build_hook() {
    let tmp = tempdir().unwrap();
    let repo_root = tmp.path();
    let out = repo_root.join("target/impresspress-cloudflare");
    fs::create_dir_all(&out).unwrap();

    // Even a consumer-provided build override must not re-enable compilation
    // in the upload-only configuration.
    let overrides_path = repo_root.join("wrangler.overrides.toml");
    fs::write(
        &overrides_path,
        r#"
[build]
command = "must-not-run"

[[routes]]
pattern = "wafer.run/*"
zone_name = "wafer.run"
"#,
    )
    .unwrap();
    let mut cfg = sample_cfg();
    cfg.wrangler_overrides_path = Some("wrangler.overrides.toml".into());

    let developer_path = generate(&cfg, repo_root, &out).unwrap();
    let upload_path = generate_upload(&cfg, repo_root, &out).unwrap();
    let developer: toml::Value =
        toml::from_str(&fs::read_to_string(developer_path).unwrap()).unwrap();
    let upload: toml::Value = toml::from_str(&fs::read_to_string(&upload_path).unwrap()).unwrap();

    assert_eq!(
        upload_path.file_name().unwrap(),
        "wrangler-upload.toml",
        "the upload config must not overwrite the developer config"
    );
    assert!(
        developer.get("build").is_some(),
        "developer config still needs its build hook"
    );
    assert!(
        upload.get("build").is_none(),
        "upload config must consume the artifact already built by ImpressPress"
    );
    for key in [
        "main",
        "name",
        "compatibility_date",
        "d1_databases",
        "r2_buckets",
        "vars",
        "routes",
        "version_metadata",
    ] {
        assert_eq!(
            upload.get(key),
            developer.get(key),
            "upload and developer configs diverged at {key}"
        );
    }
}

#[test]
fn generate_upload_binds_release_identity_and_exact_key_set_after_overrides() {
    let tmp = tempdir().unwrap();
    let repo_root = tmp.path();
    let out = repo_root.join("target/impresspress-cloudflare");
    let staged = out.join("assets");
    fs::create_dir_all(staged.join("site/media")).unwrap();
    fs::write(staged.join("site/media/hero.webp"), b"hero").unwrap();
    fs::write(staged.join("site/app.js"), b"app").unwrap();
    let release = release_manifest_from_staged_dir(&staged).unwrap();

    // A consumer override cannot detach this Worker version from the release
    // identity the deployer is about to upload and verify.
    fs::write(
        repo_root.join("wrangler.overrides.toml"),
        format!(
            "[vars]\n{RELEASE_ASSET_ID_VAR} = \"wrong\"\n{RELEASE_ASSET_PREFIX_VAR} = \"wrong\"\n"
        ),
    )
    .unwrap();
    let mut cfg = sample_cfg();
    cfg.wrangler_overrides_path = Some("wrangler.overrides.toml".into());

    let path = generate_upload_with_release(&cfg, repo_root, &out, Some(&release)).unwrap();
    let generated: toml::Value = toml::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    let vars = generated["vars"].as_table().unwrap();

    assert_eq!(
        vars[RELEASE_ASSET_ID_VAR].as_str(),
        Some(release.asset_set_sha256.as_str())
    );
    assert_eq!(
        vars[RELEASE_ASSET_PREFIX_VAR].as_str(),
        Some(release.immutable_prefix.as_str())
    );
    assert_eq!(
        vars[RELEASE_ASSET_MANIFEST_VAR].as_str(),
        Some(release.manifest_key().as_str())
    );
    assert!(!vars.contains_key("IMPRESSPRESS_RELEASE_ASSET_KEYS_JSON"));
}

#[test]
fn release_key_sets_larger_than_the_old_cap_are_accepted() {
    // >4KB of logical keys — the old Worker-var budget must be gone.
    let staged = tempdir().unwrap();
    let media = staged.path().join("site/media");
    fs::create_dir_all(&media).unwrap();
    for i in 0..200 {
        fs::write(media.join(format!("image-{i:04}.webp")), b"x").unwrap();
    }
    let release = release_manifest_from_staged_dir(staged.path()).unwrap();
    assert!(release.logical_keys_json().unwrap().len() > 4 * 1024);

    let tmp = tempdir().unwrap();
    let out = tmp.path().join("target/impresspress-cloudflare");
    fs::create_dir_all(&out).unwrap();
    let path =
        generate_upload_with_release(&sample_cfg(), tmp.path(), &out, Some(&release)).unwrap();
    let generated: toml::Value = toml::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    let vars = generated["vars"].as_table().unwrap();

    assert!(!vars.contains_key("IMPRESSPRESS_RELEASE_ASSET_KEYS_JSON"));
    assert_eq!(
        vars[RELEASE_ASSET_KEYS_SHA256_VAR].as_str(),
        Some(release.logical_keys_sha256().unwrap().as_str())
    );
}

#[test]
fn candidate_and_final_configs_reuse_identity_but_only_final_loads_text_plan() {
    let tmp = tempdir().unwrap();
    let out = tmp.path().join("target/impresspress-cloudflare");
    let staged = out.join("assets");
    fs::create_dir_all(&staged).unwrap();
    fs::write(staged.join("app.js"), b"app").unwrap();
    let release = release_manifest_from_staged_dir(&staged).unwrap();
    let identity = sample_identity();
    let plan = sample_plan(&release, &identity);
    let prepared = stage_prepared_module(&out, &plan).unwrap();

    let candidate_path =
        generate_candidate_upload(&sample_cfg(), tmp.path(), &out, &release, &identity).unwrap();
    let final_path = generate_final_upload(
        &sample_cfg(),
        tmp.path(),
        &out,
        &release,
        &identity,
        &prepared,
    )
    .unwrap();
    let candidate: toml::Value =
        toml::from_str(&fs::read_to_string(candidate_path).unwrap()).unwrap();
    let final_cfg: toml::Value = toml::from_str(&fs::read_to_string(final_path).unwrap()).unwrap();

    assert!(candidate.get("build").is_none());
    assert!(final_cfg.get("build").is_none());
    assert_eq!(
        candidate["main"].as_str(),
        Some("../../build/worker/shim.mjs")
    );
    assert_eq!(
        final_cfg["main"].as_str(),
        Some("prepared-runtime/shim.mjs")
    );
    assert!(candidate.get("rules").is_none());
    assert!(final_cfg["rules"].as_array().unwrap().iter().any(|rule| {
        rule["type"].as_str() == Some("Text")
            && rule["fallthrough"].as_bool() == Some(true)
            && rule.to_string().contains(PREPARED_TEXT_GLOB)
    }));

    let candidate_vars = candidate["vars"].as_table().unwrap();
    let final_vars = final_cfg["vars"].as_table().unwrap();
    for key in [
        RELEASE_ASSET_ID_VAR,
        RELEASE_ASSET_PREFIX_VAR,
        RELEASE_ASSET_MANIFEST_VAR,
        RELEASE_ASSET_MANIFEST_SHA256_VAR,
        RELEASE_ASSET_KEYS_SHA256_VAR,
        PREPARED_APPLICATION_ID_VAR,
        PREPARED_APPLICATION_BUILD_SHA256_VAR,
        PREPARED_WAFER_LOCK_IDENTITY_VAR,
    ] {
        assert_eq!(
            candidate_vars[key], final_vars[key],
            "identity drift at {key}"
        );
    }
    assert!(candidate_vars.get(PREPARED_PLAN_HASH_VAR).is_none());
    assert_eq!(
        final_vars[PREPARED_PLAN_HASH_VAR].as_str(),
        Some(plan.plan_hash.as_str())
    );
    assert_eq!(
        final_vars[PREPARED_PLAN_MODULE_SHA256_VAR].as_str(),
        Some(prepared.plan_module_sha256.as_str())
    );
}

#[test]
fn generate_errors_on_missing_overrides_file() {
    let tmp = tempdir().unwrap();
    let mut cfg = sample_cfg();
    cfg.wrangler_overrides_path = Some("does-not-exist.toml".into());
    let out = tmp.path().join("target/impresspress-cloudflare");
    fs::create_dir_all(&out).unwrap();

    let err = generate(&cfg, tmp.path(), &out).unwrap_err();
    assert!(
        err.to_string().contains("does-not-exist.toml"),
        "error should mention the missing path. got: {err}"
    );
}
