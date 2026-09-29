//! `ProviderLlmService` — concrete `LlmService` impl for HTTP-based LLM
//! providers. Native-only (gated on `feature = "llm"`): uses `reqwest` +
//! `tokio` for SSE streaming, neither of which compiles on
//! `wasm32-unknown-unknown`. Browser targets use `BrowserLlmService` from
//! `impresspress-web` instead, registered on the same `MultiBackendLlmService`
//! router.

use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
    time::Duration,
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
    openai, openai_compatible,
};
use crate::{blocks::llm::provider_admin::ProviderAdmin, llm_wire::sse};

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

/// How long a provider may take to accept a TCP/TLS connection. reqwest has
/// no connect timeout of its own, so without this a provider whose host
/// blackholes SYNs holds the spawned task — and the caller's chat request —
/// for as long as the OS retries.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a connected provider may go without sending anything. Applies per
/// read, so it bounds silence rather than the completion's total length: a
/// model may legitimately stream for many minutes, but a gap this long means
/// the upstream has stopped answering and the stream must fail instead of
/// hanging on a socket nobody will write to again.
///
/// The gap that matters is time-to-first-token, which on a reasoning model
/// thinking about a long prompt can run well past two minutes — so this is
/// generous rather than tight. OpenAI's own SDK defaults to 600s for the
/// whole request; this bounds silence only, so half of that still ends a dead
/// socket long before a user waits it out.
const READ_TIMEOUT: Duration = Duration::from_secs(300);

/// Redirect-hop budget, matching reqwest's built-in `Policy::limited(10)` (the
/// default we replace). reqwest counts the initial request URL in
/// `Attempt::previous()`, so — exactly like its `Limit` arm — the bound trips
/// when `previous().len()` *exceeds* this, i.e. after 10 redirects have been
/// followed.
const MAX_REDIRECTS: usize = 10;

/// Outcome of evaluating one redirect hop.
#[derive(Debug, PartialEq, Eq)]
enum RedirectDecision {
    /// Target is a safe public address within the hop budget — follow it.
    Follow,
    /// Target textually names an internal/SSRF address — refuse the request.
    BlockSsrf,
    /// Hop budget exhausted — refuse (preserves reqwest's old `limited(10)`).
    TooManyRedirects,
}

/// Pure decision for a single redirect hop, split out of the reqwest redirect
/// closure so it is unit-testable: `reqwest::redirect::Attempt` has no public
/// constructor, so the closure itself can't be exercised directly.
///
/// `previous_len` is `Attempt::previous().len()` — the redirect chain so far,
/// *including* the initial request URL (reqwest's own counting convention).
/// SSRF is checked first so an internal target is reported as such even when it
/// also happens to be over the hop limit.
fn redirect_decision(target_url: &str, previous_len: usize) -> RedirectDecision {
    if crate::ssrf::is_ssrf_blocked_url(target_url) {
        RedirectDecision::BlockSsrf
    } else if previous_len > MAX_REDIRECTS {
        RedirectDecision::TooManyRedirects
    } else {
        RedirectDecision::Follow
    }
}

/// A reqwest redirect policy that revalidates every hop against impresspress's
/// own [`crate::ssrf::is_ssrf_blocked_url`], refusing redirects onto internal
/// addresses while preserving the default 10-hop bound. Replaces reqwest's
/// default `limited(10)` policy, which follows `3xx` targets with no per-hop
/// SSRF check. Only redirect (`3xx`) targets pass through here — the initial
/// request URL is never a redirect attempt, so the deliberate `http://localhost`
/// affordance on the first request is unaffected.
fn ssrf_revalidating_redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        match redirect_decision(attempt.url().as_str(), attempt.previous().len()) {
            RedirectDecision::Follow => attempt.follow(),
            RedirectDecision::BlockSsrf => {
                attempt.error("SSRF: redirect to internal address blocked")
            }
            RedirectDecision::TooManyRedirects => attempt.error("too many redirects"),
        }
    })
}

// SSRF posture for the provider client (native, `llm` feature).
//
// Provider endpoints are admin-configured and trusted, and the project
// deliberately supports self-hosted local models (an Ollama-style
// `http://localhost:11434/v1`, exercised by `local_cfg` in the tests below).
// So this client is intentionally NOT wrapped in the generic-network
// `wafer-net-security` `SsrfFilteringResolver`: that resolver drops any
// resolved IP that is loopback/private, which would resolve `localhost` →
// `127.0.0.1` → blocked and regress that supported configuration. Instead,
// provider-endpoint SSRF is enforced with `crate::util::validate_url_value` —
// the same validator the config `_URL` write surfaces use — at provider
// *write* time (`routes::providers`) and re-checked here at *call* time. That
// policy blocks internal-infra targets (RFC1918 / link-local / CGNAT /
// multicast / reserved IPs, the IPv6-embedded-v4 forms, and cloud-metadata
// IPs + hostnames) while keeping the deliberate `http://localhost` affordance.
//
// The initial URL is only half the story: reqwest's default redirect policy
// would follow a `3xx` from a trusted-but-compromised (or simply
// misconfigured) endpoint straight to an internal address the initial-URL gate
// never inspects. So the client installs a custom redirect policy
// (`ssrf_revalidating_redirect_policy`) that re-runs
// `crate::ssrf::is_ssrf_blocked_url` on every redirect target and refuses
// internal ones, preserving reqwest's old 10-hop bound. It fires ONLY on 3xx
// targets, so the `http://localhost` affordance on the *initial* request is
// untouched. With initial-URL and per-hop revalidation both in place,
// redirect-to-internal is closed; the sole residual versus the native
// `SsrfFilteringResolver` is DNS rebinding — a public hostname that resolves
// to a private IP at connect time — a weak vector for a fixed, admin-set
// endpoint (there is no attacker-controlled per-request URL here), and one a
// reqwest client cannot close without a resolve-before-connect hook.
impl ProviderLlmService {
    /// Construct a service with a `reqwest` client carrying the
    /// SSRF-revalidating redirect policy and the default timeouts. Returns
    /// `LlmError::BackendError` if the underlying TLS stack fails to
    /// initialize — rare in practice, and the host treats it as a build
    /// failure: there is deliberately no constructor that falls back to a
    /// client without the policy.
    pub fn try_new() -> Result<Self, LlmError> {
        Self::try_with_timeouts(CONNECT_TIMEOUT, READ_TIMEOUT)
    }

    /// [`try_new`](Self::try_new) with explicit timeouts. The whole-request
    /// timeout stays unset on purpose — a completion legitimately streams for
    /// minutes — so `read` is what bounds a provider that stops talking
    /// mid-answer.
    ///
    /// Private: the timeouts are a property of this client, not a knob its
    /// callers set. It exists because the read bound is only observable in a
    /// test that can shorten it to milliseconds, and the tests that do live
    /// in this module.
    fn try_with_timeouts(connect: Duration, read: Duration) -> Result<Self, LlmError> {
        let http = reqwest::Client::builder()
            .redirect(ssrf_revalidating_redirect_policy())
            .connect_timeout(connect)
            .read_timeout(read)
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

    /// Clone of a single provider's config, keyed by backend_id. The
    /// `api_key` it carries was resolved from `key_var` by the feature
    /// block's reload (see `routes::reload_provider_service`) before
    /// `configure()` — this service never touches the config store itself.
    fn provider_config(&self, backend_id: &str) -> Option<ProviderConfig> {
        let inner = recover_lock!(self.inner.read(), "provider svc read");
        inner.providers.get(backend_id).cloned()
    }
}

#[async_trait]
impl ProviderAdmin for ProviderLlmService {
    /// This is the router provider management exists for, so `true`.
    fn manages_providers(&self) -> bool {
        true
    }

    /// Replace the provider set. Called on feature block startup and again
    /// whenever the admin UI adds / edits / deletes a provider.
    ///
    /// For each provider, seeds `cached_models` from its explicit `models`
    /// list. Callers that want to refresh via `/v1/models` discovery should
    /// subsequently call `discover_models(name)` per provider.
    ///
    /// Always `Ok`: the write is to an in-process map behind a lock that
    /// `recover_lock!` un-poisons, so there is no failure to report. The
    /// `Result` is the trait's, and it is what lets an inert handle say it
    /// cannot do this at all.
    fn configure(&self, providers: Vec<ProviderConfig>) -> Result<(), LlmError> {
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
        Ok(())
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
        // SSRF: refuse an endpoint pointing at internal infra (same policy as
        // provider write-time validation; also catches endpoints stored before
        // that validation existed). Allows `http://localhost` for self-hosted.
        if let Err(e) = crate::util::validate_url_value(&url) {
            return Err(LlmError::InvalidRequest(format!(
                "provider endpoint blocked (SSRF): {e}"
            )));
        }
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
        // SSRF: refuse an endpoint pointing at internal infra, surfaced
        // synchronously before the request is spawned. Same policy as
        // provider write-time validation; allows `http://localhost`.
        if let Err(e) = crate::util::validate_url_value(&url) {
            return Box::pin(futures::stream::once(async move {
                Err(LlmError::InvalidRequest(format!(
                    "provider endpoint blocked (SSRF): {e}"
                )))
            }));
        }
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
            // Whether the model has said why it stopped. Both decoders emit a
            // `finish_reason` chunk from the provider's own terminal field,
            // and that — not the transport sentinel that may follow it — is
            // what says the answer is whole.
            let mut saw_finish = false;
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
                                saw_finish |= chunk.finish_reason.is_some();
                                if tx_err.send(Ok(chunk)).await.is_err() { return; }
                            }
                            // Bytes the transport dropped are bytes of the
                            // answer. Whatever arrives after them would join
                            // onto the prefix above as if nothing were
                            // missing, so the stream ends here instead.
                            if batch.lost.any() {
                                let _ = tx_err
                                    .send(Err(LlmError::BackendError(format!(
                                        "provider stream dropped {}; the answer is incomplete",
                                        batch.lost
                                    ))))
                                    .await;
                                return;
                            }
                            if batch.done { return; }
                        }
                        Some(Err(e)) => {
                            let _ = tx_err.send(Err(LlmError::Network(e.to_string()))).await;
                            return;
                        }
                        // End of body. The model saying why it stopped is what
                        // makes the answer whole — `[DONE]` / `message_stop`
                        // is a transport sentinel that OpenAI-compatible
                        // servers and proxies are free to close without, and
                        // failing on its absence would destroy complete
                        // replies. With no finish reason at all, though, the
                        // provider was cut off mid-answer, and ending cleanly
                        // here would persist and display half a reply as if
                        // the model had finished it.
                        None => {
                            if !saw_finish {
                                let cut_mid_frame = if decoder.has_unparsed_input() {
                                    " (mid-frame)"
                                } else {
                                    ""
                                };
                                let _ = tx_err
                                    .send(Err(LlmError::BackendError(format!(
                                        "provider stream ended{cut_mid_frame} before the model \
                                         reported why it stopped"
                                    ))))
                                    .await;
                            }
                            return;
                        }
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

    /// True when the decoder still holds bytes that never became a frame,
    /// i.e. the body stopped in the middle of one.
    fn has_unparsed_input(&self) -> bool {
        match self {
            Self::OpenAi(d) => d.has_unparsed_input(),
            Self::Anthropic(d) => d.has_unparsed_input(),
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
        let svc = ProviderLlmService::try_new().expect("build provider service");
        svc.configure(vec![openai_cfg(), local_cfg()])
            .expect("the provider router accepts configuration");

        let models = svc.list_models().await.unwrap();
        assert_eq!(models.len(), 3, "2 openai + 1 local");
        assert!(models.iter().any(|m| m.model_id == "gpt-4o"));
        assert!(models.iter().any(|m| m.model_id == "llama3"));
    }

    #[tokio::test]
    async fn disabled_providers_excluded_from_list_models() {
        let mut cfg = openai_cfg();
        cfg.enabled = false;
        let svc = ProviderLlmService::try_new().expect("build provider service");
        svc.configure(vec![cfg, local_cfg()])
            .expect("the provider router accepts configuration");

        let models = svc.list_models().await.unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].backend_id, "local");
    }

    #[tokio::test]
    async fn claims_backend_matches_configured_names() {
        let svc = ProviderLlmService::try_new().expect("build provider service");
        svc.configure(vec![openai_cfg()])
            .expect("the provider router accepts configuration");
        assert!(svc.claims_backend("openai-main"));
        assert!(!svc.claims_backend("local"));
    }

    #[tokio::test]
    async fn status_ready_for_enabled_provider() {
        let svc = ProviderLlmService::try_new().expect("build provider service");
        svc.configure(vec![openai_cfg()])
            .expect("the provider router accepts configuration");
        let s = svc.status("openai-main", "gpt-4o").await.unwrap();
        assert_eq!(s.state, ModelState::Ready);
    }

    #[tokio::test]
    async fn status_error_for_disabled_provider() {
        let mut cfg = openai_cfg();
        cfg.enabled = false;
        let svc = ProviderLlmService::try_new().expect("build provider service");
        svc.configure(vec![cfg])
            .expect("the provider router accepts configuration");
        let s = svc.status("openai-main", "gpt-4o").await.unwrap();
        assert!(matches!(s.state, ModelState::Error { .. }));
    }

    #[tokio::test]
    async fn status_invalid_request_for_unknown_backend() {
        let svc = ProviderLlmService::try_new().expect("build provider service");
        assert!(matches!(
            svc.status("nope", "m").await,
            Err(LlmError::InvalidRequest(_))
        ));
    }

    #[tokio::test]
    async fn chat_stream_on_unknown_backend_yields_invalid_request() {
        use wafer_core::interfaces::llm::service::ChatMessage;
        let svc = ProviderLlmService::try_new().expect("build provider service");
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
        let svc = ProviderLlmService::try_new().expect("build provider service");
        let cfg = ProviderConfig::new(
            "openai-main",
            ProviderProtocol::OpenAi,
            "https://api.openai.com/v1",
        );
        svc.configure(vec![cfg])
            .expect("the provider router accepts configuration");
        let req = ChatRequest::new("openai-main", "gpt-4o", vec![ChatMessage::user("hi")]);
        let stream = svc.chat_stream(req, CancellationToken::new()).await;
        let items: Vec<_> = stream.collect().await;
        assert_eq!(items.len(), 1);
        assert!(matches!(items[0], Err(LlmError::Unauthorized)));
    }

    #[tokio::test]
    async fn reconfigure_replaces_previous_providers() {
        let svc = ProviderLlmService::try_new().expect("build provider service");
        svc.configure(vec![openai_cfg()])
            .expect("the provider router accepts configuration");
        assert!(svc.claims_backend("openai-main"));

        svc.configure(vec![local_cfg()])
            .expect("the provider router accepts configuration");
        assert!(svc.claims_backend("local"));
        assert!(!svc.claims_backend("openai-main"));
    }

    // --- streaming transport: truncation + timeouts ------------------------

    /// What the fake provider does once it has written its body.
    enum ThenThe {
        /// Close the connection — an EOF-terminated body, which is what a
        /// provider that dies mid-answer leaves behind.
        ServerCloses,
        /// Hold the socket open and send nothing more.
        ServerGoesSilent,
    }

    /// Answer exactly one chat request on `localhost` with `body`, then behave
    /// as `then`. Returns the `http://localhost:<port>` endpoint to configure
    /// a provider with (the only plain-HTTP host `validate_url_value` allows).
    ///
    /// The response has neither `Content-Length` nor `Transfer-Encoding`, so
    /// its body runs to end-of-connection. That is the shape that reaches the
    /// decode loop's `None` arm; a provider using chunked encoding that is cut
    /// mid-message surfaces as `Some(Err(..))` instead and is already handled
    /// by the network-error arm above it.
    async fn fake_provider(body: &'static str, then: ThenThe) -> String {
        fake_provider_in_pieces(body, usize::MAX, then).await
    }

    /// [`fake_provider`] that writes the body `piece` bytes at a time with a
    /// gap between writes, so the client reads it as a sequence of transport
    /// chunks — the only way a frame larger than the parser's buffer is
    /// observable, since a frame that arrives whole is parsed whole.
    async fn fake_provider_in_pieces(body: &'static str, piece: usize, then: ThenThe) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("localhost:0")
            .await
            .expect("bind a loopback port");
        let port = listener.local_addr().expect("listener address").port();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.expect("accept the chat request");
            // Read the whole request. Stopping early can leave unread bytes in
            // the socket, and closing on those sends an RST that discards the
            // response we just wrote.
            let mut req = Vec::new();
            let mut buf = [0u8; 1024];
            while !ends_request(&req) {
                match sock.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => req.extend_from_slice(&buf[..n]),
                }
            }
            let _ = sock
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n")
                .await;
            let bytes = body.as_bytes();
            if piece == usize::MAX {
                let _ = sock.write_all(bytes).await;
            } else {
                for part in bytes.chunks(piece) {
                    if sock.write_all(part).await.is_err() {
                        break;
                    }
                    let _ = sock.flush().await;
                    // Let the client drain this piece before the next one, so
                    // the reads it sees are the pieces written here.
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            }
            let _ = sock.flush().await;
            match then {
                ThenThe::ServerCloses => {
                    // Half-close so the client sees a clean end of body rather
                    // than a reset.
                    let _ = sock.shutdown().await;
                }
                ThenThe::ServerGoesSilent => {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                }
            }
        });
        format!("http://localhost:{port}")
    }

    /// Whether `req` contains a complete HTTP request: headers, plus the body
    /// named by its `content-length` (the chat POST always has one).
    fn ends_request(req: &[u8]) -> bool {
        let text = String::from_utf8_lossy(req);
        let Some(head_end) = text.find("\r\n\r\n") else {
            return false;
        };
        let len: usize = text[..head_end]
            .lines()
            .find_map(|l| {
                let (k, v) = l.split_once(':')?;
                k.eq_ignore_ascii_case("content-length")
                    .then(|| v.trim().parse().ok())?
            })
            .unwrap_or(0);
        req.len() >= head_end + 4 + len
    }

    fn provider_at(endpoint: &str) -> ProviderConfig {
        ProviderConfig::new("local-fake", ProviderProtocol::OpenAi, endpoint)
            .with_api_key("sk-test")
            .with_models(vec!["m".into()])
    }

    fn chat_req() -> ChatRequest {
        use wafer_core::interfaces::llm::service::ChatMessage;
        ChatRequest::new("local-fake", "m", vec![ChatMessage::user("hi")])
    }

    /// Collect a stream's text deltas and its terminal item.
    async fn drive(svc: &ProviderLlmService) -> (String, Vec<Result<ChatChunk, LlmError>>) {
        let items: Vec<_> = svc
            .chat_stream(chat_req(), CancellationToken::new())
            .await
            .collect()
            .await;
        let text: String = items
            .iter()
            .filter_map(|i| match i {
                Ok(c) => match &c.delta {
                    wafer_core::interfaces::llm::service::ChunkDelta::Text(t) => Some(t.as_str()),
                    _ => None,
                },
                Err(_) => None,
            })
            .collect();
        (text, items)
    }

    async fn service_for(endpoint: &str) -> ProviderLlmService {
        let svc = ProviderLlmService::try_new().expect("build provider service");
        svc.configure(vec![provider_at(endpoint)])
            .expect("the provider router accepts configuration");
        svc
    }

    /// A provider cut off mid-answer must fail the stream, not finish it.
    ///
    /// The body stops after a text delta: no finish reason, so the model never
    /// said why it stopped. Ending the stream cleanly would persist and
    /// display half a reply as the model's complete answer.
    #[tokio::test]
    async fn a_body_cut_before_any_finish_reason_is_an_error() {
        let endpoint = fake_provider(
            "data: {\"choices\":[{\"delta\":{\"content\":\"half an ans\"}}]}\n\n",
            ThenThe::ServerCloses,
        )
        .await;
        let svc = service_for(&endpoint).await;

        let (text, items) = drive(&svc).await;

        assert_eq!(text, "half an ans", "the deltas that arrived are delivered");
        match items.last() {
            Some(Err(LlmError::BackendError(m))) => assert!(
                m.contains("ended"),
                "the error must say the stream was cut, got: {m}"
            ),
            other => panic!("a cut stream must terminate in an error, got {other:?}"),
        }
    }

    /// `[DONE]` is a transport sentinel, not the answer's terminator.
    ///
    /// OpenAI-compatible servers and proxies are free to close the connection
    /// after the final `finish_reason` chunk without sending it. Treating that
    /// as a cut stream destroys a complete reply: `handle_chat` would answer
    /// 500 and persist nothing, and the SSE path would emit `event: error`
    /// after the client had already rendered the whole answer.
    #[tokio::test]
    async fn a_finished_answer_is_delivered_even_without_the_done_sentinel() {
        let endpoint = fake_provider(
            "data: {\"choices\":[{\"delta\":{\"content\":\"whole answer\"}}]}\n\n\
             data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            ThenThe::ServerCloses,
        )
        .await;
        let svc = service_for(&endpoint).await;

        let (text, items) = drive(&svc).await;

        assert_eq!(text, "whole answer");
        assert!(
            items.iter().all(|i| i.is_ok()),
            "a finished answer must not be failed for a missing sentinel, got {items:?}"
        );
        assert!(
            items
                .iter()
                .any(|i| matches!(i, Ok(c) if c.finish_reason.is_some())),
            "the finish reason is what says the answer is whole"
        );
    }

    /// A frame too large to buffer takes part of the answer with it, so the
    /// stream must fail rather than resume past the gap.
    ///
    /// Dropping the frame and carrying on would deliver the text before it
    /// joined to the text after it — a reply with a missing middle and no
    /// marker, which is exactly what the buffering cap exists to prevent.
    #[tokio::test]
    async fn an_oversized_frame_fails_the_stream_instead_of_holing_the_answer() {
        // Leaked so the body can be `&'static str`, as the one-shot server
        // takes; the test process ends moments later.
        let body: &'static str = Box::leak(
            format!(
                "data: {{\"choices\":[{{\"delta\":{{\"content\":\"start\"}}}}]}}\n\n\
                 data: {{\"choices\":[{{\"delta\":{{\"content\":\"{}\"}}}}]}}\n\n\
                 data: {{\"choices\":[{{\"delta\":{{\"content\":\"end\"}}}}]}}\n\n\
                 data: [DONE]\n\n",
                "x".repeat(2 * sse::MAX_PENDING_FRAME_BYTES)
            )
            .into_boxed_str(),
        );
        // 64 KiB pieces: the frame is only oversized from the parser's point
        // of view while its terminator has not arrived yet.
        let endpoint = fake_provider_in_pieces(body, 64 * 1024, ThenThe::ServerCloses).await;
        let svc = service_for(&endpoint).await;

        let (text, items) = drive(&svc).await;

        assert_eq!(text, "start", "the prefix that arrived is delivered");
        assert!(
            !text.contains("end"),
            "text from after the gap must not be joined onto the prefix, got: {text}"
        );
        match items.last() {
            Some(Err(LlmError::BackendError(m))) => assert!(
                m.contains("oversized frame"),
                "the error must name what was dropped, got: {m}"
            ),
            other => panic!("a dropped frame must fail the stream, got {other:?}"),
        }
    }

    /// A provider that connects and then goes silent must not hold the task
    /// forever: the read timeout ends the stream with a network error.
    #[tokio::test]
    async fn a_silent_provider_trips_the_read_timeout() {
        let endpoint = fake_provider(
            "data: {\"choices\":[{\"delta\":{\"content\":\"x\"}}]}\n\n",
            ThenThe::ServerGoesSilent,
        )
        .await;
        let svc = ProviderLlmService::try_with_timeouts(
            Duration::from_secs(5),
            Duration::from_millis(150),
        )
        .expect("build provider service");
        svc.configure(vec![provider_at(&endpoint)])
            .expect("the provider router accepts configuration");

        let stream = svc.chat_stream(chat_req(), CancellationToken::new()).await;
        // The bound is the point: without it this collect never returns, so
        // the test asserts it completes rather than waiting on the server's
        // 30-second sleep.
        let items: Vec<_> = tokio::time::timeout(Duration::from_secs(5), stream.collect())
            .await
            .expect("a silent provider must not hold the stream open");

        match items.last() {
            Some(Err(LlmError::Network(_))) => {}
            other => panic!("expected a network (timeout) error, got {other:?}"),
        }
    }

    // --- M1: redirect-hop revalidation (see `redirect_decision`) -----------
    //
    // `reqwest::redirect::Attempt` has no public constructor, so the redirect
    // closure is exercised through its extracted pure decision seam.

    #[test]
    fn redirect_to_internal_targets_is_blocked() {
        // Cloud-metadata IP + short-name, loopback, and RFC1918 — every form
        // the initial-URL gate blocks must also be blocked on a redirect hop.
        for target in [
            "http://169.254.169.254/latest/meta-data/",
            "http://metadata/computeMetadata/v1/",
            "http://localhost/admin",
            "http://127.0.0.1/",
            "http://10.0.0.1/",
            "http://192.168.1.1/",
            "http://[::1]/",
        ] {
            assert_eq!(
                redirect_decision(target, 1),
                RedirectDecision::BlockSsrf,
                "redirect to {target} must be refused",
            );
        }
    }

    #[test]
    fn redirect_to_public_host_is_followed() {
        assert_eq!(
            redirect_decision("https://api.openai.com/v1/models", 1),
            RedirectDecision::Follow,
        );
        assert_eq!(
            redirect_decision("https://example.com/next", 3),
            RedirectDecision::Follow,
        );
    }

    #[test]
    fn redirect_hop_budget_matches_limited_10() {
        // reqwest's `limited(10)` trips when `previous().len() > 10` (the first
        // entry is the initial URL). So a public target at exactly 10 is still
        // followed; at 11 it is refused.
        assert_eq!(
            redirect_decision("https://example.com/", MAX_REDIRECTS),
            RedirectDecision::Follow,
        );
        assert_eq!(
            redirect_decision("https://example.com/", MAX_REDIRECTS + 1),
            RedirectDecision::TooManyRedirects,
        );
    }

    #[test]
    fn internal_target_over_hop_limit_reports_ssrf() {
        // SSRF is the more actionable signal, so it wins over the hop bound.
        assert_eq!(
            redirect_decision("http://169.254.169.254/", MAX_REDIRECTS + 5),
            RedirectDecision::BlockSsrf,
        );
    }
}
