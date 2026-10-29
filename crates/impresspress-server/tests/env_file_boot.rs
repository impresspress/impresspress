//! `run` refuses to start when its `.env` has a line it cannot parse.
//!
//! The reported failure: an unquoted value containing a space stopped the
//! load at that line, so the bootstrap admin set on a later line was never
//! created, and the server booted without a word. The lines before the bad
//! one point the database, storage and listener into a temp dir, so a `run`
//! that ignored the error would boot there and serve until the timeout.

use std::time::Duration;

use impresspress_server::{run, AppHooks, IMPRESSPRESS_LISTENER_FLOW};

#[tokio::test]
async fn run_refuses_to_start_on_an_unparseable_env_line() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    let env = format!(
        "IMPRESSPRESS_DB_PATH={db}\n\
         IMPRESSPRESS_STORAGE_ROOT={storage}\n\
         IMPRESSPRESS_LISTEN=127.0.0.1:0\n\
         WAFER_RUN_SHARED__SITE_NAME=My Site\n\
         WAFER_RUN_SHARED__BOOTSTRAP_ADMIN_EMAIL=admin@example.com\n",
        db = root.join("impresspress.db").display(),
        storage = root.join("storage").display(),
    );
    let env_path = root.join(".env");
    std::fs::write(&env_path, env).expect("write .env");

    let outcome = tokio::time::timeout(
        Duration::from_secs(60),
        run(root, IMPRESSPRESS_LISTENER_FLOW, AppHooks::none()),
    )
    .await
    .expect("run booted and served instead of refusing to start");
    let err = outcome.expect_err("run accepted an unparseable .env");
    let shown = format!("{err:#}");
    assert!(shown.contains("refusing to start"), "{shown}");
    assert!(
        shown.contains(&format!("{}:4:", env_path.display())),
        "{shown}"
    );
    assert!(
        !shown.contains("My Site"),
        "the line's value leaked: {shown}"
    );
}
