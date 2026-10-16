//! Executor diagnostics, off unless the Worker var `IMPRESSPRESS_EXECUTOR_TRACE`
//! is `"1"`.
//!
//! They exist to classify a request Workers reports as hung at ~0 ms CPU after
//! an earlier request in the same isolate was hard-stopped. The explanations
//! that fit that row each leave a different trail here:
//!
//! - the runtime never ran the Worker's JavaScript: no `ip-entry` line;
//! - the JavaScript ran and created the Rust `fetch` future, but the executor
//!   never polled it (js-sys 0.3.99's task queue stranded by the kill, which
//!   the js-sys the workspace `Cargo.toml` patches in no longer allows):
//!   `ip-entry` and no `ip-poll`, on a wasm id that has served traced
//!   requests before;
//! - worker-build's shim re-instantiated the Wasm module after a
//!   `WebAssembly.RuntimeError`: the `ip-entry` of the first request the new
//!   instance serves carries a new wasm id and the next generation, whether or
//!   not that request is ever polled;
//! - the shadow stack ran out after a series of kills (each kill skips the
//!   epilogues that restore `__stack_pointer`): the stack figure on both lines
//!   falls kill by kill, then a trap and a reset.
//!
//! # The signals
//!
//! - `ip-entry <isolate> <wasm> <generation> <seq> <stack>`: logged by the
//!   generated JS entry module (`impresspress`'s
//!   `cli::helpers::cloudflare::entry`) before it calls the shim's `fetch`.
//!   The wasm id, generation and stack come from [`executor_probe`], a
//!   synchronous export the module calls after the shim's
//!   re-initialization check, so they name the instance that serves the
//!   request. The probe runs outside the executor, so it answers even when the
//!   task queue is wedged.
//! - `ip-poll <isolate> <wasm> <generation> <seq> <stack>`: logged by
//!   [`on_first_poll`], which [`run_with_config`](crate::run_with_config) calls
//!   before its first await, so the line marks the request future's first
//!   poll.
//! - `x-ip-instance: <isolate>/<wasm>/<generation>` on every `Ok` response.
//!
//! # The identities
//!
//! - **isolate**: random, created by the entry module on its first traced
//!   request and held in JavaScript ([`STATE_GLOBAL`]), so it survives a
//!   re-instantiation of the Wasm module.
//! - **wasm**: random, held in a Rust `thread_local`. A re-instantiated module
//!   has fresh linear memory and so draws a new one.
//! - **generation**: a counter held in JavaScript, incremented when an instance
//!   draws its wasm id, which happens on the first traced request it serves
//!   (in the probe, or in [`on_first_poll`] when there is no entry module). A
//!   JS value survives `__wbg_reset_state`, so `generation - 1` is how many
//!   times the shim reset this isolate's instance before that request.
//! - **seq**: the entry module's per-isolate request number. It reaches Rust
//!   through a `WeakMap` keyed by the request object, which is the same JS
//!   object the shim hands to the Rust `fetch`; a single "current request"
//!   global would be overwritten by a concurrent request before this one's
//!   first poll.
//! - **stack**: the address of a local in a non-inlined function, i.e. a
//!   point on the shadow stack. On `ip-entry` it is taken from the probe,
//!   called from JS with no Rust frame below it, so it is the stack pointer
//!   of an idle instance; on `ip-poll` it is taken at a fixed call depth. On
//!   one wasm id either figure should not move; a figure that falls is stack
//!   leaked by hard-stopped requests.
//!
//! A field the entry module did not provide (a hand-written Wrangler config
//! whose `main` is worker-build's shim) prints as `-`.
//!
//! With the var unset nothing here runs past the flag check: no JS global is
//! read or created, no line is logged, no header is set. The probe is the one
//! piece the var cannot gate directly, because worker-build exposes every
//! export as an entrypoint method, which a service binding can call with
//! arguments of its choosing; it answers `undefined`, having touched nothing,
//! unless the shared state exists, and only a traced request creates that.

use std::cell::OnceCell;

use js_sys::{Object, Reflect, WeakMap};
use wasm_bindgen::{prelude::wasm_bindgen, JsCast, JsValue};

use crate::environment::CfEnvironment;

/// The JS global holding the state the entry module and this module share.
/// The name, and the field names below, are a contract with the module
/// `impresspress`'s `cli::helpers::cloudflare::entry` generates.
pub(crate) const STATE_GLOBAL: &str = "__impresspressExecutorTrace";
const ISOLATE_ID_FIELD: &str = "isolateId";
const WASM_GENERATION_FIELD: &str = "wasmGeneration";
const REQUESTS_FIELD: &str = "requests";

/// The fields of the object [`executor_probe`] returns, read by the entry
/// module.
const PROBE_WASM_FIELD: &str = "wasm";
const PROBE_GENERATION_FIELD: &str = "generation";
const PROBE_STACK_FIELD: &str = "stack";

/// The response header carrying [`RequestTrace::instance`].
pub(crate) const INSTANCE_HEADER: &str = "x-ip-instance";

/// This Wasm instance's identity, drawn by its first traced request.
#[derive(Debug)]
struct WasmInstance {
    id: String,
    generation: u32,
}

impl WasmInstance {
    /// Draw a fresh id and take the next generation from the JS-held counter.
    fn start(state: &Object) -> Self {
        let generation = count_field(state, WASM_GENERATION_FIELD)
            .unwrap_or(0)
            .saturating_add(1);
        set_field(state, WASM_GENERATION_FIELD, &JsValue::from(generation));
        Self {
            id: uuid::Uuid::new_v4().simple().to_string(),
            generation,
        }
    }
}

thread_local! {
    /// One per Wasm instance: a `thread_local` lives in linear memory, which
    /// `__wbg_reset_state` replaces.
    static WASM_INSTANCE: OnceCell<WasmInstance> = const { OnceCell::new() };
}

/// A point on the shadow stack: the address of a local whose address is
/// taken, which on wasm32 puts it in linear memory below `__stack_pointer`.
/// Not inlined, so every caller measures from its own frame at one fixed
/// extra depth.
#[inline(never)]
fn stack_address() -> usize {
    let marker = 0_u8;
    std::ptr::from_ref(std::hint::black_box(&marker)) as usize
}

/// One request's identities, as the `ip-poll` line and the response header
/// carry them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RequestTrace {
    isolate: Option<String>,
    wasm: String,
    generation: u32,
    seq: Option<u32>,
    stack: usize,
}

impl RequestTrace {
    /// `ip-poll <isolate> <wasm> <generation> <seq> <stack>`.
    pub(crate) fn poll_line(&self) -> String {
        format!(
            "ip-poll {} {} {} {} {}",
            self.isolate.as_deref().unwrap_or("-"),
            self.wasm,
            self.generation,
            self.seq
                .map_or_else(|| "-".to_string(), |seq| seq.to_string()),
            self.stack,
        )
    }

    /// `<isolate>/<wasm>/<generation>`, the [`INSTANCE_HEADER`] value.
    pub(crate) fn instance(&self) -> String {
        format!(
            "{}/{}/{}",
            self.isolate.as_deref().unwrap_or("-"),
            self.wasm,
            self.generation,
        )
    }

    /// Set [`INSTANCE_HEADER`] on `response`.
    ///
    /// A response whose headers are immutable (one passed through from a
    /// subrequest) refuses the write. That is logged rather than returned: a
    /// diagnostic must not turn a served response into an error, and the
    /// `ip-poll` line already carries the same identities.
    pub(crate) fn stamp(&self, response: &mut worker::Response) {
        if let Err(error) = response
            .headers_mut()
            .set(INSTANCE_HEADER, &self.instance())
        {
            worker::console_log!("ip-header-unset {} {error}", self.instance());
        }
    }
}

/// The executor diagnostics for one request, taken on its first poll.
///
/// `None`, having touched nothing, unless the environment turns them on. When
/// it does, this logs the `ip-poll` line and returns the identities the
/// response is stamped with.
pub(crate) fn on_first_poll(
    environment: &CfEnvironment,
    request: &worker::Request,
) -> Option<RequestTrace> {
    if !environment.executor_trace_enabled() {
        return None;
    }
    let trace =
        WASM_INSTANCE.with(|instance| trace_request(&js_sys::global(), instance, request.inner()));
    worker::console_log!("{}", trace.poll_line());
    Some(trace)
}

/// This Wasm instance's `<isolate>/<wasm>/<generation>`, as
/// [`RequestTrace::instance`] spells it, once a traced request or the entry
/// module's probe has drawn its identity. `None`, having touched no JS
/// global, on an instance that has not: with the var off that is every
/// instance. For lines logged outside a request's first poll
/// (`request_services`'s `ip-root-found-stale`).
pub(crate) fn traced_instance() -> Option<String> {
    WASM_INSTANCE.with(|instance| traced_instance_in(&js_sys::global(), instance))
}

/// [`traced_instance`]'s work against an explicit global object and instance
/// cell, like [`trace_request`].
fn traced_instance_in(global: &Object, instance: &OnceCell<WasmInstance>) -> Option<String> {
    let instance = instance.get()?;
    let isolate = existing_state(global)
        .and_then(|state| Reflect::get(&state, &JsValue::from_str(ISOLATE_ID_FIELD)).ok())
        .and_then(|value| value.as_string());
    Some(format!(
        "{}/{}/{}",
        isolate.as_deref().unwrap_or("-"),
        instance.id,
        instance.generation,
    ))
}

/// The identity of the Wasm instance serving the current request, for the
/// entry module's `ip-entry` line: `{ wasm, generation, stack }`, or
/// `undefined` when no traced request has run in this isolate.
///
/// Synchronous and executor-free: it answers on an instance whose task queue
/// is wedged. Called on a fresh (or freshly reset) instance it draws that
/// instance's id, exactly as [`on_first_poll`] would have.
#[wasm_bindgen(js_name = __impresspressExecutorProbe)]
pub fn executor_probe() -> JsValue {
    WASM_INSTANCE.with(|instance| probe(&js_sys::global(), instance))
}

/// [`executor_probe`]'s work against an explicit global object and instance
/// cell, like [`trace_request`].
fn probe(global: &Object, instance: &OnceCell<WasmInstance>) -> JsValue {
    let Some(state) = existing_state(global) else {
        return JsValue::UNDEFINED;
    };
    let instance = instance.get_or_init(|| WasmInstance::start(&state));
    let answer = Object::new();
    set_field(&answer, PROBE_WASM_FIELD, &JsValue::from_str(&instance.id));
    set_field(
        &answer,
        PROBE_GENERATION_FIELD,
        &JsValue::from(instance.generation),
    );
    // A JS number holds every wasm32 address exactly.
    set_field(
        &answer,
        PROBE_STACK_FIELD,
        &JsValue::from(stack_address() as f64),
    );
    answer.into()
}

/// [`on_first_poll`]'s work against an explicit global object and instance
/// cell, so tests can stand in a fresh isolate and a fresh Wasm instance.
fn trace_request(
    global: &Object,
    instance: &OnceCell<WasmInstance>,
    request: &Object,
) -> RequestTrace {
    let state = shared_state(global);
    let instance = instance.get_or_init(|| WasmInstance::start(&state));
    RequestTrace {
        isolate: Reflect::get(&state, &JsValue::from_str(ISOLATE_ID_FIELD))
            .ok()
            .and_then(|value| value.as_string()),
        wasm: instance.id.clone(),
        generation: instance.generation,
        seq: request_seq(&state, request),
        stack: stack_address(),
    }
}

/// The shared state object, if a traced request has created it.
fn existing_state(global: &Object) -> Option<Object> {
    Reflect::get(global, &JsValue::from_str(STATE_GLOBAL))
        .ok()
        .and_then(|value| value.dyn_into::<Object>().ok())
}

/// The shared state object, created on `global` when the entry module has
/// not created it (a Worker whose `main` is worker-build's shim directly).
fn shared_state(global: &Object) -> Object {
    existing_state(global).unwrap_or_else(|| {
        let state = Object::new();
        set_field(global, STATE_GLOBAL, &state);
        state
    })
}

/// The number the entry module gave `request`, if it saw it.
fn request_seq(state: &Object, request: &Object) -> Option<u32> {
    let requests = Reflect::get(state, &JsValue::from_str(REQUESTS_FIELD))
        .ok()?
        .dyn_into::<WeakMap>()
        .ok()?;
    as_count(&requests.get(request))
}

fn count_field(target: &Object, field: &str) -> Option<u32> {
    as_count(&Reflect::get(target, &JsValue::from_str(field)).ok()?)
}

/// A JS number that is a non-negative integer in `u32` range.
fn as_count(value: &JsValue) -> Option<u32> {
    let number = value.as_f64()?;
    // In range and integral, so the cast is exact.
    (number.fract() == 0.0 && (0.0..=f64::from(u32::MAX)).contains(&number))
        .then_some(number as u32)
}

/// A property write on a plain JS object. It cannot fail on the objects this
/// module writes to (the global object and the objects it creates); were it
/// to, the next line shows the unchanged value, which is the diagnostic's
/// whole job to show, so the failure is not raised into the request.
fn set_field(target: &Object, field: &str, value: &JsValue) {
    if Reflect::set(target, &JsValue::from_str(field), value).is_err() {
        worker::console_log!("ip-trace-state-unset {field}");
    }
}

/// The test binary's real global as an isolate that has served no traced
/// request: [`take`](Self::take) removes the shared state, and dropping the
/// guard puts back whatever was there. Tests share one global, so without it
/// a traced test that ran first would leave state behind and an untraced
/// test could not tell whether its own path created any.
#[cfg(test)]
pub(crate) struct NoSharedState {
    previous: Option<Object>,
}

#[cfg(test)]
impl NoSharedState {
    pub(crate) fn take() -> Self {
        let global = js_sys::global();
        let previous = existing_state(&global);
        Reflect::delete_property(&global, &JsValue::from_str(STATE_GLOBAL))
            .expect("delete a property of the global object");
        Self { previous }
    }

    /// Whether a traced request has created the shared state since
    /// [`take`](Self::take).
    pub(crate) fn created_since() -> bool {
        existing_state(&js_sys::global()).is_some()
    }
}

#[cfg(test)]
impl Drop for NoSharedState {
    fn drop(&mut self) {
        let global = js_sys::global();
        let key = JsValue::from_str(STATE_GLOBAL);
        match &self.previous {
            Some(state) => Reflect::set(&global, &key, state).map(drop),
            None => Reflect::delete_property(&global, &key).map(drop),
        }
        .expect("restore the global object's shared state");
    }
}

#[cfg(test)]
mod tests {
    use wasm_bindgen_test::wasm_bindgen_test;

    use super::*;
    use crate::environment::test_support::empty_environment;

    /// The state object as the generated entry module leaves it after numbering
    /// `request` as `seq`: an isolate id and the map. The generation counter
    /// is the Rust side's; a test that needs a prior count sets it itself.
    fn state_as_entry_module_leaves_it(global: &Object, isolate: &str, request: &Object, seq: u32) {
        let state = Object::new();
        let requests = WeakMap::new();
        requests.set(request, &JsValue::from(seq));
        Reflect::set(&state, &ISOLATE_ID_FIELD.into(), &isolate.into()).unwrap();
        Reflect::set(&state, &REQUESTS_FIELD.into(), &requests).unwrap();
        Reflect::set(global, &STATE_GLOBAL.into(), &state).unwrap();
    }

    fn global_state(global: &Object) -> Object {
        existing_state(global).expect("the shared state exists")
    }

    fn probe_field(answer: &JsValue, field: &str) -> JsValue {
        Reflect::get(answer, &field.into()).unwrap()
    }

    #[wasm_bindgen_test]
    fn the_var_is_off_unless_it_is_exactly_one() {
        let mut environment = empty_environment();
        assert!(!environment.executor_trace_enabled(), "unset is off");
        for value in ["0", "true", "yes", " 1", "1 ", ""] {
            environment.set_executor_trace_for_test(value);
            assert!(
                !environment.executor_trace_enabled(),
                "{value:?} must not turn the trace on: the entry module tests \
                 for exactly \"1\", and the two sides must agree",
            );
        }
        environment.set_executor_trace_for_test("1");
        assert!(environment.executor_trace_enabled());
    }

    /// Off, the request path sees nothing: no trace, so no header, and the
    /// JS global the entry module and the Rust side share is never created.
    #[wasm_bindgen_test]
    fn off_it_touches_no_global_and_yields_no_trace() {
        let _clean = NoSharedState::take();
        let request = worker::Request::new("https://example.test/", worker::Method::Get).unwrap();

        assert_eq!(on_first_poll(&empty_environment(), &request), None);
        assert!(
            !NoSharedState::created_since(),
            "an untraced request must not create the shared state",
        );
    }

    #[wasm_bindgen_test]
    fn on_it_reads_the_entry_modules_identities_and_counts_this_instance() {
        let global = Object::new();
        let request = Object::new();
        state_as_entry_module_leaves_it(&global, "isolate-a", &request, 7);
        let instance = OnceCell::new();

        let trace = trace_request(&global, &instance, &request);

        assert_eq!(trace.isolate.as_deref(), Some("isolate-a"));
        assert_eq!(trace.generation, 1, "the first instance is generation 1");
        assert_eq!(trace.seq, Some(7));
        assert_eq!(trace.wasm.len(), 32, "a simple-form uuid: {}", trace.wasm);
        assert_eq!(
            count_field(&global_state(&global), WASM_GENERATION_FIELD),
            Some(1),
            "the counter lives in JS, where a reset instance can read it",
        );
        assert_eq!(
            trace.poll_line(),
            format!("ip-poll isolate-a {} 1 7 {}", trace.wasm, trace.stack)
        );
        assert_eq!(trace.instance(), format!("isolate-a/{}/1", trace.wasm));
    }

    /// The stack field is a real shadow-stack address: non-zero, and the same
    /// for two measurements taken at the same depth on one instance, which is
    /// what lets a falling figure across requests read as a leak.
    #[wasm_bindgen_test]
    fn the_stack_figure_is_an_address_stable_at_one_depth() {
        let global = Object::new();
        let request = Object::new();
        state_as_entry_module_leaves_it(&global, "isolate-s", &request, 1);
        let instance = OnceCell::new();

        let first = trace_request(&global, &instance, &request);
        let second = trace_request(&global, &instance, &request);

        assert_ne!(first.stack, 0);
        assert_eq!(first.stack, second.stack);
        let line = first.poll_line();
        let figure = line.rsplit(' ').next().unwrap();
        assert_eq!(figure.parse::<usize>().unwrap(), first.stack, "{line}");
    }

    /// Later requests on the same instance keep its id and generation, and
    /// do not advance the JS counter: only a new instance does.
    #[wasm_bindgen_test]
    fn later_requests_on_one_instance_share_its_identity() {
        let global = Object::new();
        let first = Object::new();
        state_as_entry_module_leaves_it(&global, "isolate-b", &first, 1);
        let instance = OnceCell::new();
        let first_trace = trace_request(&global, &instance, &first);

        let second = Object::new();
        let requests: WeakMap = Reflect::get(&global_state(&global), &REQUESTS_FIELD.into())
            .unwrap()
            .dyn_into()
            .unwrap();
        requests.set(&second, &JsValue::from(2));
        let second_trace = trace_request(&global, &instance, &second);

        assert_eq!(second_trace.wasm, first_trace.wasm);
        assert_eq!(second_trace.generation, 1);
        assert_eq!(second_trace.seq, Some(2));
        assert_eq!(
            count_field(&global_state(&global), WASM_GENERATION_FIELD),
            Some(1)
        );
    }

    /// A re-instantiated module: same isolate (same JS global), fresh linear
    /// memory (a fresh instance cell). It draws a new id and generation 2.
    #[wasm_bindgen_test]
    fn a_reset_instance_is_the_next_generation_with_a_new_id() {
        let global = Object::new();
        let request = Object::new();
        state_as_entry_module_leaves_it(&global, "isolate-c", &request, 1);
        let before = trace_request(&global, &OnceCell::new(), &request);
        let after = trace_request(&global, &OnceCell::new(), &request);

        assert_eq!(before.generation, 1);
        assert_eq!(after.generation, 2);
        assert_ne!(after.wasm, before.wasm);
        assert_eq!(after.isolate.as_deref(), Some("isolate-c"));
    }

    /// Without the entry module (worker-build's shim as `main`) the Rust side
    /// still counts instances, and prints `-` for what only JS knows.
    #[wasm_bindgen_test]
    fn without_the_entry_module_the_js_only_fields_print_as_dashes() {
        let global = Object::new();
        let trace = trace_request(&global, &OnceCell::new(), &Object::new());

        assert_eq!(trace.isolate, None);
        assert_eq!(trace.seq, None);
        assert_eq!(trace.generation, 1);
        assert_eq!(
            trace.poll_line(),
            format!("ip-poll - {} 1 - {}", trace.wasm, trace.stack)
        );
        assert_eq!(trace.instance(), format!("-/{}/1", trace.wasm));
        assert_eq!(
            count_field(&global_state(&global), WASM_GENERATION_FIELD),
            Some(1),
            "the shared state is created where the entry module did not create it",
        );
    }

    /// Untraced, the probe is inert: whatever a service binding calls it
    /// with, it answers `undefined`, creates no state and draws no instance.
    #[wasm_bindgen_test]
    fn the_probe_answers_nothing_until_a_traced_request_created_the_state() {
        let global = Object::new();
        let instance = OnceCell::new();

        assert!(probe(&global, &instance).is_undefined());
        assert!(existing_state(&global).is_none(), "no state created");
        assert!(instance.get().is_none(), "no instance drawn");
    }

    /// The probe draws the serving instance's identity, so the `ip-entry` of
    /// the first request on a reset instance already names the new instance
    /// whether or not that request is ever polled; and the poll that follows
    /// on the same instance reports the same identity.
    #[wasm_bindgen_test]
    fn the_probe_names_the_serving_instance_and_the_poll_agrees() {
        let global = Object::new();
        let request = Object::new();
        state_as_entry_module_leaves_it(&global, "isolate-p", &request, 3);
        let old = OnceCell::new();
        let old_answer = probe(&global, &old);
        let reset = OnceCell::new();
        let reset_answer = probe(&global, &reset);

        assert_eq!(probe_field(&old_answer, PROBE_GENERATION_FIELD), 1);
        assert_eq!(
            probe_field(&reset_answer, PROBE_GENERATION_FIELD),
            2,
            "the reset instance's first probe is the next generation"
        );
        let reset_id = probe_field(&reset_answer, PROBE_WASM_FIELD)
            .as_string()
            .unwrap();
        assert_ne!(
            probe_field(&old_answer, PROBE_WASM_FIELD)
                .as_string()
                .unwrap(),
            reset_id
        );
        assert!(
            probe_field(&reset_answer, PROBE_STACK_FIELD)
                .as_f64()
                .unwrap()
                > 0.0
        );

        let poll = trace_request(&global, &reset, &request);
        assert_eq!(poll.wasm, reset_id);
        assert_eq!(poll.generation, 2);
        assert_eq!(
            count_field(&global_state(&global), WASM_GENERATION_FIELD),
            Some(2),
            "the poll did not draw a second time"
        );
    }

    #[wasm_bindgen_test]
    fn stamp_sets_the_instance_header() {
        let trace = RequestTrace {
            isolate: Some("i".into()),
            wasm: "w".into(),
            generation: 3,
            seq: Some(4),
            stack: 5,
        };
        let mut response = worker::Response::ok("body").unwrap();
        trace.stamp(&mut response);

        assert_eq!(
            response.headers().get(INSTANCE_HEADER).unwrap().as_deref(),
            Some("i/w/3")
        );
    }

    /// An instance no traced request has reached has no instance field, and
    /// one that has carries exactly the header's value.
    #[wasm_bindgen_test]
    fn the_traced_instance_is_known_only_once_a_traced_request_drew_it() {
        let global = Object::new();
        let request = Object::new();
        state_as_entry_module_leaves_it(&global, "isolate-t", &request, 1);
        let instance = OnceCell::new();
        assert_eq!(traced_instance_in(&global, &instance), None);

        let trace = trace_request(&global, &instance, &request);

        assert_eq!(
            traced_instance_in(&global, &instance),
            Some(trace.instance())
        );
    }

    #[wasm_bindgen_test]
    fn only_non_negative_integers_in_range_are_counts() {
        assert_eq!(as_count(&JsValue::from(0)), Some(0));
        assert_eq!(as_count(&JsValue::from(41)), Some(41));
        assert_eq!(as_count(&JsValue::from(1.5)), None);
        assert_eq!(as_count(&JsValue::from(-1)), None);
        assert_eq!(as_count(&JsValue::from(f64::from(u32::MAX) + 1.0)), None);
        assert_eq!(as_count(&JsValue::from_str("3")), None);
        assert_eq!(as_count(&JsValue::UNDEFINED), None);
    }
}
