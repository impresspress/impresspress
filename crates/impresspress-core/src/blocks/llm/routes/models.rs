//! Models endpoints (aggregated via wafer-run/llm service block).
//!
//! The service block aggregates `list_models` across every registered
//! `LlmService` impl in its router. `status` / `load` / `unload` are
//! per-(backend_id, model_id) ops forwarded verbatim. These handlers only
//! marshal HTTP ⇄ service-block JSON — no business logic here.

use wafer_core::clients::llm::{
    self as llm_client, LoadModelRequest, StatusRequest, UnloadModelRequest,
};
use wafer_run::{context::Context, Message, OutputStream};

use super::streaming::sse_json_response;
use crate::{
    blocks::llm::LlmBlock,
    http::{err_bad_request, err_internal, ok_json},
};

/// Extract `(backend_id, model_id)` from
/// `/b/llm/api/models/{backend_id}/{model_id}[/suffix]`.
///
/// Prefers the router-supplied path variables when available, falling back
/// to splitting `msg.path()` on `/`. Both ids may contain
/// backend-specific characters (`-`, `_`, `.`, but not `/`), so a single
/// split-on-`/` round yields the right segments.
fn extract_model_path(msg: &Message) -> (String, String) {
    let backend_var = msg.var("backend_id");
    let model_var = msg.var("model_id");
    if !backend_var.is_empty() && !model_var.is_empty() {
        return (backend_var.to_string(), model_var.to_string());
    }
    let path = msg.path();
    let suffix = path.strip_prefix("/b/llm/api/models/").unwrap_or("");
    let mut parts = suffix.splitn(3, '/');
    let backend = parts.next().unwrap_or("").to_string();
    let model = parts.next().unwrap_or("").to_string();
    (backend, model)
}

/// `GET /b/llm/api/models` — aggregated list across all registered LLM
/// backends. Authenticated (any logged-in user).
pub(in crate::blocks::llm) async fn list_models(
    _block: &LlmBlock,
    ctx: &dyn Context,
    _msg: &Message,
) -> OutputStream {
    match llm_client::list_models(ctx).await {
        Ok(models) => ok_json(&serde_json::json!({ "models": models })),
        Err(e) => err_internal("llm list_models failed", e.message),
    }
}

/// `GET /b/llm/api/models/:backend_id/:model_id/status` — per-(backend, model)
/// status. Authenticated.
pub(in crate::blocks::llm) async fn model_status(
    _block: &LlmBlock,
    ctx: &dyn Context,
    msg: &Message,
) -> OutputStream {
    let (backend_id, model_id) = extract_model_path(msg);
    if backend_id.is_empty() || model_id.is_empty() {
        return err_bad_request("Missing backend_id or model_id");
    }
    let req = StatusRequest {
        backend_id,
        model_id,
    };
    match llm_client::status(ctx, &req).await {
        Ok(status) => ok_json(&serde_json::json!({ "status": status })),
        Err(e) => err_internal("llm status failed", e.message),
    }
}

/// `POST /b/llm/api/models/:backend_id/:model_id/load` — start a model
/// load, streaming `LoadProgress` events as SSE. Admin-only.
pub(in crate::blocks::llm) async fn load_model(
    _block: &LlmBlock,
    ctx: &dyn Context,
    msg: &Message,
) -> OutputStream {
    let (backend_id, model_id) = extract_model_path(msg);
    if backend_id.is_empty() || model_id.is_empty() {
        return err_bad_request("Missing backend_id or model_id");
    }
    let req = LoadModelRequest {
        backend_id,
        model_id,
    };
    let stream = match llm_client::load_model_stream(ctx, &req).await {
        Ok(s) => s,
        Err(e) => return err_internal("llm load_model failed", e.message),
    };

    sse_json_response(stream)
}

/// `POST /b/llm/api/models/:backend_id/:model_id/unload` — buffered unload.
/// Admin-only.
pub(in crate::blocks::llm) async fn unload_model(
    _block: &LlmBlock,
    ctx: &dyn Context,
    msg: &Message,
) -> OutputStream {
    let (backend_id, model_id) = extract_model_path(msg);
    if backend_id.is_empty() || model_id.is_empty() {
        return err_bad_request("Missing backend_id or model_id");
    }
    let req = UnloadModelRequest {
        backend_id,
        model_id,
    };
    match llm_client::unload_model(ctx, &req).await {
        Ok(()) => ok_json(&serde_json::json!({ "unloaded": true })),
        Err(e) => err_internal("llm unload_model failed", e.message),
    }
}

#[cfg(test)]
mod tests {
    use wafer_run::{streams::output::TerminalNotResponse, ErrorCode};

    use super::*;
    use crate::blocks::llm::routes::test_support::{admin_msg, stub_block, user_msg, PanicCtx};

    #[tokio::test]
    async fn load_model_requires_path_vars() {
        let block = stub_block();
        let ctx = PanicCtx;
        // Admin but missing segments after the prefix.
        let msg = admin_msg("create", "/b/llm/api/models//load");

        let out = load_model(&block, &ctx, &msg).await;
        match out.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::InvalidArgument);
                assert!(
                    e.message.contains("backend_id") || e.message.contains("model_id"),
                    "got: {}",
                    e.message
                );
            }
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unload_model_requires_path_vars() {
        let block = stub_block();
        let ctx = PanicCtx;
        let msg = admin_msg("create", "/b/llm/api/models/openai/");

        let out = unload_model(&block, &ctx, &msg).await;
        match out.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::InvalidArgument);
            }
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn model_status_requires_path_vars() {
        let block = stub_block();
        let ctx = PanicCtx;
        let msg = user_msg("retrieve", "/b/llm/api/models//status");

        let out = model_status(&block, &ctx, &msg).await;
        match out.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::InvalidArgument);
            }
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
    }

    #[test]
    fn extract_model_path_from_suffix() {
        // Straight {backend_id}/{model_id}/status
        let mut m = Message::new("retrieve:/b/llm/api/models/openai/gpt-4o/status");
        m.set_meta(
            wafer_run::META_REQ_RESOURCE,
            "/b/llm/api/models/openai/gpt-4o/status",
        );
        assert_eq!(
            extract_model_path(&m),
            ("openai".to_string(), "gpt-4o".to_string())
        );

        // Load sub-resource with a model id containing dots/dashes
        let mut m2 = Message::new("create:/b/llm/api/models/webllm/llama-3.1-8b/load");
        m2.set_meta(
            wafer_run::META_REQ_RESOURCE,
            "/b/llm/api/models/webllm/llama-3.1-8b/load",
        );
        assert_eq!(
            extract_model_path(&m2),
            ("webllm".to_string(), "llama-3.1-8b".to_string())
        );

        // Missing model_id
        let mut m3 = Message::new("create:/b/llm/api/models/openai/");
        m3.set_meta(wafer_run::META_REQ_RESOURCE, "/b/llm/api/models/openai/");
        let (b, m_id) = extract_model_path(&m3);
        assert_eq!(b, "openai");
        assert_eq!(m_id, "");

        // Router-provided path variables take precedence over the path string.
        let mut m4 = Message::new("create:/b/llm/api/models/from-path/ignored/load");
        m4.set_meta(
            wafer_run::META_REQ_RESOURCE,
            "/b/llm/api/models/from-path/ignored/load",
        );
        m4.set_meta(
            format!("{}backend_id", wafer_run::META_REQ_PARAM_PREFIX),
            "from-var",
        );
        m4.set_meta(
            format!("{}model_id", wafer_run::META_REQ_PARAM_PREFIX),
            "var-model",
        );
        assert_eq!(
            extract_model_path(&m4),
            ("from-var".to_string(), "var-model".to_string())
        );
    }
}
