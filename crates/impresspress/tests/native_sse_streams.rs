//! The llm SSE routes stream over the native server, end to end.
//!
//! Each test boots the server the binary runs (`start_native`: the runtime
//! `build_native_runtime` builds, the `site-main` flow, the HTTP listener on a
//! loopback port) with one extra LLM backend, [`Gated`], registered the way a
//! downstream site registers one (`ImpresspressBuilder::llm_service` through
//! [`AppHooks`]). Everything between the socket and that backend is the
//! production path: the listener, `site-main`'s middleware, the pipeline's
//! auth, the `impresspress/llm` routes, `wafer-run/llm`'s handler and router.
//!
//! The backend's answers are gated by the test, so "the first frame arrives
//! before the stream ends" is an ordering fact, not a timing guess: the
//! client reads the frame while the backend cannot have finished. Every
//! timeout below only turns a hang into a failure.
//!
//! What is pinned:
//! - a chat streams frame by frame over the real listener, under the SSE head
//!   (`text/event-stream`, `no-cache`, `X-Accel-Buffering: no`, chunked);
//! - a client that disconnects while the model is silent stops the model;
//! - a model load whose first progress item is not there yet has already
//!   sent its head and the open frame, and a client that leaves then stops
//!   the load.

use std::{collections::HashMap, path::Path, sync::Arc, time::Duration};

use futures::stream::BoxStream;
use impresspress_core::blocks::auth::config::{
    BOOTSTRAP_ADMIN_EMAIL_KEY, BOOTSTRAP_ADMIN_PASSWORD_KEY,
};
use impresspress_native::InfraConfig;
use impresspress_password::pepper::PasswordPeppers;
use impresspress_server::{start_native, AppHooks, IMPRESSPRESS_LISTENER_FLOW};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::Notify,
};
use tokio_util::sync::CancellationToken;
use wafer_core::interfaces::llm::service::{
    ChatChunk, ChatRequest, LlmError, LlmService, LoadProgress, ModelInfo, ModelStatus,
};
use wafer_run::Wafer;

const ADMIN_EMAIL: &str = "admin@example.com";
const ADMIN_PASSWORD: &str = "correct-horse-battery-staple";

/// The backend id [`Gated`] claims.
const BACKEND: &str = "gated";
const MODEL: &str = "gated-model";

/// How long a step that needs nothing but scheduling and loopback I/O may
/// take before the test calls it a hang.
const STEP: Duration = Duration::from_secs(10);

/// An LLM backend whose answers the test releases.
///
/// A chat answers `"one"` at once, then waits: [`Gated::release`] lets it
/// answer `"two"` and end. A model load says nothing until released, then
/// reports one progress item and ends. Either way, the cancellation token
/// `wafer-run/llm`'s handler hands the backend — the one that fires when the
/// handler's output is dropped — is watched the way a provider watches it
/// (`ProviderLlmService` abandons its HTTP request on it), and
/// [`Gated::stopped`] reports that it fired.
#[derive(Clone, Default)]
struct Gated {
    release: Arc<Notify>,
    cancelled: Arc<Notify>,
}

impl Gated {
    fn release(&self) {
        self.release.notify_one();
    }

    /// Wait for the backend's cancellation token to fire.
    async fn stopped(&self, what: &str) {
        tokio::time::timeout(STEP, self.cancelled.notified())
            .await
            .unwrap_or_else(|_| panic!("{what}"));
    }

    /// A stream whose items `run` sends, with `run` racing the backend's
    /// release against its cancellation.
    fn answer<T: Send + 'static>(
        &self,
        first: Option<T>,
        then: T,
        cancel: CancellationToken,
    ) -> BoxStream<'static, Result<T, LlmError>> {
        let (tx, rx) = futures::channel::mpsc::unbounded();
        let release = Arc::clone(&self.release);
        let cancelled = Arc::clone(&self.cancelled);
        if let Some(first) = first {
            tx.unbounded_send(Ok(first))
                .expect("the receiver is held below");
        }
        tokio::spawn(async move {
            tokio::select! {
                () = release.notified() => {
                    let _ = tx.unbounded_send(Ok(then));
                }
                () = cancel.cancelled() => cancelled.notify_one(),
            }
        });
        Box::pin(rx)
    }
}

#[wafer_block::wafer_async_trait]
impl LlmService for Gated {
    async fn chat_stream(
        &self,
        _req: ChatRequest,
        cancel: CancellationToken,
    ) -> BoxStream<'static, Result<ChatChunk, LlmError>> {
        self.answer(Some(ChatChunk::text("one")), ChatChunk::text("two"), cancel)
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, LlmError> {
        Ok(Vec::new())
    }

    async fn status(&self, _backend_id: &str, _model_id: &str) -> Result<ModelStatus, LlmError> {
        Ok(ModelStatus::ready())
    }

    fn load_model(
        &self,
        _backend_id: &str,
        _model_id: &str,
        cancel: CancellationToken,
    ) -> BoxStream<'static, Result<LoadProgress, LlmError>> {
        self.answer(None, LoadProgress::new("ready"), cancel)
    }

    fn claims_backend(&self, backend_id: &str) -> bool {
        backend_id == BACKEND
    }
}

/// A native server serving on a loopback port, with [`Gated`] as a backend
/// and a signed-in admin.
struct Server {
    port: u16,
    token: String,
    _wafer: Arc<Wafer>,
    _tmp: tempfile::TempDir,
}

fn infra_for(db_path: &Path, storage_root: &Path, port: u16) -> InfraConfig {
    InfraConfig {
        listen: format!("127.0.0.1:{port}"),
        db_type: "sqlite".to_string(),
        db_path: db_path
            .to_str()
            .expect("db path is valid utf-8")
            .to_string(),
        db_url: None,
        storage_type: "local".to_string(),
        storage_root: storage_root
            .to_str()
            .expect("storage root is valid utf-8")
            .to_string(),
        model_cache_dir: "data/models".to_string(),
        listener: Default::default(),
    }
}

async fn serve(backend: Gated) -> Server {
    watch_listening();
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("sse.sqlite3");
    let storage_root = tmp.path().join("storage");
    std::fs::create_dir_all(&storage_root).expect("create storage root");
    // The listener binds the address it is configured with and cannot
    // report the port of a `:0` bind, so a free port is picked here and handed
    // to it ([`listening`] says what happens if it is taken meanwhile).
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("reserve a loopback port")
        .port();
    let infra = infra_for(&db_path, &storage_root, port);
    let database = impresspress_native::make_database_service(&infra.db_type, &infra.db_path, None)
        .await
        .expect("construct sqlite database service");
    let app_env = HashMap::from([
        (
            BOOTSTRAP_ADMIN_EMAIL_KEY.to_string(),
            ADMIN_EMAIL.to_string(),
        ),
        (
            BOOTSTRAP_ADMIN_PASSWORD_KEY.to_string(),
            ADMIN_PASSWORD.to_string(),
        ),
    ]);
    let hooks = AppHooks {
        register_blocks: Box::new(move |builder| {
            Ok(builder.llm_service(BACKEND, Arc::new(backend)))
        }),
        register_post_build: Box::new(|_, _| Ok(())),
    };
    let wafer = start_native(
        &infra,
        database,
        &app_env,
        PasswordPeppers::default(),
        IMPRESSPRESS_LISTENER_FLOW,
        hooks,
    )
    .await
    .expect("the native server starts");
    listening(port).await;

    let mut server = Server {
        port,
        token: String::new(),
        _wafer: wafer,
        _tmp: tmp,
    };
    let (status, login) = server
        .json(
            "POST",
            "/b/auth/api/login",
            serde_json::json!({ "email": ADMIN_EMAIL, "password": ADMIN_PASSWORD }),
        )
        .await;
    assert_eq!(status, 200, "sign in: {login}");
    server.token = login["access_token"]
        .as_str()
        .expect("the sign-in answers an access token")
        .to_string();
    server
}

/// The addresses the HTTP listener has reported binding, from its
/// `listening` event — `target = "wafer.runtime"`, `event = "listening"`,
/// `addr`, emitted right after the bind and kept stable as a contract
/// (`wafer-block-http-listener`, consumed by `wafer dev`).
struct Bound {
    addrs: std::sync::Mutex<std::collections::HashSet<String>>,
    reported: Notify,
}

fn bound() -> &'static Bound {
    static BOUND: std::sync::OnceLock<Bound> = std::sync::OnceLock::new();
    BOUND.get_or_init(|| Bound {
        addrs: std::sync::Mutex::new(std::collections::HashSet::new()),
        reported: Notify::new(),
    })
}

/// A tracing layer that records each `listening` event's `addr` in [`bound`].
struct ListeningLayer;

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for ListeningLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        #[derive(Default)]
        struct Fields {
            event: Option<String>,
            addr: Option<String>,
        }
        impl tracing::field::Visit for Fields {
            fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
                self.record(field.name(), value.to_string());
            }
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                self.record(field.name(), format!("{value:?}"));
            }
        }
        impl Fields {
            fn record(&mut self, name: &str, value: String) {
                match name {
                    "event" => self.event = Some(value),
                    "addr" => self.addr = Some(value),
                    _ => {}
                }
            }
        }
        let mut fields = Fields::default();
        event.record(&mut fields);
        if let (Some("listening"), Some(addr)) = (fields.event.as_deref(), fields.addr) {
            bound().addrs.lock().expect("bound addresses").insert(addr);
            bound().reported.notify_waiters();
        }
    }
}

/// Install [`ListeningLayer`] as the process's subscriber, once. A global
/// subscriber, because the event is emitted on the listener's own task, on
/// whichever runtime thread runs it.
fn watch_listening() {
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| {
        use tracing_subscriber::{layer::SubscriberExt, Layer};
        let only_the_runtime =
            tracing_subscriber::filter::filter_fn(|meta| meta.target() == "wafer.runtime");
        tracing::subscriber::set_global_default(
            tracing_subscriber::registry().with(ListeningLayer.with_filter(only_the_runtime)),
        )
        .expect("no other subscriber in this test binary");
    });
}

/// Wait for the listener to report that it bound `127.0.0.1:{port}`.
///
/// `start_native` returns once the listener's server task is spawned; the
/// task binds as its first step and then emits its `listening` event. So
/// this waits on that event, not on the socket: a port another process took
/// in the meantime fails the bind, no event comes, and the test fails here
/// rather than talking to that process.
async fn listening(port: u16) {
    let addr = format!("127.0.0.1:{port}");
    tokio::time::timeout(STEP, async {
        loop {
            let reported = bound().reported.notified();
            tokio::pin!(reported);
            reported.as_mut().enable();
            if bound()
                .addrs
                .lock()
                .expect("bound addresses")
                .contains(&addr)
            {
                return;
            }
            reported.await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the listener never reported binding {addr}"));
}

impl Server {
    /// Open a connection and send `method path` with a JSON `body`.
    async fn send(&self, method: &str, path: &str, body: &serde_json::Value) -> Conn {
        let body = body.to_string();
        let mut sock = TcpStream::connect(("127.0.0.1", self.port))
            .await
            .expect("connect to the server");
        let auth = if self.token.is_empty() {
            String::new()
        } else {
            format!("Authorization: Bearer {}\r\n", self.token)
        };
        let request = format!(
            "{method} {path} HTTP/1.1\r\n\
             Host: 127.0.0.1:{port}\r\n\
             Origin: http://127.0.0.1:{port}\r\n\
             Sec-Fetch-Site: same-origin\r\n\
             Accept: application/json\r\n\
             Content-Type: application/json\r\n\
             {auth}\
             Content-Length: {len}\r\n\
             Connection: close\r\n\r\n{body}",
            port = self.port,
            len = body.len(),
        );
        sock.write_all(request.as_bytes())
            .await
            .expect("send the request");
        Conn {
            sock,
            received: Vec::new(),
        }
    }

    /// A buffered JSON call: its status and body.
    async fn json(
        &self,
        method: &str,
        path: &str,
        body: serde_json::Value,
    ) -> (u16, serde_json::Value) {
        let mut conn = self.send(method, path, &body).await;
        conn.read_to_end(path).await;
        let text = conn.text();
        let (head, body) = text
            .split_once("\r\n\r\n")
            .unwrap_or_else(|| panic!("{path}: no head in {text:?}"));
        let status = head
            .split(' ')
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| panic!("{path}: no status in {head:?}"));
        let body = serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
        (status, body)
    }

    /// A conversation the admin owns, for a chat to write to.
    async fn thread(&self) -> String {
        let (status, thread) = self
            .json(
                "POST",
                "/b/messages/api/contexts",
                serde_json::json!({ "type": "conversation", "title": "sse" }),
            )
            .await;
        assert_eq!(status, 200, "create a thread: {thread}");
        thread["id"]
            .as_str()
            .or_else(|| thread["data"]["id"].as_str())
            .unwrap_or_else(|| panic!("a thread id in {thread}"))
            .to_string()
    }

    /// Start a streamed chat on `thread`, routed to [`Gated`].
    async fn chat(&self, thread: &str) -> Conn {
        self.send(
            "POST",
            "/b/llm/api/chat/stream",
            &serde_json::json!({
                "thread_id": thread,
                "message": "hi",
                "provider": BACKEND,
                "model": MODEL,
            }),
        )
        .await
    }
}

/// One client connection and everything read from it so far.
struct Conn {
    sock: TcpStream,
    received: Vec<u8>,
}

impl Conn {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.received).into_owned()
    }

    /// Read until what was received contains `needle`.
    async fn read_until(&mut self, needle: &str, what: &str) {
        let read = async {
            let mut buf = [0u8; 4096];
            while !self.text().contains(needle) {
                let n = self.sock.read(&mut buf).await.expect("read the response");
                if n == 0 {
                    return Err("the connection closed");
                }
                self.received.extend_from_slice(&buf[..n]);
            }
            Ok(())
        };
        match tokio::time::timeout(STEP, read).await {
            Ok(Ok(())) => {}
            Ok(Err(why)) => panic!("{what}: {why} first; received {:?}", self.text()),
            Err(_) => panic!(
                "{what}: not received within {STEP:?}; received {:?}",
                self.text()
            ),
        }
    }

    /// Read until the server closes the connection.
    async fn read_to_end(&mut self, what: &str) {
        tokio::time::timeout(STEP, self.sock.read_to_end(&mut self.received))
            .await
            .unwrap_or_else(|_| panic!("{what}: the response did not end within {STEP:?}"))
            .expect("read the response");
    }

    /// What was received of the body so far, as raw chunked framing.
    fn body(&self) -> String {
        let text = self.text();
        let (_, body) = text
            .split_once("\r\n\r\n")
            .expect("the head has been received");
        body.to_string()
    }

    /// The response head, lowercased, once it has been received.
    fn head(&self) -> String {
        let text = self.text();
        let (head, _) = text
            .split_once("\r\n\r\n")
            .expect("the head has been received");
        head.to_ascii_lowercase()
    }
}

/// The head an SSE response must carry: streamed, chunked, uncached and not
/// buffered by a proxy.
fn assert_sse_head(head: &str) {
    assert!(head.starts_with("http/1.1 200"), "{head}");
    for line in [
        "content-type: text/event-stream",
        "cache-control: no-cache",
        "x-accel-buffering: no",
        "transfer-encoding: chunked",
    ] {
        assert!(
            head.lines().any(|l| l == line),
            "the SSE head carries `{line}`: {head}"
        );
    }
    assert!(
        !head.contains("content-length"),
        "a stream declares no length: {head}"
    );
}

/// A chat's frames reach the client one by one while the model is still
/// answering: the first is read while the backend holds the second, which
/// a server that collected the body first could never send. Releasing the
/// backend then delivers the rest and ends the chunked body cleanly.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_chat_streams_frame_by_frame_over_the_native_listener() {
    let backend = Gated::default();
    let server = serve(backend.clone()).await;
    let thread = server.thread().await;

    let mut chat = server.chat(&thread).await;
    chat.read_until(
        "\"one\"",
        "the first frame, while the model holds the second",
    )
    .await;
    assert_sse_head(&chat.head());
    let before = chat.body();
    let open = before.find(": open\n\n").expect("the open frame was sent");
    let first = before.find("\"one\"").expect("the first frame was sent");
    assert!(open < first, "the open frame comes first: {before:?}");
    assert!(
        !before.contains("\"two\"") && !before.contains("[DONE]"),
        "the model has not answered more yet: {before:?}"
    );

    backend.release();
    chat.read_until("\"two\"", "the second frame").await;
    chat.read_until("data: [DONE]\n\n", "the done frame").await;
    chat.read_to_end("the chat").await;
    assert!(
        chat.text().ends_with("0\r\n\r\n"),
        "the chunked body ends cleanly: {:?}",
        chat.text()
    );
}

/// A client that disconnects while the model is silent stops the model: the
/// backend's cancellation token fires without the model having to say
/// anything more.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_that_disconnects_mid_chat_stops_the_model() {
    let backend = Gated::default();
    let server = serve(backend.clone()).await;
    let thread = server.thread().await;

    let mut chat = server.chat(&thread).await;
    chat.read_until("\"one\"", "the first frame").await;
    drop(chat);

    backend
        .stopped("the model is stopped once the client disconnects")
        .await;
}

/// A model load commits its head and the open frame before its first
/// progress item exists — a client sees a live stream, not a request that
/// has not been answered — and a client that leaves before that item stops
/// the load.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_model_load_opens_before_its_first_item_and_stops_when_the_client_leaves() {
    let backend = Gated::default();
    let server = serve(backend.clone()).await;

    let mut load = server
        .send(
            "POST",
            &format!("/b/llm/api/models/{BACKEND}/{MODEL}/load"),
            &serde_json::json!({}),
        )
        .await;
    load.read_until(": open\n\n", "the open frame, before any progress")
        .await;
    assert_sse_head(&load.head());
    assert!(
        !load.body().contains("data: "),
        "no progress exists yet: {:?}",
        load.body()
    );
    drop(load);

    backend
        .stopped("the load is stopped once the client disconnects")
        .await;
}
