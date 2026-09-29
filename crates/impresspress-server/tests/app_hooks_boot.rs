//! A consumer's hooks and listener flow reach the running server.
//!
//! Drives `start_native` — the boot `run` performs before it serves — with
//! both hooks set and a listener flow other than impresspress's `site-main`,
//! then sends a real HTTP request to the bound socket: the block the
//! `register_blocks` hook added answers it, through the flow the
//! `register_post_build` hook registered, which the listener dispatched to.

use std::{collections::HashMap, sync::Arc, time::Duration};

use impresspress_native::{InfraConfig, ListenerEnv};
use impresspress_server::{start_native, AppHooks};
use wafer_run::{context::Context, InputStream, LifecycleEvent, Message, OutputStream, WaferError};

const PROBE: &str = "test/probe";
const PROBE_FLOW: &str = "test-main";
const ANSWER: &str = "answered by the consumer's block";

struct Probe;

#[wafer_block::wafer_async_trait]
impl wafer_run::Block for Probe {
    fn info(&self) -> wafer_run::BlockInfo {
        wafer_run::BlockInfo::new(PROBE, "0.0.1", "http-handler@v1", "answers every request")
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        _event: LifecycleEvent,
    ) -> Result<(), WaferError> {
        Ok(())
    }

    async fn handle(&self, _ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        OutputStream::respond(ANSWER.as_bytes().to_vec())
    }
}

/// A port nothing is listening on, for the listener to bind.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind an ephemeral port")
        .local_addr()
        .expect("local addr")
        .port()
}

#[tokio::test]
async fn the_consumers_hooks_and_listener_flow_serve_requests() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let storage_root = tmp.path().join("storage");
    std::fs::create_dir_all(&storage_root).expect("create storage root");
    let port = free_port();
    let infra = InfraConfig {
        listen: format!("127.0.0.1:{port}"),
        db_type: "sqlite".to_string(),
        db_path: tmp
            .path()
            .join("hooks.sqlite3")
            .to_str()
            .unwrap()
            .to_string(),
        db_url: None,
        storage_type: "local".to_string(),
        storage_root: storage_root.to_str().unwrap().to_string(),
        model_cache_dir: "data/models".to_string(),
        listener: ListenerEnv::default(),
    };
    let database = impresspress_native::make_database_service(&infra.db_type, &infra.db_path, None)
        .await
        .expect("construct sqlite database service");

    let hooks = AppHooks {
        register_blocks: Box::new(|builder| Ok(builder.extra_block(PROBE, Arc::new(Probe)))),
        register_post_build: Box::new(|wafer, _storage| {
            wafer.add_flow_json(&format!(
                r#"{{ "id": "{PROBE_FLOW}", "name": "Probe", "version": "0.1.0",
                     "steps": [ {{ "id": "probe", "block": "{PROBE}" }} ] }}"#
            ))?;
            Ok(())
        }),
    };
    let wafer = start_native(
        &infra,
        database,
        &HashMap::new(),
        Default::default(),
        false,
        PROBE_FLOW,
        hooks,
    )
    .await
    .expect("the server starts");

    let url = format!("http://127.0.0.1:{port}/anything");
    let mut body = None;
    for _ in 0..50 {
        if let Ok(resp) = reqwest::get(&url).await {
            body = Some((
                resp.status().as_u16(),
                resp.text().await.unwrap_or_default(),
            ));
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let (status, body) = body.expect("the listener accepts a connection");
    assert_eq!(status, 200, "{body}");
    assert_eq!(body, ANSWER);

    wafer.shutdown().await;
}
