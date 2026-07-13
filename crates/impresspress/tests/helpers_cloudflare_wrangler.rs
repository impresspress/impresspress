use std::fs;

use impresspress::cli::helpers::cloudflare::wrangler::{
    generate, CloudflareConfig, D1Config, R2Config,
};
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
        },
        wrangler_overrides_path: None,
    }
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
    assert!(body.contains(r#"compatibility_date = "2026-05-01""#));
    assert!(body.contains(r#"binding = "DB""#));
    assert!(body.contains(r#"database_name = "wafer-site-prod""#));
    assert!(body.contains(r#"binding = "STORAGE""#));
    assert!(body.contains(r#"bucket_name = "wafer-site-assets""#));
    assert!(body.contains(r#"main = "../../build/worker/shim.mjs""#));
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
