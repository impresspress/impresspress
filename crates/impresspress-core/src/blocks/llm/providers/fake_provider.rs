//! A fake LLM provider on loopback, one per wire protocol, plus the
//! `wafer-run/llm` service block that routes to it.
//!
//! Shared by the chat-route tests and the vector block's contextual-retrieval
//! test. Both need a chat request to travel through the real
//! [`ProviderLlmService`] and the real encoder, because the encoders are where
//! the output-token budget becomes bytes:
//!
//! * Anthropic refuses a request that carries no budget at all
//!   ([`MissingMaxTokens`](super::anthropic::EncodeError::MissingMaxTokens)),
//!   before a byte reaches the provider.
//! * The two OpenAI-shaped protocols spell the budget differently
//!   (`max_completion_tokens` vs `max_tokens`), and sending the wrong one is a
//!   400 from the server or a silently uncapped reply.
//!
//! A stubbed `wafer-run/llm` block encodes nothing, so it can see neither —
//! which is why every chat test in this repo passed while every Anthropic chat
//! in production failed.

use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use wafer_run::Block;

use super::{
    config::{ProviderConfig, ProviderProtocol},
    ProviderLlmService,
};
use crate::{blocks::llm::provider_admin::ProviderAdmin, llm_wire::openai::MaxTokensField};

/// A one-provider deployment: an HTTP server on loopback that answers every
/// chat request with a scripted reply in its protocol's streaming format, and
/// a record of the request bodies it was sent.
pub(crate) struct FakeProvider {
    endpoint: String,
    protocol: ProviderProtocol,
    max_tokens_field: Option<MaxTokensField>,
    requests: Arc<Mutex<Vec<serde_json::Value>>>,
}

impl FakeProvider {
    /// An Anthropic Messages-API provider answering `text`.
    pub(crate) async fn anthropic(text: &'static str) -> Self {
        Self::start(ProviderProtocol::Anthropic, text).await
    }

    /// OpenAI's own API — the protocol whose budget field is
    /// `max_completion_tokens`.
    pub(crate) async fn openai(text: &'static str) -> Self {
        Self::start(ProviderProtocol::OpenAi, text).await
    }

    /// An OpenAI-*compatible* server (Ollama, vLLM, a hosted gateway): same
    /// streaming format, but `max_tokens` is the budget field it knows.
    pub(crate) async fn openai_compatible(text: &'static str) -> Self {
        Self::start(ProviderProtocol::OpenAiCompatible, text).await
    }

    /// Configure this provider with an explicit `max_tokens_field`, the way an
    /// operator declares one on the admin form for a server whose budget
    /// spelling departs from its protocol's — an Azure OpenAI reasoning
    /// deployment, which is configured as `open_ai_compatible` but accepts
    /// only `max_completion_tokens`.
    pub(crate) fn requiring_max_tokens_field(mut self, field: MaxTokensField) -> Self {
        self.max_tokens_field = Some(field);
        self
    }

    /// Bind a loopback port and serve `text` until the test ends.
    ///
    /// The listener takes `127.0.0.1` rather than the name: on a dual-stack
    /// host `localhost` can resolve to `::1`, and binding one family while the
    /// client reaches for the other is a hang that looks like a product bug.
    /// The *URL* still says `localhost`, because that is the only plain-HTTP
    /// host `crate::util::validate_url_value` allows — the affordance for
    /// self-hosted models, which the real `chat_stream` re-checks before
    /// dispatching.
    async fn start(protocol: ProviderProtocol, text: &'static str) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind a loopback port");
        let port = listener.local_addr().expect("listener address").port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&requests);
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let recorded = Arc::clone(&recorded);
                tokio::spawn(async move {
                    // Read the whole request. Stopping early leaves unread
                    // bytes in the socket, and closing on those sends an RST
                    // that discards the response we are about to write.
                    let mut req = Vec::new();
                    let mut buf = [0u8; 1024];
                    while !ends_request(&req) {
                        match sock.read(&mut buf).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => req.extend_from_slice(&buf[..n]),
                        }
                    }
                    if let Some(body) = request_body(&req) {
                        recorded.lock().expect("recorded requests lock").push(body);
                    }
                    let _ = sock
                        .write_all(
                            format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n{}",
                                stream_body(protocol, text)
                            )
                            .as_bytes(),
                        )
                        .await;
                    let _ = sock.flush().await;
                    // Half-close so the client sees a clean end of body.
                    let _ = sock.shutdown().await;
                });
            }
        });
        Self {
            endpoint: format!("http://localhost:{port}"),
            protocol,
            max_tokens_field: None,
            requests,
        }
    }

    /// Provider name this fake is configured under — the `backend_id` a chat
    /// request routes on.
    pub(crate) fn backend_id(&self) -> &'static str {
        match self.protocol {
            ProviderProtocol::Anthropic => "anthropic-main",
            ProviderProtocol::OpenAi => "openai-main",
            ProviderProtocol::OpenAiCompatible => "compatible-main",
        }
    }

    /// Model the fake answers for. Never inspected by the fake; it is what the
    /// encoder puts in the request body. Named after a real model of the
    /// protocol, and for OpenAI after a *reasoning* model — the class that
    /// refuses the deprecated budget field.
    pub(crate) fn model(&self) -> &'static str {
        match self.protocol {
            ProviderProtocol::Anthropic => "claude-sonnet-4-5",
            ProviderProtocol::OpenAi => "o3",
            ProviderProtocol::OpenAiCompatible => "llama3",
        }
    }

    /// Every request body the provider was sent, as the JSON the encoder
    /// produced. This is where the budget is observable as a *field and a
    /// value* rather than as the absence of an error.
    pub(crate) fn requests(&self) -> Vec<serde_json::Value> {
        self.requests
            .lock()
            .expect("recorded requests lock")
            .clone()
    }

    /// The `wafer-run/llm` service block a test registers: the production
    /// service-block wrapper around a real [`ProviderLlmService`] holding one
    /// enabled provider pointed at this fake.
    pub(crate) fn llm_service_block(&self) -> Arc<dyn Block> {
        let svc = ProviderLlmService::try_new().expect("build the provider service");
        let mut cfg = ProviderConfig::new(self.backend_id(), self.protocol, &self.endpoint)
            .with_api_key("sk-test")
            .with_models(vec![self.model().to_string()]);
        cfg.max_tokens_field = self.max_tokens_field;
        svc.configure(vec![cfg])
            .expect("the provider router accepts configuration");
        Arc::new(wafer_core::service_blocks::llm::LlmBlock::new(Arc::new(
            svc,
        )))
    }
}

/// One scripted reply in `protocol`'s streaming format: the text, then the
/// model's own reason for stopping (which is what makes the answer whole for
/// `ProviderLlmService`), then the transport terminator.
fn stream_body(protocol: ProviderProtocol, text: &str) -> String {
    match protocol {
        ProviderProtocol::Anthropic => format!(
            "event: content_block_delta\n\
             data: {}\n\n\
             event: message_delta\n\
             data: {{\"delta\":{{\"stop_reason\":\"end_turn\"}},\"usage\":{{\"output_tokens\":3}}}}\n\n\
             event: message_stop\n\
             data: {{}}\n\n",
            serde_json::json!({
                "index": 0,
                "delta": { "type": "text_delta", "text": text },
            })
        ),
        ProviderProtocol::OpenAi | ProviderProtocol::OpenAiCompatible => format!(
            "data: {}\n\n\
             data: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"stop\"}}]}}\n\n\
             data: [DONE]\n\n",
            serde_json::json!({
                "choices": [{ "delta": { "content": text } }],
            })
        ),
    }
}

/// Whether `req` contains a complete HTTP request: headers, plus the body
/// named by its `content-length` (the chat POST always has one).
fn ends_request(req: &[u8]) -> bool {
    body_span(req).is_some_and(|(start, len)| req.len() >= start + len)
}

/// The request body, decoded as JSON.
fn request_body(req: &[u8]) -> Option<serde_json::Value> {
    let (start, len) = body_span(req)?;
    serde_json::from_slice(req.get(start..start + len)?).ok()
}

/// `(body offset, content-length)` once the headers have arrived.
fn body_span(req: &[u8]) -> Option<(usize, usize)> {
    let text = String::from_utf8_lossy(req);
    let head_end = text.find("\r\n\r\n")?;
    let len: usize = text[..head_end]
        .lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case("content-length")
                .then(|| v.trim().parse().ok())?
        })
        .unwrap_or(0);
    Some((head_end + 4, len))
}
