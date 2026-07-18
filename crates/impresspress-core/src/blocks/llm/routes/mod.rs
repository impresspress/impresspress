//! HTTP route handlers for the `impresspress/llm` feature block.
//!
//! Both endpoints (`/b/llm/api/chat`, `/b/llm/api/chat/stream`) route through
//! the `wafer-run/llm` service block via `ctx.call_block`. They persist user
//! and assistant messages via `impresspress/messages`, resolve the provider +
//! model via [`LlmBlock::resolve_provider`](super::LlmBlock::resolve_provider),
//! and translate the `ChatChunk` stream returned by the service into either a
//! buffered JSON response or a Server-Sent Events stream.
//!
//! Split by domain responsibility:
//! - [`chat`] — chat request parsing/persistence/dispatch: the buffered
//!   `/api/chat` handler and the (thin) `/api/chat/stream` entry point.
//! - [`streaming`] — the SSE framing helpers shared by the chat-stream
//!   responder and the model-load responder, so the wire format can't drift
//!   between them.
//! - [`providers`] — provider CRUD (admin-only), the in-memory provider
//!   router reload, and per-provider model discovery.
//! - [`models`] — the aggregated model listing/status/load/unload endpoints
//!   backed by the `wafer-run/llm` service block.

mod chat;
mod models;
mod providers;
mod streaming;

#[cfg(test)]
mod test_support;

pub(super) use chat::{handle_chat, handle_chat_stream};
pub(super) use models::{list_models, load_model, model_status, unload_model};
pub(in crate::blocks::llm) use providers::reload_provider_service;
pub(super) use providers::{
    create_provider, delete_provider, discover_models, list_providers, update_provider,
};
