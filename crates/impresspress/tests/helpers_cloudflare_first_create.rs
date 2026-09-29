//! `impresspress deploy`'s first-create step against a stand-in `wrangler`.
//!
//! [`Wrangler::at`] runs a shell script that answers the way wrangler does
//! and records its arguments and stdin, so these tests pin the command lines
//! the CLI sends and how it reads the answers — Cloudflare's "no such
//! Worker" error included — without a Cloudflare account. Then the whole
//! first-create sequence runs through that same script: plan, create,
//! secrets.

#![cfg(unix)]

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use impresspress::cli::helpers::cloudflare::{
    first_create::{
        create_main_worker, generate_placeholder, plan_main_worker, resolve_worker_secrets,
        MainWorkerPlan, Wrangler, PLACEHOLDER_MODULE,
    },
    wrangler::{CloudflareConfig, D1Config, PasswordHasherConfig, R2Config},
};

/// How wrangler 4 prints Cloudflare's answer for a Worker the account does
/// not have (its `renderError`: message, then `[code: N]`).
const NOT_FOUND_STDERR: &str = "✘ [ERROR] A request to the Cloudflare API \
(/accounts/acct/workers/scripts/site/deployments) failed.\n\n  This Worker does not exist on \
your account. [code: 10007]\n";

/// A stand-in `wrangler` in `dir`. It appends each invocation's arguments,
/// and for `secret put` the value it read on stdin, to `dir/calls`; the
/// names it was given are what `secret list` answers. Its
/// `deployments status` answer is read from `dir/status` each time: `exists`
/// succeeds, `absent` fails with Cloudflare's 10007, anything else fails
/// with an authentication error. `deploy` flips `dir/status` to `exists`.
fn fake_wrangler(dir: &Path, status: &str) -> PathBuf {
    fs::write(dir.join("status"), status).unwrap();
    let script = dir.join("wrangler");
    fs::write(
        &script,
        format!(
            r#"#!/bin/sh
dir='{dir}'
case "$1 $2" in
  "deployments status")
    echo "$*" >> "$dir/calls"
    case "$(cat "$dir/status")" in
      exists) echo '{{"id":"d1"}}'; exit 0 ;;
      absent) printf '%s' '{not_found}' >&2; exit 1 ;;
      *) echo 'Not logged in.' >&2; exit 1 ;;
    esac ;;
  "secret put")
    echo "$* <- $(cat)" >> "$dir/calls"
    echo "$3" >> "$dir/secrets"
    exit 0 ;;
  "secret list")
    echo "$*" >> "$dir/calls"
    echo 'Fetching secrets...'
    printf '['
    sep=''
    if [ -f "$dir/secrets" ]; then
      while read -r name; do printf '%s{{"name":"%s","type":"secret_text"}}' "$sep" "$name"; sep=','; done < "$dir/secrets"
    fi
    echo ']'
    exit 0 ;;
  deploy*)
    echo "$*" >> "$dir/calls"
    echo exists > "$dir/status"
    exit 0 ;;
esac
echo "unexpected: $*" >> "$dir/calls"
exit 2
"#,
            dir = dir.display(),
            not_found = NOT_FOUND_STDERR,
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    script
}

fn calls(dir: &Path) -> Vec<String> {
    fs::read_to_string(dir.join("calls"))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

fn sample_cfg() -> CloudflareConfig {
    CloudflareConfig {
        account_id: "acct".into(),
        worker_name: "site".into(),
        compatibility_date: "2026-05-01".into(),
        d1: D1Config {
            binding: "DB".into(),
            database_name: "site-db".into(),
            database_id: "00000000-0000-0000-0000-000000000000".into(),
        },
        r2: R2Config {
            binding: "STORAGE".into(),
            bucket_name: "site-assets".into(),
            release_assets_dir: None,
            release_assets_prefix: Default::default(),
            release_assets_exclude: Vec::new(),
        },
        wrangler_overrides_path: None,
        head_sampling_rate: 1.0,
        d1_queries_per_invocation: 1000,
        crons: Vec::new(),
        deploy_smoke_paths: vec!["/health".into()],
        password_hasher: PasswordHasherConfig {
            worker_name: "site-password-hasher".into(),
            shards: 8,
            pepper_required: false,
        },
    }
}

/// The existence check is `wrangler deployments status` on the placeholder
/// config, and each of its three answers reads the way it must.
#[test]
fn the_existence_check_reads_wranglers_three_answers() {
    use impresspress::cli::helpers::cloudflare::first_create::WorkerCommands;

    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("wrangler.toml");
    let wrangler = Wrangler::at(fake_wrangler(tmp.path(), "exists"));

    assert!(wrangler.worker_exists(&config).unwrap());
    fs::write(tmp.path().join("status"), "absent").unwrap();
    assert!(!wrangler.worker_exists(&config).unwrap());
    fs::write(tmp.path().join("status"), "logged-out").unwrap();
    let err = wrangler.worker_exists(&config).unwrap_err().to_string();
    assert!(
        err.contains("Not logged in") && err.contains("unknown"),
        "a failure that is not 10007 is not an absent Worker: {err}"
    );

    let expected = format!("deployments status --json --config {}", config.display());
    assert_eq!(
        calls(tmp.path()),
        [expected.clone(), expected.clone(), expected]
    );
}

/// A site's first deploy, end to end through the stand-in: the Worker is
/// absent, so no token is needed up front; creation deploys the placeholder
/// config and then puts both secrets on stdin; the token handed back is the
/// one put. A second deploy then finds the Worker and wants that token.
#[test]
fn a_first_deploy_creates_the_worker_and_the_next_one_needs_its_token() {
    let tmp = tempfile::tempdir().unwrap();
    let wrangler = Wrangler::at(fake_wrangler(tmp.path(), "absent"));
    let placeholder =
        generate_placeholder(&sample_cfg(), &tmp.path().join("first-create")).unwrap();

    let secrets = resolve_worker_secrets(|_| None, || Ok([0x5a; 32])).unwrap();
    assert_eq!(
        plan_main_worker(&wrangler, &placeholder, "site", &secrets, None).unwrap(),
        MainWorkerPlan::Create
    );

    let token = create_main_worker(&wrangler, &placeholder, "site", &secrets).unwrap();
    assert_eq!(token, "5a".repeat(32));

    let config = placeholder.display();
    assert_eq!(
        calls(tmp.path()),
        [
            format!("deployments status --json --config {config}"),
            format!("deployments status --json --config {config}"),
            format!("deploy --config {config}"),
            format!(
                "secret put IMPRESSPRESS_DEPLOY_TOKEN --config {config} <- {}",
                "5a".repeat(32)
            ),
            format!(
                "secret put WAFER_RUN__AUTH__JWT_SECRET --config {config} <- {}",
                "5a".repeat(32)
            ),
        ]
    );

    let next = resolve_worker_secrets(|_| None, || Ok([0x77; 32])).unwrap();
    let err = plan_main_worker(&wrangler, &placeholder, "site", &next, None)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("IMPRESSPRESS_DEPLOY_TOKEN is not set"),
        "{err}"
    );
    assert_eq!(
        plan_main_worker(&wrangler, &placeholder, "site", &next, Some(token.clone())).unwrap(),
        MainWorkerPlan::Existing {
            deploy_token: token
        }
    );
    let status = format!("deployments status --json --config {config}");
    let list = format!("secret list --format json --config {config}");
    assert_eq!(
        calls(tmp.path()).split_off(5),
        [status.clone(), list.clone(), status, list],
        "a Worker that holds both secrets is given neither again"
    );
}

/// The placeholder names the main Worker and its account, serves the module
/// written beside it, and binds nothing the application needs: it has
/// nothing migrated to serve.
#[test]
fn the_placeholder_names_the_worker_and_binds_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let path = generate_placeholder(&sample_cfg(), tmp.path()).unwrap();
    let value: toml::Value = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let table = value.as_table().unwrap();
    let mut keys: Vec<&str> = table.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "account_id",
            "compatibility_date",
            "main",
            "name",
            "preview_urls"
        ]
    );
    assert_eq!(table["name"].as_str(), Some("site"));
    assert_eq!(table["account_id"].as_str(), Some("acct"));
    assert_eq!(table["main"].as_str(), Some(PLACEHOLDER_MODULE));
    let module = fs::read_to_string(tmp.path().join(PLACEHOLDER_MODULE)).unwrap();
    assert!(module.contains("status: 503"), "{module}");
}
