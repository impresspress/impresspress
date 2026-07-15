//! Smoke tests: each `make_*_service` factory returns a non-null
//! `Arc<dyn ...>`. These don't exercise the underlying service in
//! depth — they just catch compile-time type-signature regressions
//! and confirm the factory hands back something usable.

#[test]
fn sqlite_factory_returns_service() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("smoke.db");
    let _svc = impresspress_native::make_sqlite_database_service(path.to_str().unwrap())
        .expect("sqlite factory should succeed in temp dir");
    // If we got here, the factory worked.
}

#[test]
fn local_storage_factory_returns_service() {
    let tmp = tempfile::tempdir().unwrap();
    let _svc = impresspress_native::make_local_storage_service(tmp.path().to_str().unwrap())
        .expect("local storage factory should succeed in temp dir");
}

#[test]
fn network_factory_returns_service() {
    let _svc = impresspress_native::make_fetch_network_service()
        .expect("network factory should succeed with default env");
}

#[test]
fn crypto_factory_returns_service() {
    // ≥ 32 bytes — meets the HMAC-SHA256 minimum the underlying
    // Argon2JwtCryptoService enforces on construction.
    let secret = "smoke-test-secret-padded-to-min-32-bytes-aaaa".to_string();
    let _svc =
        impresspress_native::make_jwt_crypto_service(secret).expect("smoke secret is long enough");
}

#[test]
fn crypto_factory_rejects_short_secret() {
    match impresspress_native::make_jwt_crypto_service("too-short".to_string()) {
        Ok(_) => panic!("short secret must be rejected"),
        Err(e) => {
            let msg = format!("{e}");
            assert!(msg.contains("at least 32 bytes"), "got: {msg}");
        }
    }
}

#[test]
fn logger_factory_returns_service() {
    let _svc = impresspress_native::make_tracing_logger();
}

#[test]
fn infra_config_reads_defaults_when_env_unset() {
    let _cfg = impresspress_native::InfraConfig::from_env();
}

// The env-filter logic is tested as a unit test against the pure
// `filter_app_env_vars` inner function (see `src/env.rs`). Running it as an
// integration test would have to mutate the process environment, which is
// `unsafe` in Rust 2024 and races with parallel test runs.
