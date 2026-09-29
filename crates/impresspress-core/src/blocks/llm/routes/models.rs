//! Models endpoints (aggregated via wafer-run/llm service block).
//!
//! The service block aggregates `list_models` across every registered
//! `LlmService` impl in its router. `status` / `load` / `unload` are
//! per-(backend_id, model_id) ops forwarded verbatim. These handlers only
//! marshal HTTP ⇄ service-block JSON — no business logic here.

use wafer_core::clients::llm::{
    self as llm_client, LoadModelRequest, StatusRequest, UnloadModelRequest,
};
use wafer_run::{context::Context, ErrorCode, Message, OutputStream, WaferError};

use super::streaming::sse_json_response;
use crate::{
    blocks::{
        crud,
        llm::{
            contracts::{
                ModelInfoView, ModelListResponse, ModelStatusResponse, ModelStatusView,
                ModelUnloadResponse,
            },
            LlmBlock,
        },
    },
    http::{err_bad_request, ok_json},
};

/// `(backend_id, model_id)` as bound by the block's route table for
/// `/b/llm/api/models/{backend_id}/{model_id}/...`. Either is empty when the
/// request matched no row.
fn extract_model_path(msg: &Message) -> (String, String) {
    (
        msg.var("backend_id").to_string(),
        msg.var("model_id").to_string(),
    )
}

/// `GET /b/llm/api/models` — aggregated list across all registered LLM
/// backends. Authenticated (any logged-in user).
pub(in crate::blocks::llm) async fn list_models(
    _block: &LlmBlock,
    ctx: &dyn Context,
    _msg: &Message,
) -> OutputStream {
    match llm_client::list_models(ctx).await {
        Ok(models) => ok_json(&ModelListResponse {
            models: models.into_iter().map(ModelInfoView::from).collect(),
        }),
        Err(e) => crud::db_error_internal(e, "llm list_models failed"),
    }
}

/// Answer a service refusal with the classification it already carries.
///
/// `wafer-core`'s llm handler maps each `LlmError` onto a code before it
/// crosses the block boundary: `InvalidRequest` → `InvalidArgument` (what
/// `providers::service::status` answers for an unknown backend),
/// `ModelNotFound` → `NotFound`, `NotSupported` → `Unimplemented`. All three
/// handlers below used to discard that with a blanket `err_internal`, so a
/// caller naming a backend that does not exist was told the site had failed —
/// what the 2026-09-10 live run recorded as 500s on non-existent ids.
///
/// Everything except the caller-actionable `InvalidArgument` goes through
/// `crud::db_error`, which is the only place in `blocks/` allowed to map an
/// error by hand (`tests/error_door.rs` enforces that). It is also what makes
/// the rest correct rather than merely classified:
///
/// - a `NotFound` answers with OUR label, so the runtime's
///   `"block not found: wafer-run/llm"` — what a deployment missing the llm
///   service block produces — is never echoed to a caller;
/// - a `PermissionDenied` is sanitized to `"Access denied"` and logged, rather
///   than publishing which `ResourceGrant` and table were refused;
/// - a `BackendError`, a network fault, or a provider-credential failure stays
///   an internal error: sanitized, logged, with a correlation id. A wrong
///   provider API key is an operator's problem, and answering the caller 401
///   would read as their own session expiring.
fn llm_service_error(context: &str, error: WaferError) -> OutputStream {
    match error.code {
        ErrorCode::InvalidArgument | ErrorCode::FailedPrecondition => {
            err_bad_request(&error.message)
        }
        _ => crud::db_error(error, "Model not found", context),
    }
}

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
        Ok(status) => ok_json(&ModelStatusResponse {
            status: ModelStatusView::from(status),
        }),
        Err(e) => llm_service_error("llm status failed", e),
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
        Err(e) => return llm_service_error("llm load_model failed", e),
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
        Ok(()) => ok_json(&ModelUnloadResponse { unloaded: true }),
        Err(e) => llm_service_error("llm unload_model failed", e),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use wafer_core::clients::llm::{ModelCapabilities, ModelInfo, ModelStatus};
    use wafer_run::{streams::output::TerminalNotResponse, ErrorCode};

    use super::*;
    use crate::{
        blocks::llm::routes::test_support::{
            admin_msg, routed, stub_block, user_msg, PanicCtx, StubLlmServiceBlock,
        },
        test_support::{output_is_error, output_json, TestContext},
    };

    async fn ctx_with(stub: StubLlmServiceBlock) -> TestContext {
        let mut ctx = TestContext::new()
            .await
            .running_as(crate::blocks::llm::LlmBlock::BLOCK_NAME);
        ctx.register_block("wafer-run/llm", Arc::new(stub));
        ctx
    }

    /// The list is the service block's `ModelInfo` rows projected through the
    /// block's own view, field for field — nullable capability limits
    /// included, which the wire carries as explicit `null`s.
    #[tokio::test]
    async fn list_models_publishes_the_model_view() {
        let ctx = ctx_with(StubLlmServiceBlock {
            models: vec![
                ModelInfo::new("openai-main", "gpt-4o", "GPT-4o").with_capabilities(
                    ModelCapabilities {
                        streaming: true,
                        tools: true,
                        vision: false,
                        json_mode: true,
                        max_context_tokens: Some(128_000),
                        max_output_tokens: None,
                    },
                ),
            ],
            ..Default::default()
        })
        .await;

        let body = output_json(
            list_models(
                &stub_block(),
                &ctx,
                &user_msg("retrieve", "/b/llm/api/models"),
            )
            .await,
        )
        .await;

        assert_eq!(
            body,
            serde_json::json!({
                "models": [{
                    "backend_id": "openai-main",
                    "model_id": "gpt-4o",
                    "display_name": "GPT-4o",
                    "capabilities": {
                        "streaming": true,
                        "tools": true,
                        "vision": false,
                        "json_mode": true,
                        "max_context_tokens": 128000,
                        "max_output_tokens": null,
                    },
                }],
            })
        );
    }

    /// `progress` is present only while loading; the error state carries its
    /// message under the variant name. Both are what the schema promises.
    #[tokio::test]
    async fn model_status_publishes_the_status_view() {
        for (status, expected) in [
            (
                ModelStatus::ready(),
                serde_json::json!({ "status": { "state": "Ready" } }),
            ),
            (
                ModelStatus::loading(0.5),
                serde_json::json!({ "status": { "state": "Loading", "progress": 0.5 } }),
            ),
            (
                ModelStatus::error("provider disabled"),
                serde_json::json!({
                    "status": { "state": { "Error": { "message": "provider disabled" } } }
                }),
            ),
        ] {
            let ctx = ctx_with(StubLlmServiceBlock {
                status,
                ..Default::default()
            })
            .await;

            let body = output_json(
                model_status(
                    &stub_block(),
                    &ctx,
                    &routed(user_msg(
                        "retrieve",
                        "/b/llm/api/models/openai-main/gpt-4o/status",
                    )),
                )
                .await,
            )
            .await;

            assert_eq!(body, expected);
        }
    }

    #[tokio::test]
    async fn unload_model_acknowledges() {
        let ctx = ctx_with(StubLlmServiceBlock::default()).await;

        let body = output_json(
            unload_model(
                &stub_block(),
                &ctx,
                &routed(admin_msg(
                    "create",
                    "/b/llm/api/models/openai-main/gpt-4o/unload",
                )),
            )
            .await,
        )
        .await;

        assert_eq!(body, serde_json::json!({ "unloaded": true }));
    }

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

    /// Drive the load route against the stub service and return its SSE body,
    /// or the terminal the stream ended on instead of completing.
    async fn load_body(stub: StubLlmServiceBlock) -> Result<String, TerminalNotResponse> {
        let ctx = ctx_with(stub).await;
        let out = load_model(
            &stub_block(),
            &ctx,
            &routed(admin_msg(
                "create",
                "/b/llm/api/models/openai-main/gpt-4o/load",
            )),
        )
        .await;
        let buf = out.collect_buffered().await?;
        Ok(String::from_utf8(buf.body).expect("SSE body is utf8"))
    }

    /// A load that finishes is a whole SSE response: `[DONE]`, then a
    /// completed stream. The producer has to end it itself; a stream it lets
    /// go of without a terminal is an error to every consumer, which a
    /// buffering transport answers with a 500.
    #[tokio::test]
    async fn a_finished_model_load_ends_in_done_and_completes() {
        let body = load_body(StubLlmServiceBlock::default())
            .await
            .expect("a finished load completes");
        assert_eq!(body, "data: [DONE]\n\n");
    }

    /// A load the service refuses ends in the in-band error frame, and the
    /// stream still completes: the frame is the failure signal, and the body
    /// that carries it is whole.
    #[tokio::test]
    async fn a_refused_model_load_ends_in_an_error_frame_and_completes() {
        let body = load_body(StubLlmServiceBlock {
            error: Some((ErrorCode::NotFound, "no such model".to_string())),
            ..Default::default()
        })
        .await
        .expect("an SSE body ending in the error frame completes");
        assert_eq!(body, "event: error\ndata: {}\n\n");
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

    /// Model handlers read the ids the table bound, nothing else.
    #[test]
    fn model_path_is_bound_by_the_table() {
        let m = routed(user_msg(
            "retrieve",
            "/b/llm/api/models/openai/gpt-4o/status",
        ));
        assert_eq!(
            extract_model_path(&m),
            ("openai".to_string(), "gpt-4o".to_string())
        );

        // A model id with dots and dashes is one segment.
        let m2 = routed(admin_msg(
            "create",
            "/b/llm/api/models/webllm/llama-3.1-8b/load",
        ));
        assert_eq!(
            extract_model_path(&m2),
            ("webllm".to_string(), "llama-3.1-8b".to_string())
        );

        // Missing model id: no row matches, nothing is bound, the handler
        // answers InvalidArgument (see `unload_model_requires_path_vars`).
        let mut m3 = admin_msg("create", "/b/llm/api/models/openai/");
        assert!(crate::endpoint_match::dispatch(&mut m3, crate::blocks::llm::ROUTES).is_none());
        assert_eq!(extract_model_path(&m3), (String::new(), String::new()));
    }

    /// A classified service refusal keeps its classification.
    ///
    /// `wafer-core`'s llm handler maps every `LlmError` onto a code before it
    /// crosses the block boundary — `InvalidRequest` → `InvalidArgument`
    /// (which is what `providers::service::status` answers for an unknown
    /// backend), `ModelNotFound` → `NotFound`, `NotSupported` →
    /// `Unimplemented`. This handler then discarded all of it with a blanket
    /// `err_internal`, so a caller naming a backend that does not exist was
    /// told the site had failed. The 2026-09-10 live run recorded these as
    /// 500s on a non-existent id; the cause is the flattening, not a missing
    /// 404.
    #[tokio::test]
    async fn model_status_keeps_a_classified_refusal() {
        let ctx = ctx_with(StubLlmServiceBlock {
            error: Some((ErrorCode::InvalidArgument, "unknown backend: nope".into())),
            ..Default::default()
        })
        .await;

        let out = model_status(
            &stub_block(),
            &ctx,
            &routed(user_msg("retrieve", "/b/llm/api/models/nope/gpt-4o/status")),
        )
        .await;

        assert!(
            output_is_error(out, "InvalidArgument").await,
            "an unknown backend must keep the InvalidArgument the service assigned"
        );
    }

    /// A `NotFound` from the service reaches the caller as `NotFound`, on the
    /// unload surface too.
    #[tokio::test]
    async fn unload_model_keeps_a_classified_refusal() {
        let ctx = ctx_with(StubLlmServiceBlock {
            error: Some((ErrorCode::NotFound, "model not found: ghost".into())),
            ..Default::default()
        })
        .await;

        let out = unload_model(
            &stub_block(),
            &ctx,
            // `POST .../{backend_id}/{model_id}/unload` — "create" is this
            // codebase's POST action, and the route is admin-level.
            &routed(admin_msg(
                "create",
                "/b/llm/api/models/openai-main/ghost/unload",
            )),
        )
        .await;

        assert!(
            output_is_error(out, "NotFound").await,
            "a missing model must answer NotFound, not an internal error"
        );
    }

    /// An unclassified backend failure is still an internal error.
    ///
    /// The mapping must not turn every refusal into a client error: a
    /// `BackendError` (the service's own `Internal`) is a real fault and has
    /// to stay a 500, sanitized, the way `err_internal` reports it.
    #[tokio::test]
    async fn a_backend_failure_is_still_internal() {
        let ctx = ctx_with(StubLlmServiceBlock {
            error: Some((ErrorCode::Internal, "upstream exploded".into())),
            ..Default::default()
        })
        .await;

        let out = model_status(
            &stub_block(),
            &ctx,
            &routed(user_msg(
                "retrieve",
                "/b/llm/api/models/openai-main/gpt-4o/status",
            )),
        )
        .await;

        assert!(
            output_is_error(out, "Internal").await,
            "a backend fault must remain an internal error"
        );
    }
}
