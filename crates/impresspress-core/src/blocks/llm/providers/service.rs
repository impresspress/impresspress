//! `ProviderLlmService` — concrete `LlmService` impl for HTTP-based LLM
//! providers. Native-only (gated on `feature = "llm"`): uses `reqwest` +
//! `tokio` for SSE streaming, neither of which compiles on
//! `wasm32-unknown-unknown`. Browser targets use `BrowserLlmService` from
//! `impresspress-web` instead, registered on the same `MultiBackendLlmService`
//! router.

use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use async_trait::async_trait;
use futures::{stream::BoxStream, StreamExt};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;
use wafer_core::interfaces::llm::service::{
    ChatChunk, ChatRequest, LlmError, LlmService, ModelInfo, ModelStatus,
};

use super::{
    anthropic,
    config::{ProviderConfig, ProviderProtocol},
    openai, openai_compatible, sse,
};
use crate::blocks::llm::provider_admin::ProviderAdmin;

/// Unwrap a `RwLock` read/write guard, recovering from poisoning rather than
/// panicking. A poisoned lock means a prior writer panicked while holding it;
/// the data may be partially-updated but is still readable and safe to mutate
/// after re-locking — so we log and continue rather than bringing the chat /
/// model listing / status endpoints down for every subsequent request.
macro_rules! recover_lock {
    ($result:expr, $what:expr) => {
        match $result {
            Ok(g) => g,
            Err(p) => {
                tracing::error!("{} lock poisoned — recovering", $what);
                p.into_inner()
            }
        }
    };
}

pub struct ProviderLlmService {
    inner: Arc<RwLock<Inner>>,
    http: reqwest::Client,
}

struct Inner {
    providers: HashMap<String, ProviderConfig>,
    /// Per-provider cached model lists, populated from configure() and
    /// refreshed by discover_models(). The aggregated list_models() view
    /// is built from this on each call — cheap since the cardinality is
    /// small (providers * models-per-provider).
    cached_models: HashMap<String, Vec<ModelInfo>>,
}

impl ProviderLlmService {
    /// Construct a service with a default `reqwest` client. Returns
    /// `LlmError::BackendError` if the underlying TLS stack fails to
    /// initialize — rare in practice but propagating it lets the host fall
    /// back to a degraded mode rather than aborting the whole process.
    pub fn try_new() -> Result<Self, LlmError> {
        let http = reqwest::Client::builder()
            .build()
            .map_err(|e| LlmError::BackendError(format!("reqwest client build: {e}")))?;
        Ok(Self {
            inner: Arc::new(RwLock::new(Inner {
                providers: HashMap::new(),
                cached_models: HashMap::new(),
            })),
            http,
        })
    }

    /// Infallible legacy constructor — kept so existing `info()` probes and
    /// throwaway-instance call sites still compile. On the rare TLS-init
    /// failure we degrade to a client with no extra options (which itself
    /// cannot fail to build); the next chat call surfaces the underlying
    /// reqwest error as `LlmError::Network`.
    pub fn new() -> Self {
        Self::try_new().unwrap_or_else(|_| Self {
            inner: Arc::new(RwLock::new(Inner {
                providers: HashMap::new(),
                cached_models: HashMap::new(),
            })),
            http: reqwest::Client::new(),
        })
    }

    /// Clone of a single provider's config, keyed by backend_id. The
    /// `api_key` it carries was resolved from `key_var` by the feature
    /// block's reload (see `routes::reload_provider_service`) before
    /// `configure()` — this service never touches the config store itself.
    fn provider_config(&self, backend_id: &str) -> Option<ProviderConfig> {
        let inner = recover_lock!(self.inner.read(), "provider svc read");
        inner.providers.get(backend_id).cloned()
    }
}

impl Default for ProviderLlmService {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ProviderAdmin for ProviderLlmService {
    /// Replace the provider set. Called on feature block startup and again
    /// whenever the admin UI adds / edits / deletes a provider.
    ///
    /// For each provider, seeds `cached_models` from its explicit `models`
    /// list. Callers that want to refresh via `/v1/models` discovery should
    /// subsequently call `discover_models(name)` per provider.
    fn configure(&self, providers: Vec<ProviderConfig>) {
        let mut inner = recover_lock!(self.inner.write(), "provider svc write");
        inner.providers.clear();
        inner.cached_models.clear();
        for p in providers {
            let seeded = p
                .models
                .iter()
                .map(|id| ModelInfo::new(&p.name, id, id))
                .collect();
            let name = p.name.clone();
            inner.cached_models.insert(name.clone(), seeded);
            inner.providers.insert(name, p);
        }
    }

    /// Read-only snapshot of the configured providers. Used by route handlers
    /// that previously hit the DB on every request — the in-memory cache is
    /// the source of truth for the running process.
    fn providers_snapshot(&self) -> Vec<ProviderConfig> {
        let inner = recover_lock!(self.inner.read(), "provider svc read");
        inner.providers.values().cloned().collect()
    }

    /// Query the provider's `/v1/models` endpoint and cache the result.
    /// Errors if the provider isn't configured, the HTTP call fails, or
    /// the response can't be parsed. Only implemented for protocols that
    /// have a well-defined discovery endpoint (OpenAI + compatible);
    /// Anthropic returns `NotSupported`.
    async fn discover_models(&self, provider_name: &str) -> Result<Vec<ModelInfo>, LlmError> {
        let (endpoint, protocol, api_key, models_explicit) = {
            let inner = recover_lock!(self.inner.read(), "provider svc read");
            let p = inner.providers.get(provider_name).ok_or_else(|| {
                LlmError::InvalidRequest(format!("unknown provider: {provider_name}"))
            })?;
            (
                p.endpoint.clone(),
                p.protocol,
                p.api_key.clone(),
                !p.models.is_empty(),
            )
        };

        if models_explicit {
            // Admin set an explicit model list — honour that rather than
            // querying. `list_models()` will still surface them.
            let inner = recover_lock!(self.inner.read(), "provider svc read");
            return Ok(inner
                .cached_models
                .get(provider_name)
                .cloned()
                .unwrap_or_default());
        }

        if !matches!(
            protocol,
            ProviderProtocol::OpenAi | ProviderProtocol::OpenAiCompatible
        ) {
            return Err(LlmError::NotSupported);
        }

        let url = format!("{}/models", endpoint.trim_end_matches('/'));
        let mut req = self.http.get(&url);
        if let Some(key) = api_key {
            req = req.bearer_auth(key);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| LlmError::Network(e.to_string()))?;
        let status = resp.status();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| LlmError::Network(e.to_string()))?;
        if !status.is_success() {
            return Err(LlmError::BackendError(format!(
                "{status}: {}",
                String::from_utf8_lossy(&bytes)
            )));
        }
        let models = openai_compatible::decode_models_response(&bytes, provider_name)
            .map_err(|e| LlmError::BackendError(e.to_string()))?;

        // Cache for aggregated list_models.
        let mut inner = recover_lock!(self.inner.write(), "provider svc write");
        inner
            .cached_models
            .insert(provider_name.to_string(), models.clone());
        Ok(models)
    }
}

#[async_trait]
impl LlmService for ProviderLlmService {
    async fn chat_stream(
        &self,
        req: ChatRequest,
        cancel: CancellationToken,
    ) -> BoxStream<'static, Result<ChatChunk, LlmError>> {
        let Some(cfg) = self.provider_config(&req.backend_id) else {
            let id = req.backend_id;
            return Box::pin(futures::stream::once(async move {
                Err(LlmError::InvalidRequest(format!("unknown backend: {id}")))
            }));
        };
        let http = self.http.clone();

        // Build the provider-specific request up front so any encode error is
        // surfaced synchronously before we start streaming.
        let api_key = cfg.api_key.as_deref();
        let encoded = match cfg.protocol {
            ProviderProtocol::OpenAi => {
                openai::encode_chat_request(&req, &cfg, api_key).map_err(map_openai_encode_error)
            }
            ProviderProtocol::Anthropic => anthropic::encode_chat_request(&req, &cfg, api_key)
                .map_err(map_anthropic_encode_error),
            ProviderProtocol::OpenAiCompatible => {
                openai_compatible::encode_chat_request(&req, &cfg, api_key)
                    .map_err(map_openai_encode_error)
            }
        };
        let (url, headers, body) = match encoded {
            Ok(v) => v,
            Err(e) => return Box::pin(futures::stream::once(async move { Err(e) })),
        };
        let protocol = cfg.protocol;

        let (tx, rx) = mpsc::channel::<Result<ChatChunk, LlmError>>(16);
        tokio::spawn(async move {
            let tx_err = tx;
            let mut builder = http.post(&url);
            for (k, v) in headers {
                builder = builder.header(k, v);
            }
            let fut = builder.body(body).send();
            let resp = tokio::select! {
                r = fut => r,
                _ = cancel.cancelled() => {
                    let _ = tx_err.send(Err(LlmError::Cancelled)).await;
                    return;
                }
            };
            let resp = match resp {
                Ok(r) => r,
                Err(e) => {
                    let _ = tx_err.send(Err(LlmError::Network(e.to_string()))).await;
                    return;
                }
            };
            let status = resp.status();
            if !status.is_success() {
                let bytes = resp.bytes().await.unwrap_or_default();
                let msg = format!("{status}: {}", String::from_utf8_lossy(&bytes));
                let err = match status.as_u16() {
                    401 | 403 => LlmError::Unauthorized,
                    429 => LlmError::RateLimited,
                    _ => LlmError::BackendError(msg),
                };
                let _ = tx_err.send(Err(err)).await;
                return;
            }

            let mut body_stream = resp.bytes_stream();
            let mut decoder = Decoder::for_protocol(protocol);
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        let _ = tx_err.send(Err(LlmError::Cancelled)).await;
                        return;
                    }
                    next = body_stream.next() => match next {
                        Some(Ok(bytes)) => {
                            let batch = decoder.push(&bytes);
                            for chunk in batch.chunks {
                                if tx_err.send(Ok(chunk)).await.is_err() { return; }
                            }
                            if batch.done { return; }
                        }
                        Some(Err(e)) => {
                            let _ = tx_err.send(Err(LlmError::Network(e.to_string()))).await;
                            return;
                        }
                        None => return,
                    }
                }
            }
        });
        Box::pin(ReceiverStream::new(rx))
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, LlmError> {
        let inner = recover_lock!(self.inner.read(), "provider svc read");
        let mut all = Vec::new();
        for (name, cfg) in &inner.providers {
            if !cfg.enabled {
                continue;
            }
            if let Some(models) = inner.cached_models.get(name) {
                all.extend(models.iter().cloned());
            }
        }
        Ok(all)
    }

    async fn status(&self, backend_id: &str, _model_id: &str) -> Result<ModelStatus, LlmError> {
        let inner = recover_lock!(self.inner.read(), "provider svc read");
        let cfg = inner
            .providers
            .get(backend_id)
            .ok_or_else(|| LlmError::InvalidRequest(format!("unknown backend: {backend_id}")))?;
        if !cfg.enabled {
            return Ok(ModelStatus::error("provider disabled"));
        }
        // For remote HTTP providers, "reachable" is the best signal we have
        // without per-request round-tripping. Return Ready; a real
        // reachability check happens on first chat_stream call (errors surface
        // there).
        Ok(ModelStatus::ready())
    }

    fn claims_backend(&self, backend_id: &str) -> bool {
        let inner = recover_lock!(self.inner.read(), "provider svc read");
        inner.providers.contains_key(backend_id)
    }
}

/// Protocol-selected SSE chunk decoder. Both provider decoders share the
/// same `push(&[u8]) -> DecodeBatch` interface; this enum picks the variant
/// from the wire protocol once, so `chat_stream` keeps exactly one decode
/// loop. Adding a fourth protocol is a one-arm change here instead of a
/// copied ~25-line loop.
enum Decoder {
    OpenAi(openai::OpenAiSseDecoder),
    Anthropic(anthropic::AnthropicSseDecoder),
}

impl Decoder {
    fn for_protocol(protocol: ProviderProtocol) -> Self {
        match protocol {
            ProviderProtocol::OpenAi | ProviderProtocol::OpenAiCompatible => {
                Self::OpenAi(openai::OpenAiSseDecoder::new())
            }
            ProviderProtocol::Anthropic => Self::Anthropic(anthropic::AnthropicSseDecoder::new()),
        }
    }

    fn push(&mut self, bytes: &[u8]) -> sse::DecodeBatch {
        match self {
            Self::OpenAi(d) => d.push(bytes),
            Self::Anthropic(d) => d.push(bytes),
        }
    }
}

fn map_openai_encode_error(e: openai::EncodeError) -> LlmError {
    match e {
        openai::EncodeError::MissingApiKey => LlmError::Unauthorized,
        openai::EncodeError::Serialize(m) => LlmError::InvalidRequest(m),
    }
}

fn map_anthropic_encode_error(e: anthropic::EncodeError) -> LlmError {
    match e {
        anthropic::EncodeError::MissingApiKey => LlmError::Unauthorized,
        anthropic::EncodeError::MissingMaxTokens => {
            LlmError::InvalidRequest("max_tokens required for Anthropic".into())
        }
        anthropic::EncodeError::Serialize(m) => LlmError::InvalidRequest(m),
    }
}

#[cfg(test)]
mod tests {
    use wafer_core::interfaces::llm::service::ModelState;

    use super::*;

    fn openai_cfg() -> ProviderConfig {
        ProviderConfig::new(
            "openai-main",
            ProviderProtocol::OpenAi,
            "https://api.openai.com/v1",
        )
        .with_api_key("sk-test")
        .with_models(vec!["gpt-4o-mini".into(), "gpt-4o".into()])
    }

    fn local_cfg() -> ProviderConfig {
        ProviderConfig::new(
            "local",
            ProviderProtocol::OpenAiCompatible,
            "http://localhost:11434/v1",
        )
        .with_models(vec!["llama3".into()])
    }

    #[tokio::test]
    async fn configure_populates_cached_models() {
        let svc = ProviderLlmService::new();
        svc.configure(vec![openai_cfg(), local_cfg()]);

        let models = svc.list_models().await.unwrap();
        assert_eq!(models.len(), 3, "2 openai + 1 local");
        assert!(models.iter().any(|m| m.model_id == "gpt-4o"));
        assert!(models.iter().any(|m| m.model_id == "llama3"));
    }

    #[tokio::test]
    async fn disabled_providers_excluded_from_list_models() {
        let mut cfg = openai_cfg();
        cfg.enabled = false;
        let svc = ProviderLlmService::new();
        svc.configure(vec![cfg, local_cfg()]);

        let models = svc.list_models().await.unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].backend_id, "local");
    }

    #[tokio::test]
    async fn claims_backend_matches_configured_names() {
        let svc = ProviderLlmService::new();
        svc.configure(vec![openai_cfg()]);
        assert!(svc.claims_backend("openai-main"));
        assert!(!svc.claims_backend("local"));
    }

    #[tokio::test]
    async fn status_ready_for_enabled_provider() {
        let svc = ProviderLlmService::new();
        svc.configure(vec![openai_cfg()]);
        let s = svc.status("openai-main", "gpt-4o").await.unwrap();
        assert_eq!(s.state, ModelState::Ready);
    }

    #[tokio::test]
    async fn status_error_for_disabled_provider() {
        let mut cfg = openai_cfg();
        cfg.enabled = false;
        let svc = ProviderLlmService::new();
        svc.configure(vec![cfg]);
        let s = svc.status("openai-main", "gpt-4o").await.unwrap();
        assert!(matches!(s.state, ModelState::Error { .. }));
    }

    #[tokio::test]
    async fn status_invalid_request_for_unknown_backend() {
        let svc = ProviderLlmService::new();
        assert!(matches!(
            svc.status("nope", "m").await,
            Err(LlmError::InvalidRequest(_))
        ));
    }

    #[tokio::test]
    async fn chat_stream_on_unknown_backend_yields_invalid_request() {
        use wafer_core::interfaces::llm::service::ChatMessage;
        let svc = ProviderLlmService::new();
        let req = ChatRequest::new("nope", "m", vec![ChatMessage::user("hi")]);
        let stream = svc.chat_stream(req, CancellationToken::new()).await;
        let items: Vec<_> = stream.collect().await;
        assert_eq!(items.len(), 1);
        assert!(matches!(items[0], Err(LlmError::InvalidRequest(_))));
    }

    #[tokio::test]
    async fn chat_stream_missing_api_key_is_unauthorized() {
        use wafer_core::interfaces::llm::service::ChatMessage;
        // OpenAI without api_key should surface Unauthorized at encode time.
        let svc = ProviderLlmService::new();
        let cfg = ProviderConfig::new(
            "openai-main",
            ProviderProtocol::OpenAi,
            "https://api.openai.com/v1",
        );
        svc.configure(vec![cfg]);
        let req = ChatRequest::new("openai-main", "gpt-4o", vec![ChatMessage::user("hi")]);
        let stream = svc.chat_stream(req, CancellationToken::new()).await;
        let items: Vec<_> = stream.collect().await;
        assert_eq!(items.len(), 1);
        assert!(matches!(items[0], Err(LlmError::Unauthorized)));
    }

    #[tokio::test]
    async fn reconfigure_replaces_previous_providers() {
        let svc = ProviderLlmService::new();
        svc.configure(vec![openai_cfg()]);
        assert!(svc.claims_backend("openai-main"));

        svc.configure(vec![local_cfg()]);
        assert!(svc.claims_backend("local"));
        assert!(!svc.claims_backend("openai-main"));
    }
}
