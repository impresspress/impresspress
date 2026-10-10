//! The browser half of the sandbox's WORKSPACE: the dry-run probe, the runtime
//! counter and the announcement to the `/b/dev` page, and the static shell the
//! export copies. Compiled only by `browser-devtools`; a site build
//! (`browser-site-runtime` alone) has none of it.
//!
//! The runtime half — wasmi loading, the rebuild, seed-on-boot and journal
//! convergence — is `site_runtime`, which every sandbox build compiles. This
//! module adds what only the workspace asks of the host:
//!
//! * [`WorkspaceControl`] for [`BrowserRuntimeControl`] (whose
//!   [`RuntimeControl`](impresspress_core::blocks::dev::control::RuntimeControl)
//!   half is `site_runtime`'s): the probe a staged block runs before it may
//!   go live, the counter `GET /b/dev/api/status` reports, and the
//!   `dev-generation` message `dev.js` listens for;
//! * `BrowserShellSource`, the [`ShellSource`] the export reads the
//!   deployment's static files through;
//! * `shared` and `repair_llms`, the two calls `site_runtime`'s
//!   `attach` and `install` make into this module, each under
//!   `browser-devtools`: the `DevShared` with its workspace half, and the boot step
//!   that records the sandbox's own `llms.txt`.
//!
//! # The context that is not the runtime's
//!
//! [`DenyAllContext`] is what a guest is probed under. It grants nothing, on
//! purpose (see [`BrowserRuntimeControl::probe`]).
//!
//! # Why no `unsafe impl Send`/`Sync`
//!
//! For the reason `site_runtime`'s header gives: `WorkspaceControl`,
//! `ShellSource` and `Context` are bounded on `wafer_run::MaybeSend +
//! MaybeSync`, which is unbounded on `wasm32`. `clippy::arc_with_non_send_sync`
//! fires in this module only at `DenyAllContext::clone_arc` — and NOT on
//! `shared`'s `Arc::new(BrowserShellSource)`, because `BrowserShellSource`
//! is a fieldless unit struct and so is genuinely `Send + Sync`.

use std::{cell::Cell, sync::Arc};

use impresspress_core::blocks::dev::{
    control::{
        DynamicBlockSpec, GenerationAnnouncement, ShellSource, ValidationFailure, ValidationStage,
        WorkspaceControl,
    },
    seed, BypassRules, DevShared,
};
use wafer_run::{
    context::Context, ErrorCode, InputStream, LifecycleEvent, LifecycleType, Message, OutputStream,
    TerminalNotResponse, WaferError,
};
use wasm_bindgen::{prelude::*, JsCast};
use wasm_bindgen_futures::JsFuture;

use crate::site_runtime::{describe_js, load_guest, BrowserRuntimeControl, SwFetch};

/// The `DevShared` of a `browser-devtools` build: `control` as both the
/// `RuntimeControl` every activation uses and the `WorkspaceControl` the
/// workspace asks, the bypass rules, and [`BrowserShellSource`] as the shell
/// the export copies. Called by `site_runtime::attach`, which owns the order
/// it happens in (see its doc and the module header there).
pub(crate) fn shared(control: Arc<BrowserRuntimeControl>, bypass: BypassRules) -> Arc<DevShared> {
    // `ShellSource` is `MaybeSend + MaybeSync`, unbounded on wasm32, for the
    // same reason `RuntimeControl` is — this implementation resolves through a
    // `JsFuture`.
    let shell: Arc<dyn ShellSource> = Arc::new(BrowserShellSource);
    DevShared::new(control, bypass, shell)
}

/// The boot step `site_runtime::install` runs between convergence and the
/// rebuild in a `browser-devtools` build: a sandbox whose seed was imported
/// before the bundle carried an `llms.txt` fetches and publishes the one it
/// carries now ([`seed::repair_llms`]). On every other boot that is one read
/// of three `seed_info` columns — plus the active generation's manifest when
/// the sandbox's text is recorded but is not what is published (the site has
/// an `llms.txt` of its own). See that function for the accounting.
///
/// A failure is logged and retried by the next boot; the sandbox is no worse
/// off than before it was tried.
pub(crate) async fn repair_llms(ctx: &dyn Context) {
    match seed::repair_llms(ctx, &SwFetch).await {
        Ok(seed::LlmsRepair::Recorded) => web_sys::console::log_1(
            &"impresspress: dev sandbox recorded the llms.txt its bundle now carries".into(),
        ),
        Ok(_) => {}
        Err(e) => web_sys::console::warn_1(
            &format!(
                "impresspress: dev sandbox could not record its llms.txt ({e}); /llms.txt \
                 stays the site's fallback document until a later boot can"
            )
            .into(),
        ),
    }
}

// ---------------------------------------------------------------------------
// The probe context
// ---------------------------------------------------------------------------

/// The [`Context`] a guest is probed under: every host call is denied.
///
/// A probe is a dry run of untrusted code, and it happens *before* anything
/// has been registered — there is no `wafer-run/database` to route to that
/// would not be acting on the live instance's data. So the answer to every
/// host call is `PermissionDenied`, and the guest's own capability set (which
/// wasmi enforces independently) never gets the chance to matter.
///
/// The denial *count* is the load-bearing part. It is how
/// [`BrowserRuntimeControl::probe`] tells "this guest's `Init` failed because
/// the probe denied it the database" — expected, and not a reason to refuse a
/// block — from "this guest's `Init` failed on its own", which is. Nothing
/// about the guest's error code or message is consulted for that: a template
/// that wraps a host error in its own is judged the same as one that
/// propagates it.
#[derive(Default)]
struct DenyAllContext {
    /// How many host calls this context has refused, over the whole probe.
    denials: Cell<u32>,
}

impl DenyAllContext {
    /// The refusal count, for comparing before and after one probe step.
    fn denials(&self) -> u32 {
        self.denials.get()
    }

    fn deny(&self, what: &str) -> WaferError {
        self.denials.set(self.denials.get().saturating_add(1));
        WaferError::new(
            ErrorCode::PermissionDenied,
            format!("the validation probe grants no host access: {what} is unreachable"),
        )
    }
}

#[wafer_block::wafer_async_trait]
impl Context for DenyAllContext {
    async fn call_block(
        &self,
        block_name: &str,
        _msg: Message,
        _input: InputStream,
    ) -> OutputStream {
        OutputStream::error(self.deny(&format!("block {block_name:?}")))
    }

    fn is_cancelled(&self) -> bool {
        false
    }

    fn config_get(&self, _key: &str) -> Option<&str> {
        None
    }

    fn clone_arc(&self) -> Arc<dyn Context> {
        // A fresh counter rather than a shared one: the clone is handed out
        // for a caller to retain, and a probe step's verdict must be decided
        // by the calls made *during* that step through the context the step
        // was given.
        Arc::new(Self::default())
    }

    /// Deny, and count.
    ///
    /// The trait's own default already denies (it is fail-closed), so this
    /// override exists for the counter — and to say in one place that this
    /// context refuses on purpose rather than by omission.
    fn check_resource_access(
        &self,
        resource: &str,
        _resource_type: wafer_run::ResourceType,
        _access: wafer_block::ResourceAccess,
    ) -> Result<(), WaferError> {
        Err(self.deny(&format!("resource {resource:?}")))
    }

    /// Admits nothing, as [`Self::check_resource_access`] does. Not counted:
    /// this is a probe, never the authorization itself.
    fn resource_access_admitted(
        &self,
        _resource: &str,
        _resource_type: wafer_run::ResourceType,
        _access: wafer_block::ResourceAccess,
    ) -> bool {
        false
    }
}

#[wafer_block::wafer_async_trait]
impl WorkspaceControl for BrowserRuntimeControl {
    /// Run `Init`, `Start` and one request under the **accepted** spec's
    /// capabilities, swapping nothing in.
    ///
    /// # What each step proves, and what it does not
    ///
    /// The seam gives the host a `Result`/`OutputStream`, not a trap flag: a
    /// wasmi trap is delivered as an ordinary `ErrorCode::Internal`, which is
    /// also what a guest can return on its own. So neither step can name a
    /// trap exactly, and each is judged on the discriminator that is real:
    ///
    /// * **`Init` / `Start`** fail the probe when they error *without the
    ///   probe context having denied them anything*. A guest whose `Init`
    ///   calls `ensure_table` will be denied by [`DenyAllContext`] and fail;
    ///   that is the probe's own doing, not a defect, so it passes. An `Init`
    ///   that fails having asked the host for nothing failed on its own, and
    ///   would fail identically once live.
    /// * **The request** passes on any terminal except an
    ///   `ErrorCode::Internal` error or a stream that ends with no terminal at
    ///   all. A `GET` of a guest's route root is *expected* to 404 on a block
    ///   that serves only sub-paths, so an application error here proves
    ///   nothing either way — but every host-side failure (a trap, fuel
    ///   exhaustion, a missing `__wafer_handle`, a guest result that does not
    ///   decode) arrives as `Internal`, and a guest that answers `Internal` to
    ///   a bare `GET` of its own prefix is broken whichever produced it.
    ///
    /// A guest with no declared route is not requested at all; there is no URL
    /// to make up, and the static rules have already refused a block whose
    /// endpoints lie outside its prefix.
    async fn probe(
        &self,
        spec: &DynamicBlockSpec,
        artifact: &[u8],
    ) -> Result<(), ValidationFailure> {
        let block = load_guest(spec, artifact).map_err(|e| {
            ValidationFailure::new(
                ValidationStage::Load,
                format!("the module did not compile or instantiate: {e}"),
            )
        })?;
        let ctx = DenyAllContext::default();

        for (event_type, stage) in [
            (LifecycleType::Init, ValidationStage::Init),
            (LifecycleType::Start, ValidationStage::Start),
        ] {
            let before = ctx.denials();
            let outcome = block
                .lifecycle(
                    &ctx,
                    LifecycleEvent {
                        event_type,
                        data: Vec::new(),
                    },
                )
                .await;
            if let Err(e) = outcome {
                if ctx.denials() == before {
                    return Err(ValidationFailure::new(
                        stage,
                        format!(
                            "{:?} failed without asking the host for anything: {}",
                            event_type, e.message
                        ),
                    ));
                }
            }
        }

        let Some(route) = spec.routes.first() else {
            return Ok(());
        };
        let msg = wafer_block::http_codec::build_http_message(
            "GET",
            &route.prefix,
            "",
            "127.0.0.1",
            std::iter::empty::<(&str, &str)>(),
        );
        match block
            .handle(&ctx, msg, InputStream::empty())
            .await
            .collect_buffered()
            .await
        {
            Ok(_) => Ok(()),
            Err(TerminalNotResponse::Error(e)) if e.code == ErrorCode::Internal => {
                Err(ValidationFailure::new(
                    ValidationStage::Probe,
                    format!(
                        "GET {} failed inside the runtime: {}",
                        route.prefix, e.message
                    ),
                ))
            }
            Err(TerminalNotResponse::Malformed) => Err(ValidationFailure::new(
                ValidationStage::Probe,
                format!(
                    "GET {} produced no terminal event — the guest did not complete its ABI \
                     contract",
                    route.prefix
                ),
            )),
            // Error (application), Drop, Halt, Continue: the guest ran and
            // answered. Whether it answered 200 or 404 is not this step's
            // question.
            Err(_) => Ok(()),
        }
    }

    fn runtime_generation(&self) -> u64 {
        self.generation.get()
    }

    /// Post the `dev-generation` message to every window this worker
    /// controls — the `/b/dev` page in every tab, and whatever else is open
    /// on the origin, which ignores a type it does not know.
    ///
    /// Spawned rather than awaited: `matchAll` is a promise, and the trait
    /// method is fire-and-forget because the activation has already
    /// committed (see [`WorkspaceControl::announce_active`]). A failure is
    /// logged and the activation stands.
    fn announce_active(&self, generation: &GenerationAnnouncement) {
        let id = generation.id.clone();
        let message = match announcement_message(generation) {
            Ok(message) => message,
            Err(e) => {
                web_sys::console::error_1(
                    &describe_js(&e, &format!("announcing generation {id}")).into(),
                );
                return;
            }
        };
        wasm_bindgen_futures::spawn_local(async move {
            if let Err(e) = post_to_windows(&message).await {
                web_sys::console::error_1(&format!("announcing generation {id}: {e}").into());
            }
        });
    }
}

/// The `dev-generation` message [`BrowserRuntimeControl::announce_active`]
/// posts, whose shape [`GenerationAnnouncement`]'s doc states.
fn announcement_message(generation: &GenerationAnnouncement) -> Result<JsValue, JsValue> {
    let message = js_sys::Object::new();
    let changed_paths: js_sys::Array = generation
        .changed_paths
        .iter()
        .map(|path| JsValue::from_str(path))
        .collect();
    for (key, value) in [
        ("type", JsValue::from_str("dev-generation")),
        ("id", JsValue::from_str(&generation.id)),
        ("cause", JsValue::from_str(generation.cause.as_str())),
        ("changed_paths", changed_paths.into()),
    ] {
        js_sys::Reflect::set(&message, &JsValue::from_str(key), &value)?;
    }
    Ok(message.into())
}

/// Post `message` to every window client of this service worker.
///
/// One client that refuses the message does not stop the others from
/// getting it: each failure is logged on its own and the loop carries on.
async fn post_to_windows(message: &JsValue) -> Result<(), String> {
    let global: web_sys::ServiceWorkerGlobalScope = js_sys::global()
        .dyn_into()
        .map_err(|_| "activations can only be announced from a service worker".to_string())?;
    let options = web_sys::ClientQueryOptions::new();
    options.set_type(web_sys::ClientType::Window);
    let clients = JsFuture::from(global.clients().match_all_with_options(&options))
        .await
        .map_err(|e| describe_js(&e, "listing the window clients"))?;
    for client in js_sys::Array::from(&clients).iter() {
        let client: web_sys::Client = client.unchecked_into();
        if let Err(e) = client.post_message(message) {
            web_sys::console::error_1(
                &describe_js(&e, &format!("posting to client {}", client.url())).into(),
            );
        }
    }
    Ok(())
}

/// [`ShellSource`] over `/asset-manifest.json` and the service worker's own
/// `fetch` — how the export reads the static files this deployment was
/// shipped as.
///
/// # Why fetching from in here reaches the network
///
/// Two of the paths this fetches — `/` and `/index.html` — are ones the
/// worker's own `fetch` handler deliberately INTERCEPTS (`sw.js.tmpl` says so
/// outright: they are intercepted "so the consumer's router can render a UI
/// block at root"), and `/sw.js` is on the bypass list. Neither matters here,
/// because a service worker does not intercept its OWN outgoing requests: the
/// `fetch` event fires for requests from the clients a worker controls, not
/// for requests the worker itself makes. So `global.fetch('/index.html')`
/// from inside this worker goes to the network — it is the deployment's real
/// `index.html` that comes back, not the runtime's rendered landing page,
/// which is exactly what an export has to copy.
///
/// `cache: 'no-store'` because the HTTP cache is the one thing that could
/// hand this a file from a previous deployment. An export naming this
/// build's content-hashed script while carrying the previous build's bytes
/// would be a folder that 404s its own runtime.
struct BrowserShellSource;

impl BrowserShellSource {
    /// One file, by absolute path, straight from the network.
    async fn get(&self, url: &str) -> Result<Vec<u8>, String> {
        let global: web_sys::ServiceWorkerGlobalScope = js_sys::global()
            .dyn_into()
            .map_err(|_| "the static shell can only be read from a service worker".to_string())?;
        let init = web_sys::RequestInit::new();
        init.set_cache(web_sys::RequestCache::NoStore);
        let request = web_sys::Request::new_with_str_and_init(url, &init)
            .map_err(|e| describe_js(&e, &format!("building a request for {url}")))?;
        let response: web_sys::Response = JsFuture::from(global.fetch_with_request(&request))
            .await
            .map_err(|e| describe_js(&e, &format!("fetching {url}")))?
            .dyn_into()
            .map_err(|_| format!("fetching {url}: the response was not a Response"))?;
        if !response.ok() {
            return Err(format!("fetching {url}: HTTP {}", response.status()));
        }
        let buffer = JsFuture::from(
            response
                .array_buffer()
                .map_err(|e| describe_js(&e, &format!("reading {url}")))?,
        )
        .await
        .map_err(|e| describe_js(&e, &format!("reading {url}")))?;
        Ok(js_sys::Uint8Array::new(&buffer.unchecked_into::<js_sys::ArrayBuffer>()).to_vec())
    }
}

/// Where the bundler writes the shell's own file listing.
const ASSET_MANIFEST_URL: &str = "/asset-manifest.json";

/// The one field of it this reads.
#[derive(serde::Deserialize)]
struct AssetManifestFiles {
    /// Every file the bundler left in the dist directory, relative and
    /// sorted (`impresspress-bundle`'s `AssetManifest::files`).
    #[serde(default)]
    files: Vec<String>,
}

#[wafer_block::wafer_async_trait]
impl ShellSource for BrowserShellSource {
    async fn list(&self) -> Result<Vec<String>, String> {
        let bytes = self.get(ASSET_MANIFEST_URL).await?;
        let manifest: AssetManifestFiles = serde_json::from_slice(&bytes)
            .map_err(|e| format!("{ASSET_MANIFEST_URL} did not parse: {e}"))?;
        if manifest.files.is_empty() {
            // An empty list is not "a shell with no files" — it is a manifest
            // written by a bundler that predates the `files` field, and the
            // export would silently produce a folder with no runtime in it.
            return Err(format!(
                "{ASSET_MANIFEST_URL} lists no files; this bundle was built by an \
                 impresspress-bundle that does not write the shell's file list"
            ));
        }
        Ok(manifest.files)
    }

    async fn fetch(&self, path: &str) -> Result<Vec<u8>, String> {
        self.get(&format!("/{path}")).await
    }
}
