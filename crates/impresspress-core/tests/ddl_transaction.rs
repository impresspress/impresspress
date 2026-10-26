//! `db::ddl_transaction` from a block, through the context, onto native
//! SQLite: the path a block's `Init` will apply a migration file through.
//! The block is authorized exactly as for `db::ddl` (the `__ddl__` resource),
//! and what crosses the host-op boundary keeps what the runner needs: the
//! classified code, the failing statement's position and the database's
//! message, with nothing of the refused statements left behind.

use impresspress_core::{blocks::admin::ADMIN_BLOCK_ID, test_support::TestContext};
use wafer_block::{wire::database::failed_statement_index, ErrorCode};
use wafer_core::clients::database::{self as db, RawStatement};

fn raw(query: &str) -> RawStatement {
    RawStatement {
        query: query.to_string(),
        args: vec![],
    }
}

/// Whether `table` exists, read off the database itself.
async fn exists(ctx: &TestContext, table: &str) -> bool {
    ctx.database_service()
        .schema_table_exists(table)
        .await
        .expect("table lookup")
}

#[tokio::test]
async fn a_block_runs_a_raw_transaction_through_the_context() {
    let ctx = TestContext::with_admin().await;
    let admin = ctx.clone().running_as(ADMIN_BLOCK_ID);

    let err = db::ddl_transaction(
        &admin,
        &[
            raw("CREATE TABLE impresspress__admin__ip1_tx (id TEXT PRIMARY KEY)"),
            raw("INSERT INTO impresspress__admin__ip1_tx (id) VALUES ('k')"),
            raw("INSERT INTO impresspress__admin__ip1_tx (id) VALUES ('k')"),
        ],
    )
    .await
    .expect_err("a taken key refuses the transaction");
    assert_eq!(err.code, ErrorCode::AlreadyExists, "{err:?}");
    assert_eq!(failed_statement_index(&err), Some(2), "{err:?}");
    assert!(
        err.message.contains("impresspress__admin__ip1_tx"),
        "the database's own message: {err:?}"
    );
    assert!(
        !exists(&ctx, "impresspress__admin__ip1_tx").await,
        "the CREATE before the refused statement rolled back"
    );

    let counts = db::ddl_transaction(
        &admin,
        &[
            raw("CREATE TABLE impresspress__admin__ip1_tx (id TEXT PRIMARY KEY)"),
            raw("INSERT INTO impresspress__admin__ip1_tx (id) VALUES ('k')"),
        ],
    )
    .await
    .expect("the transaction commits");
    assert_eq!(counts.len(), 2);
    assert_eq!(counts[1], 1, "the insert's count");
    assert!(exists(&ctx, "impresspress__admin__ip1_tx").await);
}
