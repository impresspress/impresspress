//! The `wafer-run/http-listener` settings an operator sets through
//! `IMPRESSPRESS_*` variables reach the listener, and the ones left unset
//! keep the listener's own defaults.
//!
//! Driven through `register_http_listener` and the listener's real `Init`
//! on a sealed runtime, which is where the listener reads and validates its
//! config: a value that arrived is one it checks, so an invalid value that
//! fails `Init` naming its key proves the setting was passed.

use std::{collections::HashMap, sync::Arc};

use impresspress_native::{register_http_listener, ListenerEnv};
use wafer_run::{StaticConfigSource, Wafer};

/// Seal a runtime whose listener is configured from `listener`, then run the
/// listener's `Init`.
async fn init_listener(listener: &ListenerEnv) -> Result<(), String> {
    let mut wafer = Wafer::new(Arc::new(StaticConfigSource::default())).expect("runtime");
    register_http_listener(&mut wafer, "127.0.0.1:0", "site-main", listener);
    wafer.seal().await.map_err(|e| e.to_string())?;
    wafer
        .init_block("wafer-run/http-listener")
        .await
        .map(|_| ())
        .map_err(|e| format!("{e:?}"))
}

#[tokio::test]
async fn unset_settings_keep_the_listeners_defaults() {
    init_listener(&ListenerEnv::default())
        .await
        .expect("the listener starts on its defaults");
}

#[tokio::test]
async fn each_set_setting_reaches_the_listener() {
    for (listener, key) in [
        (
            ListenerEnv {
                trusted_proxies: Some("not-an-ip".into()),
                ..Default::default()
            },
            "trusted_proxies",
        ),
        (
            ListenerEnv {
                header_read_timeout_secs: Some("0".into()),
                ..Default::default()
            },
            "header_read_timeout_secs",
        ),
        (
            ListenerEnv {
                body_read_timeout_secs: Some("0".into()),
                ..Default::default()
            },
            "body_read_timeout_secs",
        ),
        (
            ListenerEnv {
                max_connections: Some("0".into()),
                ..Default::default()
            },
            "max_connections",
        ),
        (
            ListenerEnv {
                write_timeout_secs: Some("0".into()),
                ..Default::default()
            },
            "write_timeout_secs",
        ),
        (
            ListenerEnv {
                shutdown_grace_secs: Some("0".into()),
                ..Default::default()
            },
            "shutdown_grace_secs",
        ),
    ] {
        let error = init_listener(&listener)
            .await
            .expect_err(&format!("an invalid {key} must fail the listener's Init"));
        assert!(error.contains(key), "{key}: {error}");
    }
    // And a valid value is accepted.
    init_listener(&ListenerEnv {
        trusted_proxies: Some("10.0.0.0/8, 192.0.2.1".into()),
        max_connections: Some("64".into()),
        ..Default::default()
    })
    .await
    .expect("valid settings start the listener");
}

/// Each variable feeds its own setting, and an unset one is `None`.
#[test]
fn each_variable_feeds_its_own_setting() {
    let vars: HashMap<&str, &str> = HashMap::from([
        ("IMPRESSPRESS_TRUSTED_PROXIES", "10.0.0.1"),
        ("IMPRESSPRESS_HEADER_READ_TIMEOUT_SECS", "11"),
        ("IMPRESSPRESS_BODY_READ_TIMEOUT_SECS", "12"),
        ("IMPRESSPRESS_MAX_CONNECTIONS", "13"),
        ("IMPRESSPRESS_WRITE_TIMEOUT_SECS", "14"),
    ]);
    let listener = ListenerEnv::from_vars(|key| vars.get(key).map(|v| (*v).to_string()));
    assert_eq!(
        listener,
        ListenerEnv {
            trusted_proxies: Some("10.0.0.1".into()),
            header_read_timeout_secs: Some("11".into()),
            body_read_timeout_secs: Some("12".into()),
            max_connections: Some("13".into()),
            write_timeout_secs: Some("14".into()),
            shutdown_grace_secs: None,
        }
    );
}
