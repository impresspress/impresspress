use std::{collections::HashMap, fs};

use impresspress::cli::helpers::cloudflare::env::{load, parse, RawCloudflareConfig};
use tempfile::tempdir;

const FULL_TOML: &str = r#"
[cloudflare]
account_id = "acct-toml"
worker_name = "x"
compatibility_date = "2026-05-01"

[cloudflare.d1]
binding = "DB"
database_name = "x"
database_id = "00000000-0000-0000-0000-000000000000"

[cloudflare.r2]
binding = "STORAGE"
bucket_name = "x-bucket"
release_assets_dir = "data/storage/site/media"
release_assets_prefix = "site/media"
"#;

const BINDINGS_ONLY_TOML: &str = r#"
[cloudflare]

[cloudflare.d1]
binding = "DB"

[cloudflare.r2]
binding = "STORAGE"
"#;

fn fake_env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
    move |name: &str| map.get(name).cloned()
}

fn parse_str(s: &str) -> RawCloudflareConfig {
    let tmp = tempdir().unwrap();
    fs::write(tmp.path().join("impresspress.toml"), s).unwrap();
    parse(tmp.path()).unwrap()
}

#[test]
fn parse_returns_raw_with_optionals_when_only_bindings_present() {
    let raw = parse_str(BINDINGS_ONLY_TOML);
    assert_eq!(raw.d1.binding, "DB");
    assert_eq!(raw.r2.binding, "STORAGE");
    assert!(raw.account_id.is_none());
    assert!(raw.worker_name.is_none());
    assert!(raw.compatibility_date.is_none());
    assert!(raw.d1.database_name.is_none());
    assert!(raw.d1.database_id.is_none());
    assert!(raw.r2.bucket_name.is_none());
    assert!(raw.r2.release_assets_dir.is_none());
    assert!(raw.r2.release_assets_prefix.is_none());
    assert!(raw.deploy_smoke_paths.is_none());
}

#[test]
fn resolve_uses_env_when_toml_missing_values() {
    let raw = parse_str(BINDINGS_ONLY_TOML);
    let env = fake_env(&[
        ("CLOUDFLARE_ACCOUNT_ID", "acct-env"),
        ("IMPRESSPRESS_CLOUDFLARE_WORKER_NAME", "site-env"),
        ("IMPRESSPRESS_CLOUDFLARE_COMPATIBILITY_DATE", "2030-01-01"),
        ("IMPRESSPRESS_CLOUDFLARE_D1_DATABASE_NAME", "db-env"),
        ("IMPRESSPRESS_CLOUDFLARE_D1_DATABASE_ID", "id-env"),
        ("IMPRESSPRESS_CLOUDFLARE_R2_BUCKET_NAME", "bucket-env"),
    ]);
    let cfg = raw.resolve(env).unwrap();
    assert_eq!(cfg.account_id, "acct-env");
    assert_eq!(cfg.worker_name, "site-env");
    assert_eq!(cfg.compatibility_date, "2030-01-01");
    assert_eq!(cfg.d1.binding, "DB");
    assert_eq!(cfg.d1.database_name, "db-env");
    assert_eq!(cfg.d1.database_id, "id-env");
    assert_eq!(cfg.r2.binding, "STORAGE");
    assert_eq!(cfg.r2.bucket_name, "bucket-env");
}

#[test]
fn resolve_uses_toml_when_env_empty() {
    let raw = parse_str(FULL_TOML);
    let cfg = raw.resolve(fake_env(&[])).unwrap();
    assert_eq!(cfg.account_id, "acct-toml");
    assert_eq!(cfg.worker_name, "x");
    assert_eq!(cfg.compatibility_date, "2026-05-01");
    assert_eq!(cfg.d1.database_name, "x");
    assert_eq!(cfg.d1.database_id, "00000000-0000-0000-0000-000000000000");
    assert_eq!(cfg.r2.bucket_name, "x-bucket");
    assert_eq!(
        cfg.r2.release_assets_dir.as_deref(),
        Some(std::path::Path::new("data/storage/site/media"))
    );
    assert_eq!(
        cfg.r2.release_assets_prefix,
        std::path::PathBuf::from("site/media")
    );
}

#[test]
fn resolve_defaults_head_sampling_rate_to_one_when_unset() {
    let raw = parse_str(FULL_TOML);
    let cfg = raw.resolve(fake_env(&[])).unwrap();
    assert_eq!(cfg.head_sampling_rate, 1.0);
}

#[test]
fn resolve_defaults_deploy_smoke_paths_to_health() {
    let raw = parse_str(FULL_TOML);
    let cfg = raw.resolve(fake_env(&[])).unwrap();
    assert_eq!(cfg.deploy_smoke_paths, vec!["/health"]);
}

#[test]
fn resolve_preserves_configured_deploy_smoke_paths_and_trailing_slashes() {
    let configured = FULL_TOML.replace(
        "compatibility_date = \"2026-05-01\"",
        "compatibility_date = \"2026-05-01\"\n\
         deploy_smoke_paths = [\"/catalog/\", \"/catalog/example\"]",
    );
    let cfg = parse_str(&configured).resolve(fake_env(&[])).unwrap();
    assert_eq!(
        cfg.deploy_smoke_paths,
        vec!["/catalog/", "/catalog/example"]
    );
}

/// The sweep is opt-in: a consumer that exported no `scheduled` handler — and
/// the CLI scaffolds none — must not be given a daily failed invocation by a
/// configuration default it never set.
#[test]
fn resolve_defaults_crons_to_no_schedule_and_takes_an_explicit_list() {
    let cfg = parse_str(FULL_TOML).resolve(fake_env(&[])).unwrap();
    assert!(cfg.crons.is_empty());

    let configured = FULL_TOML.replace(
        "compatibility_date = \"2026-05-01\"",
        "compatibility_date = \"2026-05-01\"\ncrons = [\"0 */6 * * *\"]",
    );
    let cfg = parse_str(&configured).resolve(fake_env(&[])).unwrap();
    assert_eq!(cfg.crons, vec!["0 */6 * * *"]);

    // Empty is a real answer, not a mistake: this deployment runs no cron.
    let disabled = FULL_TOML.replace(
        "compatibility_date = \"2026-05-01\"",
        "compatibility_date = \"2026-05-01\"\ncrons = []",
    );
    let cfg = parse_str(&disabled).resolve(fake_env(&[])).unwrap();
    assert!(cfg.crons.is_empty());
}

/// `wrangler deploy` rejects a malformed cron at the very end of a two-stage
/// deployment, after the candidate has been uploaded. Reject it before
/// anything is built.
#[test]
fn resolve_rejects_a_cron_expression_that_is_not_five_fields() {
    for invalid in ["17 3 * *", "17 3 * * * *", "@daily", ""] {
        let configured = FULL_TOML.replace(
            "compatibility_date = \"2026-05-01\"",
            &format!(
                "compatibility_date = \"2026-05-01\"\ncrons = [{}]",
                serde_json::to_string(invalid).unwrap()
            ),
        );
        let err = parse_str(&configured).resolve(fake_env(&[])).unwrap_err();
        assert!(
            err.to_string().contains("five-field cron expression"),
            "{invalid:?} should be refused: {err}"
        );
    }
}

#[test]
fn resolve_rejects_empty_deploy_smoke_path_list() {
    let configured = FULL_TOML.replace(
        "compatibility_date = \"2026-05-01\"",
        "compatibility_date = \"2026-05-01\"\ndeploy_smoke_paths = []",
    );
    let err = parse_str(&configured).resolve(fake_env(&[])).unwrap_err();
    assert!(err.to_string().contains("at least one path"), "{err}");
}

#[test]
fn resolve_rejects_invalid_deploy_smoke_paths() {
    for invalid in [
        "relative",
        "//example.com/path",
        "https://example.com/path",
        "/proxy/https://example.com",
        "/catalog?sort=new",
        "/catalog#featured",
    ] {
        let configured = FULL_TOML.replace(
            "compatibility_date = \"2026-05-01\"",
            &format!(
                "compatibility_date = \"2026-05-01\"\n\
                 deploy_smoke_paths = [{}]",
                serde_json::to_string(invalid).unwrap()
            ),
        );
        let err = parse_str(&configured).resolve(fake_env(&[])).unwrap_err();
        assert!(
            err.to_string().contains("deploy_smoke_paths"),
            "invalid {invalid:?} produced unexpected error: {err}"
        );
    }
}

#[test]
fn resolve_uses_toml_head_sampling_rate() {
    let raw = parse_str(
        r#"
[cloudflare]
account_id = "acct-toml"
worker_name = "x"
compatibility_date = "2026-05-01"
head_sampling_rate = 0.25

[cloudflare.d1]
binding = "DB"
database_name = "x"
database_id = "00000000-0000-0000-0000-000000000000"

[cloudflare.r2]
binding = "STORAGE"
bucket_name = "x-bucket"
"#,
    );
    let cfg = raw.resolve(fake_env(&[])).unwrap();
    assert_eq!(cfg.head_sampling_rate, 0.25);
}

#[test]
fn resolve_env_overrides_toml_head_sampling_rate() {
    let raw = parse_str(FULL_TOML);
    let env = fake_env(&[("IMPRESSPRESS_CLOUDFLARE_HEAD_SAMPLING_RATE", "0.05")]);
    let cfg = raw.resolve(env).unwrap();
    assert_eq!(cfg.head_sampling_rate, 0.05);
}

#[test]
fn resolve_rejects_out_of_range_head_sampling_rate() {
    let raw = parse_str(FULL_TOML);
    let env = fake_env(&[("IMPRESSPRESS_CLOUDFLARE_HEAD_SAMPLING_RATE", "1.5")]);
    let err = raw.resolve(env).unwrap_err();
    assert!(
        err.to_string().contains("out of range"),
        "expected an out-of-range error, got: {err}"
    );
}

#[test]
fn resolve_rejects_unparseable_head_sampling_rate() {
    let raw = parse_str(FULL_TOML);
    let env = fake_env(&[("IMPRESSPRESS_CLOUDFLARE_HEAD_SAMPLING_RATE", "not-a-number")]);
    let err = raw.resolve(env).unwrap_err();
    assert!(
        err.to_string()
            .contains("IMPRESSPRESS_CLOUDFLARE_HEAD_SAMPLING_RATE"),
        "expected the env var name in the error, got: {err}"
    );
}

#[test]
fn resolve_env_overrides_toml() {
    let raw = parse_str(FULL_TOML);
    let env = fake_env(&[
        ("CLOUDFLARE_ACCOUNT_ID", "acct-env-wins"),
        ("IMPRESSPRESS_CLOUDFLARE_D1_DATABASE_ID", "id-env-wins"),
        ("IMPRESSPRESS_CLOUDFLARE_R2_BUCKET_NAME", "bucket-env-wins"),
    ]);
    let cfg = raw.resolve(env).unwrap();
    assert_eq!(cfg.account_id, "acct-env-wins");
    assert_eq!(cfg.d1.database_id, "id-env-wins");
    assert_eq!(cfg.r2.bucket_name, "bucket-env-wins");
    // un-overridden fields stay from toml
    assert_eq!(cfg.worker_name, "x");
    assert_eq!(cfg.d1.database_name, "x");
}

#[test]
fn resolve_errors_naming_env_var_for_missing_database_id() {
    let raw = parse_str(BINDINGS_ONLY_TOML);
    // Provide everything except D1 database_id
    let env = fake_env(&[
        ("CLOUDFLARE_ACCOUNT_ID", "a"),
        ("IMPRESSPRESS_CLOUDFLARE_WORKER_NAME", "w"),
        ("IMPRESSPRESS_CLOUDFLARE_COMPATIBILITY_DATE", "2026-05-01"),
        ("IMPRESSPRESS_CLOUDFLARE_D1_DATABASE_NAME", "n"),
        ("IMPRESSPRESS_CLOUDFLARE_R2_BUCKET_NAME", "b"),
    ]);
    let err = raw.resolve(env).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("IMPRESSPRESS_CLOUDFLARE_D1_DATABASE_ID"),
        "error should name the missing env var. got: {msg}"
    );
    assert!(
        msg.contains("database_id"),
        "error should also reference the toml key. got: {msg}"
    );
}

#[test]
fn resolve_errors_naming_env_var_for_missing_account_id() {
    let raw = parse_str(BINDINGS_ONLY_TOML);
    let env = fake_env(&[
        ("IMPRESSPRESS_CLOUDFLARE_WORKER_NAME", "w"),
        ("IMPRESSPRESS_CLOUDFLARE_COMPATIBILITY_DATE", "2026-05-01"),
        ("IMPRESSPRESS_CLOUDFLARE_D1_DATABASE_NAME", "n"),
        ("IMPRESSPRESS_CLOUDFLARE_D1_DATABASE_ID", "i"),
        ("IMPRESSPRESS_CLOUDFLARE_R2_BUCKET_NAME", "b"),
    ]);
    let err = raw.resolve(env).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("CLOUDFLARE_ACCOUNT_ID"),
        "error should name CLOUDFLARE_ACCOUNT_ID. got: {msg}"
    );
}

#[test]
fn parse_errors_when_section_missing() {
    let tmp = tempdir().unwrap();
    fs::write(tmp.path().join("impresspress.toml"), "# empty\n").unwrap();
    let err = parse(tmp.path()).unwrap_err();
    assert!(
        err.to_string().contains("missing a [cloudflare] section"),
        "expected 'missing a [cloudflare] section' in: {err}"
    );
}

#[test]
fn load_resolves_via_real_env() {
    // Integration: writes a fully-populated toml and expects load() to
    // succeed because the toml itself supplies all required values
    // (independent of process env state).
    let tmp = tempdir().unwrap();
    fs::write(tmp.path().join("impresspress.toml"), FULL_TOML).unwrap();
    let cfg = load(tmp.path()).unwrap();
    assert_eq!(cfg.d1.binding, "DB");
    assert_eq!(cfg.r2.bucket_name, "x-bucket");
}

/// A pattern that cannot compile must fail the deploy at config time. A glob
/// that silently never matches would look identical to a working exclusion
/// right up until the release key inventory blew its 4 KB budget again.
#[test]
fn an_unparseable_release_assets_exclude_glob_is_a_config_error() {
    let toml = FULL_TOML.replace(
        r#"release_assets_prefix = "site/media""#,
        "release_assets_prefix = \"site/media\"\nrelease_assets_exclude = [\"content/guides/[\"]",
    );
    let tmp = tempdir().unwrap();
    fs::write(tmp.path().join("impresspress.toml"), &toml).unwrap();

    let err = load(tmp.path()).unwrap_err();

    assert!(
        err.to_string().contains("release_assets_exclude"),
        "expected the offending key to be named, got: {err}"
    );
}

#[test]
fn release_assets_exclude_globs_are_compiled_and_kept() {
    let toml = FULL_TOML.replace(
        r#"release_assets_prefix = "site/media""#,
        "release_assets_prefix = \"site/media\"\nrelease_assets_exclude = [\"content/guides/**\"]",
    );
    let tmp = tempdir().unwrap();
    fs::write(tmp.path().join("impresspress.toml"), &toml).unwrap();

    let cfg = load(tmp.path()).unwrap();

    assert_eq!(cfg.r2.release_assets_exclude.len(), 1);
    assert!(cfg.r2.release_assets_exclude[0].matches("content/guides/a.state.json"));
    assert!(!cfg.r2.release_assets_exclude[0].matches("content/legal/terms.md"));
}

/// `[cloudflare].d1_queries_per_invocation` is the Worker's D1 query limit:
/// unset is the default 1000, a stated number up to 1000 is taken as it
/// is, and 0 — at or below the audit-row reservation, which the Worker would
/// refuse on every request — or anything above D1's documented maximum of
/// 1000 is refused here instead.
#[test]
fn resolve_d1_queries_per_invocation_defaults_to_1000_and_refuses_out_of_range() {
    let cfg = parse_str(FULL_TOML).resolve(fake_env(&[])).unwrap();
    assert_eq!(cfg.d1_queries_per_invocation, 1000);

    let lowered = FULL_TOML.replace(
        "compatibility_date = \"2026-05-01\"\n",
        "compatibility_date = \"2026-05-01\"\nd1_queries_per_invocation = 50\n",
    );
    let cfg = parse_str(&lowered).resolve(fake_env(&[])).unwrap();
    assert_eq!(cfg.d1_queries_per_invocation, 50);

    let zero = FULL_TOML.replace(
        "compatibility_date = \"2026-05-01\"\n",
        "compatibility_date = \"2026-05-01\"\nd1_queries_per_invocation = 0\n",
    );
    let err = parse_str(&zero)
        .resolve(fake_env(&[]))
        .expect_err("0 is not a D1 query limit")
        .to_string();
    assert!(
        err.contains("cloudflare.d1_queries_per_invocation")
            && err.contains("IMPRESSPRESS_D1_QUERIES_PER_INVOCATION"),
        "{err}"
    );

    let over = FULL_TOML.replace(
        "compatibility_date = \"2026-05-01\"\n",
        "compatibility_date = \"2026-05-01\"\nd1_queries_per_invocation = 1001\n",
    );
    let err = parse_str(&over)
        .resolve(fake_env(&[]))
        .expect_err("D1 runs at most 1000 queries per invocation")
        .to_string();
    assert!(
        err.contains(&format!(
            "from {} to 1000",
            impresspress_core::config_vars::D1_QUERIES_PER_INVOCATION_MIN
        )),
        "{err}"
    );

    let max = FULL_TOML.replace(
        "compatibility_date = \"2026-05-01\"\n",
        "compatibility_date = \"2026-05-01\"\nd1_queries_per_invocation = 1000\n",
    );
    assert_eq!(
        parse_str(&max)
            .resolve(fake_env(&[]))
            .unwrap()
            .d1_queries_per_invocation,
        1000
    );
}

/// `[cloudflare.password_hasher]` is optional: the hasher is named after the
/// main Worker, spread across the default shard count, and requires no pepper.
#[test]
fn the_password_hasher_defaults_follow_the_main_worker() {
    let cfg = parse_str(FULL_TOML).resolve(fake_env(&[])).unwrap();
    assert_eq!(cfg.password_hasher.worker_name, "x-password-hasher");
    assert_eq!(
        cfg.password_hasher.shards,
        impresspress_password::protocol::DEFAULT_SHARDS
    );
    assert!(!cfg.password_hasher.pepper_required);

    // Named after the main Worker as resolved, env overlay included.
    let cfg = parse_str(FULL_TOML)
        .resolve(fake_env(&[(
            "IMPRESSPRESS_CLOUDFLARE_WORKER_NAME",
            "site-env",
        )]))
        .unwrap();
    assert_eq!(cfg.password_hasher.worker_name, "site-env-password-hasher");
}

#[test]
fn the_password_hasher_section_is_read_and_validated() {
    let with = |section: &str| format!("{FULL_TOML}\n[cloudflare.password_hasher]\n{section}\n");
    let cfg = parse_str(&with(
        "worker_name = \"hasher\"\nshards = 3\npepper_required = true",
    ))
    .resolve(fake_env(&[]))
    .unwrap();
    assert_eq!(cfg.password_hasher.worker_name, "hasher");
    assert_eq!(cfg.password_hasher.shards, 3);
    assert!(cfg.password_hasher.pepper_required);

    for (section, needle) in [
        ("shards = 0", "IMPRESSPRESS_PASSWORD_HASHER_SHARDS"),
        ("shards = 65", "IMPRESSPRESS_PASSWORD_HASHER_SHARDS"),
        ("worker_name = \"x\"", "must differ"),
        ("worker_name = \"Upper\"", "lowercase"),
        ("worker_name = \"-dash\"", "lowercase"),
    ] {
        let err = format!(
            "{:#}",
            parse_str(&with(section))
                .resolve(fake_env(&[]))
                .expect_err(section)
        );
        assert!(err.contains(needle), "{section}: {err}");
    }

    // A default name that runs past Cloudflare's 63 characters is refused,
    // pointing at the setting that fixes it.
    let long = "a".repeat(50);
    let err = format!(
        "{:#}",
        parse_str(&FULL_TOML.replace("worker_name = \"x\"", &format!("worker_name = \"{long}\"")))
            .resolve(fake_env(&[]))
            .expect_err("too long")
    );
    assert!(err.contains("password_hasher.worker_name"), "{err}");

    // A misspelt key is an error, not a silently ignored setting.
    let tmp = tempdir().unwrap();
    fs::write(tmp.path().join("impresspress.toml"), with("shard = 3")).unwrap();
    assert!(parse(tmp.path()).is_err());
}
