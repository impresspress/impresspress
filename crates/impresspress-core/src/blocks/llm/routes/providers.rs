//! Provider CRUD (admin-only).
//!
//! These endpoints back the LLM admin UI's provider management. All writes
//! reload the in-memory `ProviderLlmService` from the DB so chat requests
//! pick up the new configuration without restarting the process.

use wafer_core::clients::{config, database as db};
use wafer_run::{context::Context, InputStream, Message, OutputStream};

use crate::{
    blocks::llm::{
        provider_admin::ProviderAdmin,
        providers::config::{ProviderConfig, ProviderProtocol},
        schema::{config_to_row, row_to_config, TABLE as PROVIDERS_TABLE},
        LlmBlock,
    },
    http::{err_bad_request, err_internal, err_not_found, ok_json},
    util::path_param,
};

/// Body shape for `POST /b/llm/api/providers` and `PATCH /b/llm/api/providers/:id`.
///
/// Every field is optional so the same struct can serve both create (which
/// validates required fields after parsing) and patch.
#[derive(serde::Deserialize, Default)]
struct ProviderBody {
    name: Option<String>,
    protocol: Option<String>,
    endpoint: Option<String>,
    key_var: Option<String>,
    models: Option<Vec<String>>,
    enabled: Option<bool>,
}

/// Path prefix preceding the provider id in the JSON API routes.
const PROVIDERS_PREFIX: &str = "/b/llm/api/providers/";

/// Render a `ProviderConfig` as the JSON shape returned by list/create/update.
fn provider_to_json(id: &str, cfg: &ProviderConfig) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "name": cfg.name,
        "protocol": cfg.protocol.as_str(),
        "endpoint": cfg.endpoint,
        "key_var": cfg.key_var,
        "models": cfg.models,
        "enabled": cfg.enabled,
    })
}

/// Reload all enabled providers from the DB and push the snapshot into the
/// in-memory provider router via [`ProviderAdmin::configure`].
///
/// This is the single choke point where stored rows become live
/// `ProviderConfig`s: rows are decoded via [`row_to_config`] (which never
/// yields an `api_key`) and each config's `key_var` is resolved into
/// `api_key` here, via the config client, before `configure()`. Secret
/// rotation therefore takes effect on the next reload (boot or any provider
/// CRUD write), not per chat request.
///
/// Shared by the provider CRUD handlers, `LlmBlock::lifecycle(Init)`, and
/// the one-shot legacy-provider migration (which is why it takes the
/// provider-admin handle rather than the whole block).
///
/// Errors are returned to the caller; callers translate to 500. We do not
/// silently swallow — a failure here means the in-memory service is stale
/// and the admin needs to know.
pub(in crate::blocks::llm) async fn reload_provider_service(
    ctx: &dyn Context,
    provider_admin: &dyn ProviderAdmin,
) -> Result<(), String> {
    let records = db::list_all(ctx, PROVIDERS_TABLE, vec![])
        .await
        .map_err(|e| format!("provider reload list failed: {e}"))?;
    let mut configs: Vec<ProviderConfig> = Vec::with_capacity(records.len());
    for rec in &records {
        match row_to_config(rec) {
            Ok(mut cfg) if cfg.enabled => {
                resolve_provider_key(ctx, &mut cfg).await;
                configs.push(cfg);
            }
            Ok(_) => {} // disabled — skip
            Err(e) => {
                // A malformed row should not poison the whole reload —
                // drop just that one.
                tracing::warn!("skipping malformed provider row {}: {e}", rec.id);
            }
        }
    }
    provider_admin.configure(configs);
    Ok(())
}

/// Resolve a provider's `key_var` into its plaintext `api_key` via the
/// config client. `key_var` takes precedence over any inline `api_key`;
/// with no `key_var` the config is left untouched.
///
/// Resolution failure (unset var, empty value, denied read) is logged and
/// leaves `api_key` as-is — the provider then runs unauthenticated, and the
/// per-protocol encoder decides whether that's an error (`MissingApiKey` →
/// 401) on the next chat call. Local OpenAI-compatible servers legitimately
/// run without a key.
async fn resolve_provider_key(ctx: &dyn Context, cfg: &mut ProviderConfig) {
    let Some(var) = cfg.key_var.as_deref() else {
        return;
    };
    match config::get(ctx, var).await {
        Ok(value) if !value.is_empty() => cfg.api_key = Some(value),
        Ok(_) => tracing::warn!(
            "provider '{}': key_var `{var}` is set but empty — provider will run unauthenticated",
            cfg.name
        ),
        Err(e) => tracing::warn!(
            "provider '{}': failed to resolve key_var `{var}`: {e} — provider will run unauthenticated",
            cfg.name
        ),
    }
}

/// `GET /b/llm/api/providers` — list all rows. Admin-only.
pub(in crate::blocks::llm) async fn list_providers(
    _block: &LlmBlock,
    ctx: &dyn Context,
    _msg: &Message,
) -> OutputStream {
    let records = match db::list_all(ctx, PROVIDERS_TABLE, vec![]).await {
        Ok(r) => r,
        Err(e) => return err_internal("Database error", e),
    };
    let providers: Vec<serde_json::Value> = records
        .iter()
        .filter_map(|rec| {
            row_to_config(rec)
                .ok()
                .map(|cfg| provider_to_json(&rec.id, &cfg))
        })
        .collect();
    ok_json(&serde_json::json!({ "providers": providers }))
}

/// `POST /b/llm/api/providers` — create. Body must include `name`,
/// `protocol`, `endpoint`. `key_var`, `models`, `enabled` optional. Admin-only.
pub(in crate::blocks::llm) async fn create_provider(
    block: &LlmBlock,
    ctx: &dyn Context,
    _msg: &Message,
    input: InputStream,
) -> OutputStream {
    let raw = input.collect_to_bytes().await;
    let body: ProviderBody = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };

    let Some(name) = body
        .name
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
    else {
        return err_bad_request("`name` is required");
    };
    let Some(protocol_str) = body.protocol.as_deref().filter(|s| !s.is_empty()) else {
        return err_bad_request("`protocol` is required");
    };
    let Some(protocol) = ProviderProtocol::parse(protocol_str) else {
        return err_bad_request(&format!(
            "invalid `protocol` `{protocol_str}` — expected `open_ai`, `anthropic`, or `open_ai_compatible`"
        ));
    };
    let Some(endpoint) = body
        .endpoint
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
    else {
        return err_bad_request("`endpoint` is required");
    };

    let mut cfg = ProviderConfig::new(name, protocol, endpoint);
    if let Some(k) = body.key_var.filter(|s| !s.is_empty()) {
        cfg.key_var = Some(k);
    }
    if let Some(m) = body.models {
        cfg.models = m;
    }
    if let Some(e) = body.enabled {
        cfg.enabled = e;
    }

    let mut data = config_to_row(&cfg);
    crate::util::stamp_created(&mut data);

    let record = match db::create(ctx, PROVIDERS_TABLE, data).await {
        Ok(r) => r,
        Err(e) => return err_internal("Database error", e),
    };

    if let Err(e) = reload_provider_service(ctx, block.provider_admin.as_ref()).await {
        return err_internal("reload_provider_service failed", e);
    }

    ok_json(&provider_to_json(&record.id, &cfg))
}

/// `PATCH /b/llm/api/providers/:id` — partial update. Admin-only.
pub(in crate::blocks::llm) async fn update_provider(
    block: &LlmBlock,
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    let id = path_param(msg, "id", PROVIDERS_PREFIX).to_string();
    if id.is_empty() {
        return err_bad_request("Missing provider ID");
    }

    let raw = input.collect_to_bytes().await;
    let body: ProviderBody = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };

    // Load existing record so we can apply the patch on top of stored values.
    let existing = match db::get(ctx, PROVIDERS_TABLE, &id).await {
        Ok(r) => r,
        Err(e) if e.code == wafer_run::ErrorCode::NotFound => {
            return err_not_found("Provider not found")
        }
        Err(e) => return err_internal("Database error", e),
    };
    let mut cfg = match row_to_config(&existing) {
        Ok(c) => c,
        Err(e) => return err_internal("Stored provider row invalid", e),
    };

    if let Some(n) = body.name.filter(|s| !s.is_empty()) {
        cfg.name = n;
    }
    if let Some(p) = body.protocol.as_deref().filter(|s| !s.is_empty()) {
        match ProviderProtocol::parse(p) {
            Some(parsed) => cfg.protocol = parsed,
            None => {
                return err_bad_request(&format!(
                    "invalid `protocol` `{p}` — expected `open_ai`, `anthropic`, or `open_ai_compatible`"
                ))
            }
        }
    }
    if let Some(e) = body.endpoint.filter(|s| !s.is_empty()) {
        cfg.endpoint = e;
    }
    if let Some(k) = body.key_var {
        cfg.key_var = if k.is_empty() { None } else { Some(k) };
    }
    if let Some(m) = body.models {
        cfg.models = m;
    }
    if let Some(e) = body.enabled {
        cfg.enabled = e;
    }

    let mut data = config_to_row(&cfg);
    crate::util::stamp_updated(&mut data);

    let record = match db::update(ctx, PROVIDERS_TABLE, &id, data).await {
        Ok(r) => r,
        Err(e) if e.code == wafer_run::ErrorCode::NotFound => {
            return err_not_found("Provider not found")
        }
        Err(e) => return err_internal("Database error", e),
    };

    if let Err(e) = reload_provider_service(ctx, block.provider_admin.as_ref()).await {
        return err_internal("reload_provider_service failed", e);
    }

    ok_json(&provider_to_json(&record.id, &cfg))
}

/// `DELETE /b/llm/api/providers/:id` — remove. Admin-only.
pub(in crate::blocks::llm) async fn delete_provider(
    block: &LlmBlock,
    ctx: &dyn Context,
    msg: &Message,
) -> OutputStream {
    let id = path_param(msg, "id", PROVIDERS_PREFIX).to_string();
    if id.is_empty() {
        return err_bad_request("Missing provider ID");
    }
    match db::delete(ctx, PROVIDERS_TABLE, &id).await {
        Ok(()) => {}
        Err(e) if e.code == wafer_run::ErrorCode::NotFound => {
            return err_not_found("Provider not found")
        }
        Err(e) => return err_internal("Database error", e),
    }

    if let Err(e) = reload_provider_service(ctx, block.provider_admin.as_ref()).await {
        return err_internal("reload_provider_service failed", e);
    }

    ok_json(&serde_json::json!({ "deleted": true }))
}

/// `POST /b/llm/api/providers/:id/discover-models` — call the provider's
/// `/v1/models` endpoint, persist the discovered list back to the row, and
/// return the new model list. Admin-only.
pub(in crate::blocks::llm) async fn discover_models(
    block: &LlmBlock,
    ctx: &dyn Context,
    msg: &Message,
) -> OutputStream {
    let id = path_param(msg, "id", PROVIDERS_PREFIX).to_string();
    if id.is_empty() {
        return err_bad_request("Missing provider ID");
    }

    // Resolve the provider name from the row — discover_models is keyed by
    // provider name (== ProviderConfig::name), not by row id.
    let existing = match db::get(ctx, PROVIDERS_TABLE, &id).await {
        Ok(r) => r,
        Err(e) if e.code == wafer_run::ErrorCode::NotFound => {
            return err_not_found("Provider not found")
        }
        Err(e) => return err_internal("Database error", e),
    };
    let mut cfg = match row_to_config(&existing) {
        Ok(c) => c,
        Err(e) => return err_internal("Stored provider row invalid", e),
    };

    // Make sure the in-memory service knows about this provider — discover
    // looks up by name, and the service may be empty if the process just
    // started or the row is disabled (and so was excluded from the last
    // configure call).
    if let Err(e) = reload_provider_service(ctx, block.provider_admin.as_ref()).await {
        return err_internal("reload_provider_service failed", e);
    }

    let models = match block.provider_admin.discover_models(&cfg.name).await {
        Ok(m) => m,
        Err(e) => return err_internal("discover_models failed", format!("{e:?}")),
    };
    cfg.models = models.into_iter().map(|m| m.model_id).collect();

    let mut data = config_to_row(&cfg);
    crate::util::stamp_updated(&mut data);
    if let Err(e) = db::update(ctx, PROVIDERS_TABLE, &id, data).await {
        return err_internal("Database error", e);
    }

    if let Err(e) = reload_provider_service(ctx, block.provider_admin.as_ref()).await {
        return err_internal("reload_provider_service failed", e);
    }

    ok_json(&serde_json::json!({ "models": cfg.models }))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use wafer_run::{streams::output::TerminalNotResponse, ErrorCode};

    use super::*;
    use crate::blocks::llm::routes::test_support::{admin_msg, stub_block, PanicCtx};

    #[tokio::test]
    async fn create_provider_returns_bad_request_on_invalid_json() {
        let block = stub_block();
        let ctx = PanicCtx;
        let msg = admin_msg("create", "/b/llm/api/providers");
        let input = InputStream::from_bytes(b"not json".to_vec());

        let out = create_provider(&block, &ctx, &msg, input).await;
        match out.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::InvalidArgument);
                assert!(
                    e.message.contains("Invalid body"),
                    "expected Invalid body, got: {}",
                    e.message
                );
            }
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_provider_requires_name() {
        let block = stub_block();
        let ctx = PanicCtx;
        let msg = admin_msg("create", "/b/llm/api/providers");
        let input =
            InputStream::from_bytes(br#"{"protocol":"open_ai","endpoint":"https://x"}"#.to_vec());

        let out = create_provider(&block, &ctx, &msg, input).await;
        match out.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::InvalidArgument);
                assert!(e.message.contains("name"), "got: {}", e.message);
            }
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_provider_rejects_unknown_protocol() {
        let block = stub_block();
        let ctx = PanicCtx;
        let msg = admin_msg("create", "/b/llm/api/providers");
        let input = InputStream::from_bytes(
            br#"{"name":"x","protocol":"openai","endpoint":"https://x"}"#.to_vec(),
        );

        let out = create_provider(&block, &ctx, &msg, input).await;
        match out.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::InvalidArgument);
                assert!(e.message.contains("protocol"), "got: {}", e.message);
            }
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn update_provider_requires_id() {
        let block = stub_block();
        let ctx = PanicCtx;
        // Path has no id segment after the prefix.
        let msg = admin_msg("update", "/b/llm/api/providers/");
        let input = InputStream::from_bytes(b"{}".to_vec());

        let out = update_provider(&block, &ctx, &msg, input).await;
        match out.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::InvalidArgument);
                assert!(e.message.contains("provider ID"), "got: {}", e.message);
            }
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn delete_provider_requires_id() {
        let block = stub_block();
        let ctx = PanicCtx;
        let msg = admin_msg("delete", "/b/llm/api/providers/");

        let out = delete_provider(&block, &ctx, &msg).await;
        match out.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::InvalidArgument);
            }
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
    }

    #[test]
    fn extract_provider_id_from_path() {
        // Direct id at end of path
        let mut m = Message::new("update:/b/llm/api/providers/abc123");
        m.set_meta(wafer_run::META_REQ_RESOURCE, "/b/llm/api/providers/abc123");
        assert_eq!(path_param(&m, "id", PROVIDERS_PREFIX), "abc123");

        // Id followed by a sub-resource (discover-models)
        let mut m2 = Message::new("create:/b/llm/api/providers/abc123/discover-models");
        m2.set_meta(
            wafer_run::META_REQ_RESOURCE,
            "/b/llm/api/providers/abc123/discover-models",
        );
        assert_eq!(path_param(&m2, "id", PROVIDERS_PREFIX), "abc123");

        // Empty when no id provided
        let mut m3 = Message::new("delete:/b/llm/api/providers/");
        m3.set_meta(wafer_run::META_REQ_RESOURCE, "/b/llm/api/providers/");
        assert_eq!(path_param(&m3, "id", PROVIDERS_PREFIX), "");

        // `msg.var("id")` takes precedence
        let mut m4 = Message::new("update:/b/llm/api/providers/from-path");
        m4.set_meta(
            wafer_run::META_REQ_RESOURCE,
            "/b/llm/api/providers/from-path",
        );
        m4.set_meta(
            format!("{}id", wafer_run::META_REQ_PARAM_PREFIX),
            "from-var",
        );
        assert_eq!(path_param(&m4, "id", PROVIDERS_PREFIX), "from-var");
    }

    #[test]
    fn provider_to_json_shape() {
        let cfg = ProviderConfig::new(
            "openai-main",
            ProviderProtocol::OpenAi,
            "https://api.openai.com/v1",
        )
        .with_key_var("IMPRESSPRESS__LLM__OPENAI_KEY")
        .with_models(vec!["gpt-4o".into()]);
        let v = provider_to_json("row-1", &cfg);
        assert_eq!(v["id"], "row-1");
        assert_eq!(v["name"], "openai-main");
        assert_eq!(v["protocol"], "open_ai");
        assert_eq!(v["endpoint"], "https://api.openai.com/v1");
        assert_eq!(v["key_var"], "IMPRESSPRESS__LLM__OPENAI_KEY");
        assert_eq!(v["models"], serde_json::json!(["gpt-4o"]));
        assert_eq!(v["enabled"], true);
        assert!(
            v.get("api_key").is_none(),
            "api_key must never appear in API output"
        );
    }

    // -----------------------------------------------------------------
    // reload_provider_service — key_var resolution
    // -----------------------------------------------------------------

    /// End-to-end reload over a real in-memory DB + config block:
    /// a row whose `key_var` resolves gets its `api_key` populated, a row
    /// without `key_var` stays unauthenticated, and an unresolvable
    /// `key_var` degrades to no key (warn) instead of failing the reload.
    #[tokio::test]
    async fn reload_provider_service_resolves_key_var_into_api_key() {
        use wafer_core::{
            interfaces::config::service::ConfigService,
            service_blocks::config::{ConfigBlock, EnvConfigService},
        };

        use crate::test_support::TestContext;

        let mut ctx = TestContext::with_admin().await;
        {
            use crate::blocks::llm::migrations;
            let sqlite: Vec<&str> = migrations::SQLITE_MIGRATIONS
                .iter()
                .map(|(_, sql)| *sql)
                .collect();
            crate::migration_helper::apply_migrations(
                &ctx,
                "impresspress/llm",
                &sqlite,
                migrations::POSTGRES_MIGRATIONS,
            )
            .await
            .expect("apply llm migrations");
        }

        let config_svc = Arc::new(EnvConfigService::new());
        config_svc.set("IMPRESSPRESS__LLM__OPENAI_KEY", "sk-resolved");
        ctx.register_block("wafer-run/config", Arc::new(ConfigBlock::new(config_svc)));

        for cfg in [
            ProviderConfig::new(
                "with-key-var",
                ProviderProtocol::OpenAi,
                "https://api.openai.com/v1",
            )
            .with_key_var("IMPRESSPRESS__LLM__OPENAI_KEY"),
            ProviderConfig::new(
                "no-key-var",
                ProviderProtocol::OpenAiCompatible,
                "http://localhost:11434/v1",
            ),
            ProviderConfig::new(
                "unresolvable-key-var",
                ProviderProtocol::OpenAi,
                "https://api.openai.com/v1",
            )
            .with_key_var("IMPRESSPRESS__LLM__TEST_MISSING_KEY"),
        ] {
            let mut data = config_to_row(&cfg);
            crate::util::stamp_created(&mut data);
            db::create(&ctx, PROVIDERS_TABLE, data)
                .await
                .expect("create provider row");
        }

        let svc = crate::blocks::llm::providers::ProviderLlmService::new();
        reload_provider_service(&ctx, &svc)
            .await
            .expect("reload succeeds");

        let by_name = |name: &str| {
            svc.providers_snapshot()
                .into_iter()
                .find(|c| c.name == name)
                .unwrap_or_else(|| panic!("provider '{name}' missing from snapshot"))
        };
        assert_eq!(
            by_name("with-key-var").api_key.as_deref(),
            Some("sk-resolved"),
            "key_var must resolve into api_key at reload"
        );
        assert_eq!(by_name("no-key-var").api_key, None);
        assert_eq!(
            by_name("unresolvable-key-var").api_key,
            None,
            "unresolvable key_var degrades to no key, not a reload failure"
        );
    }
}
