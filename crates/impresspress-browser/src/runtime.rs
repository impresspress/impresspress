//! Service-Worker-side Wafer runtime storage and dispatch.
//!
//! `store_wafer` stashes a fully-started `Wafer` in a `thread_local` cell;
//! `dispatch_request` converts an incoming `web_sys::Request` into a WAFER
//! `Message`, dispatches it through the stored `Wafer`'s `site-main` flow,
//! and converts the output back into a `web_sys::Response`. WASM is
//! single-threaded, so the thread_local is safe without Send/Sync bounds.

use std::cell::RefCell;

use wasm_bindgen::prelude::*;

use crate::convert;

thread_local! {
    pub(crate) static RUNTIME: RefCell<Option<wafer_run::Wafer>> = const { RefCell::new(None) };
}

/// Returned by `store_wafer` when the runtime was already initialized.
/// Caller decides whether to ignore (idempotent boot) or surface the error.
#[derive(Debug)]
pub struct StoreError;

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("store_wafer: runtime already initialized")
    }
}

impl std::error::Error for StoreError {}

/// True if `store_wafer` has been called in this SW context.
pub fn is_initialized() -> bool {
    RUNTIME.with(|r| r.borrow().is_some())
}

/// Store a fully-started `Wafer` in the SW's thread_local. Subsequent
/// `dispatch_request` calls route through this Wafer.
///
/// Returns `Err(StoreError)` if the runtime is already initialized.
/// `dispatch_request` relies on the stored `Wafer` never being replaced
/// after this call — silently overwriting would let a stale `&Wafer`
/// borrow held across an `.await` dangle.
pub fn store_wafer(wafer: wafer_run::Wafer) -> Result<(), StoreError> {
    RUNTIME.with(|r| {
        let mut borrow = r.borrow_mut();
        if borrow.is_some() {
            return Err(StoreError);
        }
        *borrow = Some(wafer);
        Ok(())
    })
}

/// Convert a browser `Request` into a WAFER `Message`, dispatch through
/// the stored `Wafer`'s `site-main` flow, and return a browser `Response`.
/// Returns a 503-shaped `Response` if called before `store_wafer`.
/// Internal errors return a 500-shaped `Response`.
pub async fn dispatch_request(request: web_sys::Request) -> Result<web_sys::Response, JsValue> {
    // SAFETY: wasm32 is single-threaded, and `store_wafer` enforces
    // single-shot initialization via `Result<(), StoreError>` — once a
    // `Wafer` is stored it is never replaced for the life of the
    // service-worker process. That invariant lets us hand out a raw
    // `*const Wafer` across `.await` without holding the `RefCell`
    // borrow (which would conflict with concurrent fetch events that
    // interleave at await points).
    let wafer_ptr = RUNTIME.with(|r| {
        let borrow = r.borrow();
        match borrow.as_ref() {
            Some(w) => Ok(w as *const wafer_run::Wafer),
            None => Err(()),
        }
    });

    let Ok(wafer_ptr) = wafer_ptr else {
        return build_error_response(
            503,
            "impresspress-browser: runtime not initialized — call store_wafer() first",
        );
    };

    let (msg, input) = convert::request_to_message(&request).await?;
    // SAFETY: see the comment on the RUNTIME.with block above —
    // `store_wafer` is single-shot, so `wafer_ptr` is valid for the rest
    // of the service-worker process's lifetime.
    let wafer = unsafe { &*wafer_ptr };
    let output = wafer.run("site-main", msg, input).await;
    convert::output_to_response(output).await
}

fn build_error_response(status: u16, body: &str) -> Result<web_sys::Response, JsValue> {
    let init = web_sys::ResponseInit::new();
    init.set_status(status);
    web_sys::Response::new_with_opt_str_and_init(Some(body), &init)
}
