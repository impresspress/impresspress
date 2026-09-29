use wafer_block::db::{Filter, FilterOp, SortField};
use wafer_core::clients::database as db;
use wafer_run::{context::Context, Message, OutputStream};

use super::contracts::{AdminAuditLogListQuery, AdminAuditLogListResponse};
use crate::{blocks::crud, http::ok_json};

/// Audit log entries (admin-initiated mutations).
pub(crate) const AUDIT_LOGS_TABLE: &str = "impresspress__admin__audit_logs";

/// Storage access log entries (one row per object read/write).
pub(crate) const STORAGE_ACCESS_LOGS_TABLE: &str = "impresspress__admin__storage_access_logs";

/// `GET /b/admin/api/logs`.
pub(super) async fn handle_list(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let query = AdminAuditLogListQuery::from_message(msg);

    let mut filters = Vec::new();
    if let Some(user_id) = query.user_id {
        filters.push(Filter {
            field: "user_id".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::Value::String(user_id),
        });
    }
    if let Some(action_filter) = query.action {
        filters.push(Filter {
            field: "action".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::Value::String(action_filter),
        });
    }
    if let Some(resource) = query.resource {
        filters.push(Filter {
            field: "resource".to_string(),
            operator: FilterOp::Like,
            value: serde_json::Value::String(format!("%{resource}%")),
        });
    }

    let sort = vec![SortField {
        field: "created_at".to_string(),
        desc: true,
    }];

    match db::paginated_list(
        ctx,
        AUDIT_LOGS_TABLE,
        i64::from(query.page),
        i64::from(query.page_size),
        filters,
        sort,
    )
    .await
    {
        Ok(result) => ok_json(&AdminAuditLogListResponse::from_record_list(&result)),
        Err(e) => crud::db_error_internal(e, "Database error"),
    }
}

// ---------------------------------------------------------------------------
// Audit log helper
// ---------------------------------------------------------------------------

/// Record an admin action in the audit_logs table.
///
/// Runs under the CALLING block's WRAP identity. Blocks other than admin
/// hold only an append-only grant on the table, which refuses an insert
/// naming `id`, `created_at` or `updated_at` — so none is supplied: the
/// database assigns the id and stamps both timestamps itself, which also
/// means no caller can back-date an entry.
///
/// Called after the audited change has committed, so a failed write does
/// not fail the caller's request: the change happened, and answering an
/// error would tell the client it did not and invite a retry of a mutation
/// that already ran. The failure is logged at `error` with its code — an
/// action with no audit row is an incident for an operator to see, not a
/// warning to scroll past.
pub async fn audit_log(
    ctx: &dyn Context,
    user_id: &str,
    action: &str,
    resource: &str,
    ip_address: &str,
) {
    let mut data = std::collections::HashMap::new();
    data.insert("user_id".to_string(), serde_json::json!(user_id));
    data.insert("action".to_string(), serde_json::json!(action));
    data.insert("resource".to_string(), serde_json::json!(resource));
    data.insert("ip_address".to_string(), serde_json::json!(ip_address));

    if let Err(e) = db::create(ctx, AUDIT_LOGS_TABLE, data).await {
        tracing::error!(
            caller = ctx.caller_id().unwrap_or("<none>"),
            action,
            resource,
            code = ?e.code,
            error = %e.message,
            "audit_log write failed; the audited action has no audit row"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{admin_msg, output_json, TestContext};

    /// Every block the admin block grants an append on the audit trail
    /// writes its row there, running as itself. `audit_log` only logs a
    /// refused write, so a dropped or mistyped grant would leave the action
    /// unaudited behind a green response; the row is what is asserted.
    ///
    /// The grantees come off `AdminBlock::info()`, so a grant added there is
    /// covered here without editing this test, and one removed fails it.
    #[tokio::test]
    async fn every_block_granted_an_audit_append_lands_its_row() {
        let grantees: Vec<String> =
            wafer_run::Block::info(&crate::blocks::admin::AdminBlock::new())
                .grants
                .into_iter()
                .filter(|g| {
                    g.resource == AUDIT_LOGS_TABLE && g.write == wafer_block::GrantWrite::Append
                })
                .map(|g| g.grantee)
                .collect();
        assert!(
            grantees.len() >= 4,
            "userportal, products, legalpages and auth-ui append: {grantees:?}"
        );

        let ctx = TestContext::with_admin().await;
        for grantee in &grantees {
            let action = format!("probe.{grantee}");
            audit_log(
                &ctx.clone().running_as(grantee),
                "user-1",
                &action,
                "probe",
                "",
            )
            .await;
            assert_eq!(
                crate::test_support::audit_count(&ctx, &action).await,
                1,
                "{grantee}'s audit row must land"
            );
        }

        // The control: a block with no append grant writes nothing.
        audit_log(
            &ctx.clone().running_as("test/ungranted"),
            "user-1",
            "probe.ungranted",
            "probe",
            "",
        )
        .await;
        assert_eq!(
            crate::test_support::audit_count(&ctx, "probe.ungranted").await,
            0
        );
    }

    /// The audit-log list publishes exactly `AdminAuditLogView`'s fields, and
    /// the `{records, total_count, page, page_size}` envelope the untyped
    /// `RecordList` response already had.
    #[tokio::test]
    async fn list_publishes_exactly_the_contract_fields() {
        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        crate::blocks::admin::migrations::apply(&ctx)
            .await
            .expect("apply admin migrations");
        audit_log(&ctx, "admin-1", "user.delete", "users/u-9", "203.0.113.7").await;

        let body =
            output_json(handle_list(&ctx, &admin_msg("retrieve", "/b/admin/api/logs")).await).await;

        let row = body["records"][0]
            .as_object()
            .expect("one audit entry on the wire");
        let mut got: Vec<&str> = row.keys().map(String::as_str).collect();
        got.sort_unstable();
        assert_eq!(
            got,
            vec![
                "action",
                "created_at",
                "id",
                "ip_address",
                "resource",
                "updated_at",
                "user_id"
            ],
            "the wire field set must equal AdminAuditLogView's"
        );

        assert_eq!(row["action"], serde_json::json!("user.delete"));
        assert_eq!(row["resource"], serde_json::json!("users/u-9"));
        assert_eq!(body["total_count"], serde_json::json!(1));
        assert_eq!(body["page"], serde_json::json!(1));
        assert_eq!(body["page_size"], serde_json::json!(50));
    }

    /// Entries that share a `created_at` page disjointly and completely,
    /// newest-key first. **Fails on the previous wafer-run pin**, whose sorted
    /// select had no tiebreak: SQLite left the tied rows in the order its sort
    /// happened to produce (page 1 `[b, a]`, page 2 `[c]` there), an order no
    /// query promised, so a tied row could land on two pages or on none.
    /// Audit entries written by one request share a millisecond on wasm32.
    #[tokio::test]
    async fn entries_that_tie_on_created_at_page_in_one_stable_order() {
        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        crate::blocks::admin::migrations::apply(&ctx)
            .await
            .expect("apply admin migrations");
        for id in ["c", "a", "b"] {
            let mut data = std::collections::HashMap::new();
            data.insert("id".to_string(), serde_json::json!(id));
            data.insert("user_id".to_string(), serde_json::json!("admin-1"));
            data.insert("action".to_string(), serde_json::json!("user.delete"));
            data.insert(
                "resource".to_string(),
                serde_json::json!(format!("users/{id}")),
            );
            data.insert("ip_address".to_string(), serde_json::json!("203.0.113.7"));
            data.insert(
                "created_at".to_string(),
                serde_json::json!("2026-09-23T00:00:00.000+00:00"),
            );
            db::create(&ctx, AUDIT_LOGS_TABLE, data)
                .await
                .expect("seed an audit entry");
        }

        let mut pages = Vec::new();
        for page in ["1", "2"] {
            let mut msg = admin_msg("retrieve", "/b/admin/api/logs");
            msg.set_meta("req.query.page", page.to_string());
            msg.set_meta("req.query.page_size", "2".to_string());
            let body = output_json(handle_list(&ctx, &msg).await).await;
            let ids: Vec<String> = body["records"]
                .as_array()
                .expect("a records array")
                .iter()
                .map(|row| row["id"].as_str().expect("an id").to_string())
                .collect();
            pages.push(ids);
        }

        assert_eq!(
            pages,
            vec![vec!["c", "b"], vec!["a"]],
            "tied entries must page in primary-key order, newest-key first"
        );
    }

    /// `?action=` filters, and the filter reaches the query through
    /// `AdminAuditLogListQuery` — the type the published parameter schema is
    /// derived from.
    #[tokio::test]
    async fn list_applies_the_declared_query_filters() {
        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        crate::blocks::admin::migrations::apply(&ctx)
            .await
            .expect("apply admin migrations");
        audit_log(&ctx, "admin-1", "user.delete", "users/u-9", "203.0.113.7").await;
        audit_log(
            &ctx,
            "admin-1",
            "role.create",
            "roles/editor",
            "203.0.113.7",
        )
        .await;

        let mut msg = admin_msg("retrieve", "/b/admin/api/logs");
        msg.set_meta("req.query.action", "role.create".to_string());

        let body = output_json(handle_list(&ctx, &msg).await).await;

        assert_eq!(body["total_count"], serde_json::json!(1));
        assert_eq!(
            body["records"][0]["action"],
            serde_json::json!("role.create")
        );
    }

    /// The block every append-grant test acts as: one of the four the admin
    /// block grants the audit table to, append-only.
    const USERPORTAL: &str = "impresspress/userportal";

    /// Stands in for `impresspress/userportal` on a real runtime, so the
    /// database handler sees that block id as the caller exactly as it sees
    /// the real one. `audit` runs the real writer; `update` / `delete` try
    /// to rewrite or erase the row named by the `probe.id` meta.
    struct AsUserportal;

    #[async_trait::async_trait]
    impl wafer_run::Block for AsUserportal {
        fn info(&self) -> wafer_run::BlockInfo {
            wafer_run::BlockInfo::new(USERPORTAL, "0.0.1", "test/probe@v1", "audit-table probe")
        }

        async fn lifecycle(
            &self,
            _ctx: &dyn Context,
            _event: wafer_run::LifecycleEvent,
        ) -> Result<(), wafer_run::WaferError> {
            Ok(())
        }

        async fn handle(
            &self,
            ctx: &dyn Context,
            msg: Message,
            _input: wafer_run::InputStream,
        ) -> OutputStream {
            let id = msg.get_meta("probe.id").to_string();
            let result = match msg.kind.as_str() {
                "audit" => {
                    audit_log(
                        ctx,
                        "admin-1",
                        "portal.button.create",
                        "buttons/b-1",
                        "203.0.113.7",
                    )
                    .await;
                    Ok(())
                }
                "update" => {
                    let data = std::collections::HashMap::from([(
                        "action".to_string(),
                        serde_json::json!("rewritten"),
                    )]);
                    db::update(ctx, AUDIT_LOGS_TABLE, &id, data)
                        .await
                        .map(|_| ())
                }
                "delete" => db::delete(ctx, AUDIT_LOGS_TABLE, &id).await,
                other => panic!("unknown probe op {other}"),
            };
            match result {
                Ok(()) => OutputStream::respond(Vec::new()),
                Err(e) => OutputStream::error(e),
            }
        }
    }

    /// A sealed runtime holding the real admin block (whose `BlockInfo`
    /// declares the audit-table grants), the real database block over
    /// in-memory SQLite with the admin schema applied, and [`AsUserportal`].
    async fn runtime_with_userportal() -> (
        wafer_run::Wafer,
        std::sync::Arc<dyn wafer_core::interfaces::database::service::DatabaseService>,
    ) {
        let sqlite: std::sync::Arc<dyn wafer_core::interfaces::database::service::DatabaseService> =
            std::sync::Arc::new(
                wafer_block_sqlite::service::SQLiteDatabaseService::open_in_memory()
                    .expect("open in-memory sqlite"),
            );
        crate::migration_helper::apply_ddl_via_service(
            &sqlite,
            crate::blocks::admin::migrations::ddl_files("sqlite"),
        )
        .await
        .expect("apply admin migrations");

        let mut wafer = wafer_run::Wafer::builder()
            .disable_inventory()
            .disable_lockfile()
            .build()
            .expect("build a bare runtime");
        wafer.set_admin_block(crate::blocks::admin::ADMIN_BLOCK_ID);
        wafer_core::service_blocks::database::register_with_tables(
            &mut wafer,
            sqlite.clone(),
            Vec::new(),
        )
        .expect("register the database block");
        // The admin block `requires` both, and seal refuses a block whose
        // requires names an unregistered one; every target registers them.
        wafer_core::service_blocks::config::register_with(
            &mut wafer,
            std::sync::Arc::new(wafer_core::service_blocks::config::EnvConfigService::new()),
        )
        .expect("register the config block");
        wafer_core::service_blocks::crypto::register_with(
            &mut wafer,
            std::sync::Arc::new(crate::test_support::real_crypto_service()),
        )
        .expect("register the crypto block");
        wafer
            .register_block(
                crate::blocks::admin::ADMIN_BLOCK_ID,
                std::sync::Arc::new(crate::blocks::admin::AdminBlock::new()),
            )
            .expect("register the admin block");
        wafer
            .register_block(USERPORTAL, std::sync::Arc::new(AsUserportal))
            .expect("register the userportal probe");
        wafer.seal().await.expect("seal");
        (wafer, sqlite)
    }

    async fn run_probe(
        wafer: &wafer_run::Wafer,
        op: &str,
        id: &str,
    ) -> Result<(), wafer_run::WaferError> {
        let mut msg = Message::new(op);
        msg.set_meta("probe.id", id.to_string());
        match wafer
            .run_block(USERPORTAL, msg, wafer_run::InputStream::empty())
            .await
            .collect_buffered()
            .await
        {
            Ok(_) => Ok(()),
            Err(wafer_run::TerminalNotResponse::Error(e)) => Err(e),
            Err(other) => panic!("{op}: neither a response nor an error: {other:?}"),
        }
    }

    /// Userportal holds an append-only grant on the audit table, through the
    /// real runtime's WRAP check and the real database handler: the audit
    /// writer's row lands, stamped by the database, and the same block is
    /// refused when it tries to rewrite or erase that row.
    #[tokio::test]
    async fn an_append_grantee_writes_audit_rows_and_cannot_rewrite_or_erase_them() {
        let (wafer, sqlite) = runtime_with_userportal().await;

        run_probe(&wafer, "audit", "")
            .await
            .expect("the audit write");
        let rows = sqlite
            .list(AUDIT_LOGS_TABLE, &wafer_block::db::ListOptions::default())
            .await
            .expect("read the audit table host-side")
            .records;
        assert_eq!(rows.len(), 1, "the audit row must land: {rows:?}");
        let row = &rows[0];
        assert_eq!(
            row.data["action"],
            serde_json::json!("portal.button.create")
        );
        assert!(!row.id.is_empty(), "the database assigns the id");
        for stamp in ["created_at", "updated_at"] {
            assert!(
                row.data[stamp].as_str().is_some_and(|s| !s.is_empty()),
                "the database stamps {stamp}: {row:?}"
            );
        }

        for op in ["update", "delete"] {
            let Err(err) = run_probe(&wafer, op, &row.id).await else {
                panic!("an append grant must not admit {op}");
            };
            assert_eq!(
                err.code,
                wafer_run::ErrorCode::PermissionDenied,
                "{op}: {err:?}"
            );
        }
        let after = sqlite
            .get(AUDIT_LOGS_TABLE, &row.id)
            .await
            .expect("the row survives");
        assert_eq!(after.data, row.data, "the refused ops changed nothing");
    }
}
