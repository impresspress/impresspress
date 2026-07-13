//! EnvConfigSource: reads env vars for native target.
//!
//! Spec: docs/superpowers/specs/2026-05-15-lazy-block-init-design.md §2

use impresspress_core::config_source::EnvConfigSource;
use wafer_block::ConfigVar;
use wafer_run::ConfigSource;

#[tokio::test]
async fn reads_env_var_for_declared_key() {
    std::env::set_var("TEST__ENV__KEY", "hello");
    let src = EnvConfigSource::new();
    let declared = vec![ConfigVar::new("TEST__ENV__KEY", "doc", "fallback")];
    let cfg = src.load_for_block("test/env", &declared).await.unwrap();
    assert_eq!(cfg.get("TEST__ENV__KEY"), Some("hello"));
    std::env::remove_var("TEST__ENV__KEY");
}

#[tokio::test]
async fn falls_back_to_default_when_env_missing() {
    std::env::remove_var("TEST__ENV__OTHER");
    let src = EnvConfigSource::new();
    let declared = vec![ConfigVar::new("TEST__ENV__OTHER", "doc", "default-v")];
    let cfg = src.load_for_block("test/env", &declared).await.unwrap();
    assert_eq!(cfg.get("TEST__ENV__OTHER"), Some("default-v"));
}

#[tokio::test]
async fn required_missing_returns_error() {
    std::env::remove_var("TEST__ENV__REQ");
    let src = EnvConfigSource::new();
    // optional defaults to false, default is empty → required-with-no-value.
    let declared = vec![ConfigVar::new("TEST__ENV__REQ", "doc", "")];
    let result = src.load_for_block("test/env", &declared).await;
    assert!(matches!(
        result,
        Err(wafer_run::ConfigError::MissingRequired { .. })
    ));
}

#[tokio::test]
async fn optional_missing_is_skipped() {
    std::env::remove_var("TEST__ENV__OPT");
    let src = EnvConfigSource::new();
    let declared = vec![ConfigVar::new("TEST__ENV__OPT", "doc", "").optional()];
    let cfg = src.load_for_block("test/env", &declared).await.unwrap();
    assert_eq!(cfg.get("TEST__ENV__OPT"), None);
}
