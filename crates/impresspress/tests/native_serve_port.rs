//! `impresspress serve --port` reaches a native server.
//!
//! The sealed flow used to drop the flag, so the server bound whatever
//! `IMPRESSPRESS_LISTEN` said; the embedded flow dropped it too, though it
//! cannot honor it (the app's own binary chooses its address) and now
//! refuses it instead.

use std::{net::TcpListener, time::Duration};

use impresspress::cli::flows::{embed_native, sealed_native};

/// A port nothing listens on right now.
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind an ephemeral port")
        .local_addr()
        .expect("local addr")
        .port()
}

/// The `.env` points the listener at `env_port`; `--port` names another one,
/// and the server must answer there.
#[tokio::test]
async fn sealed_native_serves_on_the_port_flag_keeping_the_env_host() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    let env_port = free_port();
    let flag_port = free_port();
    assert_ne!(env_port, flag_port);
    let env = format!(
        "IMPRESSPRESS_DB_PATH={db}\n\
         IMPRESSPRESS_STORAGE_ROOT={storage}\n\
         IMPRESSPRESS_MODEL_CACHE_DIR={models}\n\
         IMPRESSPRESS_LISTEN=127.0.0.1:{env_port}\n",
        db = root.join("impresspress.db").display(),
        storage = root.join("storage").display(),
        models = root.join("models").display(),
    );
    std::fs::write(root.join(".env"), env).expect("write .env");

    let probe = async {
        let client = reqwest::Client::new();
        let url = format!("http://127.0.0.1:{flag_port}/health");
        loop {
            if let Ok(response) = client.get(&url).send().await {
                return response.status();
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    };
    let status = tokio::time::timeout(Duration::from_secs(120), async {
        tokio::select! {
            served = sealed_native::serve(root, false, Some(flag_port)) => {
                panic!("the server stopped before answering on --port: {served:?}")
            }
            status = probe => status,
        }
    })
    .await
    .expect("nothing answered on the --port port");
    assert!(status.is_success(), "/health on --port answered {status}");
}

#[tokio::test]
async fn embed_native_refuses_the_port_flag() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let err = embed_native::serve(tmp.path(), false, Some(9123))
        .await
        .expect_err("an embedded native app accepted --port");
    let shown = format!("{err:#}");
    assert!(shown.contains("--port 9123"), "{shown}");
    assert!(shown.contains("IMPRESSPRESS_LISTEN"), "{shown}");
}
