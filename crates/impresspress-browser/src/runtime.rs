//! Service-Worker-side Wafer runtime storage and dispatch.
//!
//! The active runtime is an `Rc<Wafer>`. Every dispatch clones the `Rc`
//! before its first `.await`, so a `replace_wafer` that lands while a
//! request is in flight leaves that request on the runtime it started on
//! and routes every later request to the new one. wasm32 is
//! single-threaded, so the thread_local needs no Send/Sync.

use std::{cell::RefCell, rc::Rc};

use wasm_bindgen::prelude::*;

use crate::convert;

thread_local! {
    pub(crate) static RUNTIME: RefCell<Option<Rc<wafer_run::Wafer>>> = const { RefCell::new(None) };
}

#[derive(Debug, PartialEq, Eq)]
pub enum StoreError {
    AlreadyInitialized,
    NotInitialized,
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyInitialized => f.write_str("store_wafer: runtime already initialized"),
            Self::NotInitialized => f.write_str("replace_wafer: runtime not initialized"),
        }
    }
}

impl std::error::Error for StoreError {}

/// True if a runtime is currently stored (via `store_wafer` or `replace_wafer`).
pub fn is_initialized() -> bool {
    RUNTIME.with(|r| r.borrow().is_some())
}

/// Clone a handle to the currently active runtime, if any.
pub fn current_wafer() -> Option<Rc<wafer_run::Wafer>> {
    RUNTIME.with(|r| r.borrow().clone())
}

/// Install the first runtime. Cold initialization only — a second call is an
/// error so an accidental double `initialize()` cannot swap runtimes silently.
pub fn store_wafer(wafer: wafer_run::Wafer) -> Result<(), StoreError> {
    RUNTIME.with(|r| {
        let mut slot = r.borrow_mut();
        if slot.is_some() {
            return Err(StoreError::AlreadyInitialized);
        }
        *slot = Some(Rc::new(wafer));
        Ok(())
    })
}

/// Swap in a rebuilt runtime and hand back the one that was active.
///
/// The returned handle is what makes the swap reversible: an activation
/// rebuilds the runtime *before* it publishes the site, so a publish that
/// fails afterwards has to put the previous runtime back. Hold the returned
/// `Rc` across the rest of the activation and pass it to [`restore_wafer`] on
/// that path; drop it once the activation has committed. A caller that
/// discards it cannot undo the swap — the old runtime is gone as soon as the
/// last handle to it is.
///
/// The caller that does this is `impresspress-web`'s `BrowserRuntimeControl`:
/// it parks the handle in its `retained` slot on every successful `rebuild`
/// and hands it to [`restore_wafer`] from `RuntimeControl::restore_previous`,
/// which is the rollback half of design §7.3.
pub fn replace_wafer(wafer: wafer_run::Wafer) -> Result<Rc<wafer_run::Wafer>, StoreError> {
    RUNTIME.with(|r| {
        let mut slot = r.borrow_mut();
        let previous = slot.take().ok_or(StoreError::NotInitialized)?;
        *slot = Some(Rc::new(wafer));
        Ok(previous)
    })
}

/// Restore a runtime handed back by [`replace_wafer`].
///
/// Infallible by construction, and that is the point of restoring the value
/// rather than rebuilding one: the caller is already on a failure path, and
/// the runtime it wants back is the one it is holding.
pub fn restore_wafer(previous: Rc<wafer_run::Wafer>) {
    RUNTIME.with(|r| *r.borrow_mut() = Some(previous));
}

/// Convert a browser `Request` into a WAFER `Message`, dispatch through
/// the currently active `Wafer`'s `site-main` flow, and return a browser
/// `Response`. Returns a 503-shaped `Response` if called before
/// `store_wafer`; internal errors return a 500-shaped `Response`. A request
/// body over `impresspress_core::streaming::MAX_REQUEST_BODY_BYTES` is marked
/// by `convert::request_to_message` and answered 413 by the flow, like any
/// other refusal.
///
/// The `Rc` is cloned synchronously (before the first `.await`), so a
/// `replace_wafer` that lands mid-dispatch does not affect this call — it
/// keeps running against the runtime it started on.
pub async fn dispatch_request(request: web_sys::Request) -> Result<web_sys::Response, JsValue> {
    let Some(wafer) = current_wafer() else {
        return build_error_response(
            503,
            "impresspress-browser: runtime not initialized — call store_wafer() first",
        );
    };
    let (msg, input) = convert::request_to_message(&request).await?;
    let output = wafer.run("site-main", msg, input).await;
    convert::output_to_response(output).await
}

fn build_error_response(status: u16, body: &str) -> Result<web_sys::Response, JsValue> {
    let init = web_sys::ResponseInit::new();
    init.set_status(status);
    web_sys::Response::new_with_opt_str_and_init(Some(body), &init)
}

#[cfg(all(test, target_arch = "wasm32"))]
mod tests {
    use wasm_bindgen_test::*;

    use super::*;

    fn empty_wafer() -> wafer_run::Wafer {
        let cfg: std::sync::Arc<dyn wafer_run::ConfigSource> =
            std::sync::Arc::new(wafer_run::StaticConfigSource::default());
        wafer_run::Wafer::new(cfg).expect("wafer")
    }

    fn reset() {
        RUNTIME.with(|r| *r.borrow_mut() = None);
    }

    #[wasm_bindgen_test]
    fn first_store_succeeds_and_second_cold_store_fails() {
        reset();
        assert!(!is_initialized());
        store_wafer(empty_wafer()).expect("first store");
        assert!(is_initialized());
        assert!(
            store_wafer(empty_wafer()).is_err(),
            "store_wafer is single-shot"
        );
    }

    #[wasm_bindgen_test]
    fn replace_returns_the_previous_runtime_and_keeps_it_alive() {
        reset();
        store_wafer(empty_wafer()).unwrap();
        let held = current_wafer().expect("current");
        let previous = replace_wafer(empty_wafer()).expect("replace");
        assert!(
            Rc::ptr_eq(&held, &previous),
            "replace hands back the runtime that was active"
        );
        assert_eq!(
            Rc::strong_count(&previous),
            2,
            "an in-flight holder keeps the old runtime alive"
        );
        let now = current_wafer().unwrap();
        assert!(!Rc::ptr_eq(&now, &previous));
    }

    #[wasm_bindgen_test]
    fn replace_before_store_is_an_error() {
        reset();
        assert!(replace_wafer(empty_wafer()).is_err());
    }

    /// **Fails on the pre-fix tree**, where an over-cap body left
    /// `request_to_message` as a `JsValue` error: `dispatch_request` returns
    /// `Err`, the Service Worker's `respondWith` rejects, and the uploader's
    /// `fetch` fails with no status at all. The body is now dropped and the
    /// message marked, and the flow answers 413 — the assertion of the status
    /// itself lives with the code that builds it
    /// (`impresspress_core::pipeline`'s `oversized_body_tests`), because an
    /// empty `Wafer` has no `site-main` flow to answer through.
    #[wasm_bindgen_test]
    async fn an_over_cap_request_body_is_marked_and_dropped() {
        let body = js_sys::Uint8Array::new_with_length(
            (impresspress_core::streaming::MAX_REQUEST_BODY_BYTES + 1) as u32,
        );
        let init = web_sys::RequestInit::new();
        init.set_method("POST");
        init.set_body(&body);
        let request = web_sys::Request::new_with_str_and_init(
            "https://dev.impresspress.org/b/storage/api/buckets/photos/objects?key=big.bin",
            &init,
        )
        .expect("build request");

        let (msg, input) = convert::request_to_message(&request)
            .await
            .expect("conversion must not fail the fetch");

        assert!(
            impresspress_core::streaming::body_too_large(&msg),
            "the marker the pipeline refuses on"
        );
        let forwarded = input
            .collect_to_bytes()
            .await
            .expect("an in-memory body does not fail");
        assert!(
            forwarded.is_empty(),
            "an oversized body must not reach a block"
        );
    }
}
