use std::{collections::HashMap, fs};

use impresspress::cli::helpers::cloudflare::env::{
    load, parse, require_api_token, RawCloudflareConfig,
};
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

#[test]
fn require_api_token_errors_when_unset() {
    // SAFETY: tests in this binary may be parallel; this could flake if
    // another test sets CLOUDFLARE_API_TOKEN concurrently.
    // SAFETY: env mutation is unsafe on Rust 2024 edition; test-only.
    unsafe {
        std::env::remove_var("CLOUDFLARE_API_TOKEN");
    }
    let err = require_api_token().unwrap_err();
    assert!(
        err.to_string().contains("CLOUDFLARE_API_TOKEN"),
        "expected 'CLOUDFLARE_API_TOKEN' in: {err}"
    );
}
