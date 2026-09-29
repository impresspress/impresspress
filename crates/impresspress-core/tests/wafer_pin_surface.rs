//! The dev-sandbox plans use these producer APIs; a wrong pin fails here,
//! naming the missing item, instead of deep inside a block.
#[test]
fn producer_surface_is_pinned() {
    // `generate_webmcp_selected` takes `impl Fn(...)`, so a bare function
    // pointer can't infer the type parameter — call it with concrete
    // arguments instead to prove the whole signature (including
    // `ToolSelection`) resolves in-tree.
    let (_manifest, _refused) = wafer_core::discovery::generate_webmcp_selected(
        &[] as &[wafer_block::types::BlockInfo],
        wafer_block::types::AuthLevel::Public,
        |_block: &wafer_block::types::BlockInfo, ep: &wafer_block::types::BlockEndpoint| ep.auth,
        &[] as &[wafer_core::discovery::ToolSelection],
    );
    let _ = wafer_block::wire::database::EnsureTableRequest {
        table: wafer_block::wire::database::TableDef {
            name: String::new(),
            columns: vec![],
            indexes: vec![],
            primary_key: vec![],
            unique_keys: vec![],
        },
    };
    assert_eq!(wafer_block::abi::HOST_CODEC_JSON, 1);

    let caps = wafer_block::BlockCapabilities {
        schema: true,
        ..wafer_block::BlockCapabilities::none()
    };
    assert!(caps.schema);

    assert_eq!(wafer_block::wrap::SCHEMA_RESOURCE, "__schema__");
}

/// The producer surface the phase-4 adapter work consumes, pinned at the bump
/// so a later pin move fails here by name rather than mid-refactor.
///
/// Every item below arrived in the four upstream PRs this rev carries (#328,
/// #330, #331, #332). Naming one is not adopting it — the call sites land in
/// the PRs that follow.
///
/// Not pinned here: `PasswordScheme` / `Argon2JwtCryptoService::with_password_scheme`
/// and `primitives::{pbkdf2_hash, pbkdf2_verify}`, because `wafer-block-crypto`
/// is not a dependency of `impresspress-core`. Their consumers are
/// `impresspress-browser` and `impresspress-cloudflare`, which pin them when
/// they take the dependency.
#[test]
fn phase_four_producer_surface_is_pinned() {
    // #330: `AuthLevel` derives `Ord`, so the variant order *is* the
    // strictness ladder and the consumer's private `auth_rank` in
    // `endpoint_match.rs` becomes deletable.
    use wafer_block::types::AuthLevel;
    assert!(AuthLevel::Public < AuthLevel::Authenticated);
    assert!(AuthLevel::Authenticated < AuthLevel::Admin);

    // #330: the equality-filter predicate on the wire type, so every vector
    // backend answers a query the same way. An entry with no metadata
    // satisfies only the empty filter.
    let filter = wafer_block::wire::vector::MetadataFilter::default();
    assert!(filter.matches(None));

    // #330: the shared body of a `ConfigSource`, so the resolution rules
    // cannot drift between sources.
    // It takes `impl Fn`, so a fn-pointer annotation cannot name it; call it
    // with concrete arguments instead, the way `generate_webmcp_selected` is
    // named above.
    let resolved: wafer_run::EnvBlockConfig =
        wafer_run::resolve_declared("impresspress/files", &[], |_key: &str| None)
            .expect("no declared keys resolves to an empty config");
    assert_eq!(resolved.get("ANYTHING"), None);

    // #328 / WR-14: one decode policy for SQL result rows, keyed on the
    // declared column type: a JSON column's text is parsed, any other
    // column's text stays a string however JSON-like it looks.
    use wafer_core::interfaces::database::codec;
    let json = codec::JsonColumns::new(["doc"]);
    assert_eq!(
        codec::decode_text("doc", "{\"a\":1}", &json),
        serde_json::json!({"a": 1})
    );
    assert_eq!(
        codec::decode_text("title", "{\"a\":1}", &json),
        serde_json::json!("{\"a\":1}")
    );
    let _: fn(
        serde_json::Value,
        &codec::JsonColumns,
    ) -> wafer_core::interfaces::database::service::Record = codec::record_from_json_row;

    // #328: the three defaulted `DbExec` operations. Named as function items
    // so the signatures resolve without an impl in this crate.
    fn _db_exec_defaults<T: wafer_core::interfaces::database::exec::DbExec>() {
        let _ = T::ensure_schema_table;
        let _ = T::run_schema_table_ddl;
        let _ = T::create_many;
    }

    // #328: the forwarder macro and the `async_trait` re-export its generated
    // `impl` needs. `macro_rules!` has no value form, so existence is pinned
    // by naming the path; the KV-cache decorator writes the ledger in PR 6.
    #[expect(
        unused_imports,
        reason = "naming the path is the only way to pin a `macro_rules!` export; \
                  nothing here expands it"
    )]
    use wafer_core::{forward_database_service, wafer_async_trait};

    // #330: `fuse` discards the fused RRF score, which is why the browser
    // adapter re-implemented RRF instead of calling it.
    let _ = wafer_core::interfaces::vector::fuse_scored;
    let _: f32 = wafer_core::interfaces::vector::DEFAULT_RRF_K;

    // #331: static block registration works on wasm32. `WAFER_STATIC_BLOCKS`
    // is empty on every target where `linkme` works, so the call site needs
    // no `cfg`.
    let _: fn(
        &mut wafer_run::Wafer,
        &[&wafer_block::StaticBlockRegistration],
    ) -> Result<(), wafer_run::RuntimeError> = wafer_run::Wafer::register_static_blocks;
}

/// The two public items upstream PR #333 added, pinned for the same reason as
/// everything above: a pin move that loses them fails here by name.
///
/// This one is not merely convenience. `DbExec::run_execute_returning` has no
/// default body, so a missing pin is normally caught by the compiler at the
/// `impl` — except that BOTH of this repo's `DbExec` impls (`D1DatabaseService`,
/// `BrowserDatabaseService`) are `wasm32`-only, and neither is built by any
/// host job. A host-only build would therefore not notice the method
/// disappearing. This test is the host-side witness.
#[test]
fn take_where_write_path_surface_is_pinned() {
    // #333: the write-path primitive `DbExec::take_where` now dispatches its
    // `DELETE … RETURNING` through, instead of the read-path `run_fetch`.
    // Named as a function item so the signature resolves without an impl in
    // this crate, the same way `_db_exec_defaults` above names #328's.
    fn _db_exec_write_returning<T: wafer_core::interfaces::database::exec::DbExec>() {
        let _ = T::run_execute_returning;
    }

    // #333: the reader-connection count, made `pub` so a test can assert it is
    // exercising the read/write-split topology rather than silently degrading
    // to the single-connection one. `TestContext::new_on_disk` asserts on it.
    let _: fn(&wafer_block_sqlite::service::SQLiteDatabaseService) -> usize =
        wafer_block_sqlite::service::SQLiteDatabaseService::reader_count;
}

/// The stable-list-ordering and error-body surface (#337, #338), pinned for
/// the reason [`take_where_write_path_surface_is_pinned`] states: both
/// `DbExec` impls in this repo are `wasm32`-only, so a host build would not
/// notice one of these disappearing at the `impl`.
#[test]
fn list_tiebreak_and_error_body_surface_is_pinned() {
    // #337: the record-id policy, public so a backend that inserts rows
    // through its own path (the D1 batch insert) mints ids the same way the
    // shared `create` does — a UUIDv7, so a `created_at` tie lists in
    // creation order.
    let id: String = wafer_core::interfaces::database::mint_record_id();
    assert_eq!(
        uuid::Uuid::parse_str(&id)
            .expect("a UUID")
            .get_version_num(),
        7
    );

    // #337: the accessor a backend overrides to memoize introspection — now
    // also the primary key a sorted or paged `list` orders by, which is why
    // both wasm backends implement it.
    fn _db_exec_schema_cache<T: wafer_core::interfaces::database::exec::DbExec>() {
        let _ = T::schema_cache;
        let _ = T::get_primary_key;
    }

    // #337: the builders' `unique_key` argument, and the predicate an executor
    // uses to decide whether it needs the key at all.
    let _: fn(&wafer_block::db::ListOptions) -> bool = wafer_sql_utils::query::orders_rows;
    let _: fn(&str, wafer_sql_utils::Backend) -> (String, Vec<serde_json::Value>) =
        wafer_sql_utils::introspect::build_list_primary_key;

    // #338: the single Error → HTTP rendering, for an adapter that holds a
    // `WaferError` rather than an `OutputStream` (the browser's buffered
    // path). It carries the detail code as the body's `code`.
    let parts: wafer_block::http_codec::HttpResponseParts =
        wafer_block::http_codec::error_to_http_response(
            &wafer_block::WaferError::new(wafer_block::ErrorCode::NotFound, "gone")
                .with_detail_code("not_found"),
        );
    assert_eq!(parts.status, 404);
    let body: serde_json::Value = serde_json::from_slice(&parts.body).expect("a JSON body");
    assert_eq!(body["code"], "not_found");
}
