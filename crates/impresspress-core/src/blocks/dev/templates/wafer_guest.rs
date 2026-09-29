//! `wafer_guest.rs` — the ImpressPress guest runtime for std-only blocks.
//!
//! **VENDORED — do not edit.** `dev_create_block` writes this file verbatim
//! into every scaffolded block, `GET /b/dev/api/reference` documents its API,
//! and [`WAFER_GUEST_VERSION`] is what a staged build reports so the sandbox
//! can tell a block compiled against a stale copy from a current one. An
//! edited copy is not a supported configuration: the next scaffold overwrites
//! it, and the version it reports would be a lie.
//!
//! # Why it is vendored rather than a crate
//!
//! The sandbox's compiler is a browser toolchain (Rubrc) with **no registry
//! access**: it can build `core` + `std` and nothing else. A block's
//! `Cargo.toml` therefore has an empty `[dependencies]` table, and the only
//! way to share code with it is to put the code in the crate. This file is
//! that shared code — the whole SDK, in one module, written to compile with
//! no crates, no proc macros and no build script.
//!
//! # What it is
//!
//! The host speaks two wire contracts to a guest and this module implements
//! both:
//!
//! * the **core ABI** (`__wafer_alloc` / `__wafer_info` / `__wafer_handle` /
//!   `__wafer_lifecycle`), at v1 — JSON frames, bodies as integer arrays.
//!   v1 is the implicit version: a guest that exports no
//!   `__wafer_abi_version` speaks it, which is exactly what a guest with no
//!   MessagePack encoder can do.
//! * the **host-call codec**, negotiated as JSON by exporting
//!   `__wafer_host_codec() -> 1`. The host then transcodes every request
//!   body this module writes and every response frame it reads, so
//!   `database`/`storage`/`config` are reachable without an encoder.
//!
//! Both halves are pinned against `wafer-run`'s own std-only fixture guest
//! (`crates/wafer-run/tests/json_host_guest`) and its end-to-end test; the
//! conventions here are copied from it rather than invented.
//!
//! # What a block author writes
//!
//! Two functions in `src/lib.rs`, and nothing else is required:
//!
//! ```ignore
//! pub fn block() -> Block;                       // the block's declaration
//! pub fn init(ctx: &Ctx) -> Result<(), String>;  // runs once, on Init
//! ```
//!
//! The `#[no_mangle]` exports below call them. `block()` is what the sandbox
//! validates (its name, its routes, and the capabilities its collections /
//! storage folders / config keys imply), so the declaration and the code are
//! one artifact — there is no separate manifest to keep in step.

#![expect(
    dead_code,
    reason = "this is the whole guest ABI; a block uses the part it needs"
)]

/// ABI version of this vendored module.
///
/// Bumped whenever the guest↔host contract this file implements changes in a
/// way that makes an already-compiled block wrong. The staging endpoint
/// compares it against the sandbox's own
/// `impresspress_core::blocks::dev::WAFER_GUEST_VERSION` and refuses a
/// mismatch with a `wafer-guest-version` diagnostic — a block compiled
/// against an older copy is rebuilt, not silently activated.
pub const WAFER_GUEST_VERSION: u32 = 2;

/// The `BlockInfo::interface` every sandboxed block reports.
///
/// One value, because a guest serves HTTP requests through `__wafer_handle`
/// and nothing else; there is no second shape for an author to pick wrongly.
pub const INTERFACE: &str = "http-handler@v1";

/// Block id of the database service.
pub const DATABASE: &str = "wafer-run/database";
/// Block id of the object-storage service.
pub const STORAGE: &str = "wafer-run/storage";
/// Block id of the configuration service.
pub const CONFIG: &str = "wafer-run/config";

// ---------------------------------------------------------------------------
// Host imports
// ---------------------------------------------------------------------------

// The streaming host-call ABI, plus the log sink. (A `///` comment here
// would be an `unused_doc_comments` warning — rustdoc documents no extern
// block — and vendored code must not warn in a block author's build.)
//
// One `stream_init` -> `write_chunk`* -> `finish` -> `read_chunk`* ->
// `take_error` -> `close` cycle is one host call. `read_chunk` and
// `take_error` hand back a packed `(ptr << 32) | len` naming memory the host
// wrote through `__wafer_alloc`; a negative value is an `ErrorCode` sentinel,
// and `0` from `read_chunk` is end of stream.
#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "wafer")]
extern "C" {
    fn __wafer_host_log(level_ptr: i32, level_len: i32, msg_ptr: i32, msg_len: i32);
    fn __wafer_host_stream_init(name_ptr: i32, name_len: i32, msg_ptr: i32, msg_len: i32) -> i64;
    fn __wafer_host_stream_write_chunk(handle: i64, ptr: i32, len: i32) -> i32;
    fn __wafer_host_stream_finish(handle: i64) -> i32;
    fn __wafer_host_stream_read_chunk(handle: i64) -> i64;
    fn __wafer_host_stream_take_error(handle: i64) -> i64;
    fn __wafer_host_stream_close(handle: i64);
}

/// Host-call stand-ins for a native build.
///
/// The sandbox's own parity test compiles this file for the host, so that the
/// JSON it *renders* can be parsed by the real `wafer_block` types. There is
/// no host to call there, so every import panics rather than returning a
/// plausible-looking zero: a test that reached one would otherwise pass while
/// asserting nothing. Logging is the exception — it is a side effect with no
/// result, so dropping it changes no observable behaviour.
#[cfg(not(target_arch = "wasm32"))]
mod host_shim {
    /// Discard a log line; there is no host sink on a native build.
    ///
    /// # Safety
    /// Takes no pointers it dereferences; `unsafe` only to match the wasm32
    /// import's signature.
    pub unsafe fn __wafer_host_log(_: i32, _: i32, _: i32, _: i32) {}

    /// # Safety
    /// Never returns; see the module docs.
    pub unsafe fn __wafer_host_stream_init(_: i32, _: i32, _: i32, _: i32) -> i64 {
        panic!("host calls need wasm32")
    }

    /// # Safety
    /// Never returns; see the module docs.
    pub unsafe fn __wafer_host_stream_write_chunk(_: i64, _: i32, _: i32) -> i32 {
        panic!("host calls need wasm32")
    }

    /// # Safety
    /// Never returns; see the module docs.
    pub unsafe fn __wafer_host_stream_finish(_: i64) -> i32 {
        panic!("host calls need wasm32")
    }

    /// # Safety
    /// Never returns; see the module docs.
    pub unsafe fn __wafer_host_stream_read_chunk(_: i64) -> i64 {
        panic!("host calls need wasm32")
    }

    /// # Safety
    /// Never returns; see the module docs.
    pub unsafe fn __wafer_host_stream_take_error(_: i64) -> i64 {
        panic!("host calls need wasm32")
    }

    /// # Safety
    /// Does nothing; `unsafe` only to match the wasm32 import's signature.
    pub unsafe fn __wafer_host_stream_close(_: i64) {}
}

#[cfg(not(target_arch = "wasm32"))]
use host_shim::*;

// ---------------------------------------------------------------------------
// ABI exports
// ---------------------------------------------------------------------------

/// The four exports the host calls, plus the two it negotiates on.
///
/// wasm32 only: on the host there is no linear memory to hand back and no
/// `crate::block()` to read (the parity test includes this file as a plain
/// module, not as a block crate's root).
#[cfg(target_arch = "wasm32")]
mod abi {
    use super::{dispatch, json, render_block_info, render_result, Ctx, Request, Response};

    /// Pack a slice as the `(ptr << 32) | len` the host unpacks.
    fn pack(bytes: &[u8]) -> i64 {
        ((bytes.as_ptr() as u32 as i64) << 32) | bytes.len() as i64
    }

    /// Leak a `String` so the pointer handed to the host outlives the call.
    ///
    /// Deliberate: the host reads the bytes *after* the export returns, so
    /// nothing here may free them. A guest instance is short-lived (the
    /// runtime builds a fresh one per call unless the block declares a
    /// state-retaining mode), so the leak is bounded by one request.
    fn leak(s: String) -> &'static [u8] {
        Box::leak(s.into_boxed_str()).as_bytes()
    }

    /// Allocate `size` bytes for the host to write a frame into.
    #[no_mangle]
    pub extern "C" fn __wafer_alloc(size: i32) -> i32 {
        Box::leak(vec![0u8; size.max(0) as usize].into_boxed_slice()).as_mut_ptr() as i32
    }

    /// Negotiate the JSON host-call codec
    /// (`wafer_block::abi::HOST_CODEC_JSON`).
    #[no_mangle]
    pub extern "C" fn __wafer_host_codec() -> i32 {
        1
    }

    /// Report the block's `BlockInfo`, rendered from `crate::block()`.
    #[no_mangle]
    pub extern "C" fn __wafer_info() -> i64 {
        pack(leak(render_block_info(&crate::block())))
    }

    /// Serve one request: decode the frame, route it, render the result.
    #[no_mangle]
    pub extern "C" fn __wafer_handle(ptr: i32, len: i32) -> i64 {
        let frame = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };
        let response = match Request::from_frame(frame) {
            Ok(request) => dispatch(&crate::block(), &request),
            Err(detail) => Response::text(400, &format!("bad request frame: {detail}")),
        };
        pack(leak(render_result(&response)))
    }

    /// Run `crate::init` on `Init`, and nothing on the other transitions.
    ///
    /// The wire shape is `Result<(), WaferError>` in the v1 core ABI —
    /// `{"Ok":null}` or `{"Err":{"code":…,"message":…,"meta":[]}}` — which is
    /// serde's external tagging of a `Result`. An `Err` here fails the whole
    /// activation, which is the point: a block whose `init` could not create
    /// its tables must not start serving.
    #[no_mangle]
    pub extern "C" fn __wafer_lifecycle(ptr: i32, len: i32) -> i64 {
        let event = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };
        let is_init = json::Json::parse(&String::from_utf8_lossy(event))
            .ok()
            .and_then(|parsed| {
                parsed
                    .get("event_type")
                    .and_then(|kind| kind.as_str().map(|kind| kind == "Init"))
            })
            .unwrap_or(false);
        let out = if is_init {
            match crate::init(&Ctx) {
                Ok(()) => r#"{"Ok":null}"#.to_string(),
                Err(message) => format!(
                    r#"{{"Err":{{"code":"Internal","message":{},"meta":[]}}}}"#,
                    json::escape(&message)
                ),
            }
        } else {
            r#"{"Ok":null}"#.to_string()
        };
        pack(leak(out))
    }
}

// ---------------------------------------------------------------------------
// JSON
// ---------------------------------------------------------------------------

/// A JSON value, its parser and its renderer.
///
/// Hand-written because `serde_json` is a crate and the sandbox's compiler
/// has none. It is a whole JSON implementation rather than a subset: the
/// bytes on the wire are produced by `serde_json` on the host side, so
/// anything less than the full grammar would fail on a value the host
/// legitimately sends (a `\u` escape in a user's name, a float in a numeric
/// column).
pub mod json {
    /// A JSON value.
    ///
    /// `Obj` is a `Vec` of pairs, not a map: insertion order is preserved
    /// (so a rendered request reads the way it was written), duplicate keys
    /// are impossible through [`Json::set`], and a guest has no allocator
    /// pressure to spare on a `BTreeMap`.
    #[derive(Clone, Debug, PartialEq)]
    pub enum Json {
        /// `null`.
        Null,
        /// `true` / `false`.
        Bool(bool),
        /// A number. JSON has one numeric type; [`Json::as_i64`] is the
        /// integer view of it.
        Num(f64),
        /// A string, already unescaped.
        Str(String),
        /// An array.
        Arr(Vec<Json>),
        /// An object, in insertion order.
        Obj(Vec<(String, Json)>),
    }

    impl Json {
        /// An empty object, to be filled with [`Json::set`].
        pub fn obj() -> Json {
            Json::Obj(Vec::new())
        }

        /// A string value. Shorthand for `Json::Str(text.to_string())`,
        /// which is otherwise most of what building a request looks like.
        pub fn str(text: &str) -> Json {
            Json::Str(text.to_string())
        }

        /// An integer value.
        pub fn int(value: i64) -> Json {
            Json::Num(value as f64)
        }

        /// Set `k` to `v`, replacing any existing entry, and return `self`
        /// so calls chain. A no-op on a non-object.
        pub fn set(mut self, k: &str, v: Json) -> Json {
            if let Json::Obj(members) = &mut self {
                members.retain(|(key, _)| key != k);
                members.push((k.to_string(), v));
            }
            self
        }

        /// The value at `k`, or `None` on a non-object or a missing key.
        pub fn get(&self, k: &str) -> Option<&Json> {
            if let Json::Obj(members) = self {
                members
                    .iter()
                    .find(|(key, _)| key == k)
                    .map(|(_, value)| value)
            } else {
                None
            }
        }

        /// This value as a string, or `None` if it is not one.
        pub fn as_str(&self) -> Option<&str> {
            if let Json::Str(text) = self {
                Some(text)
            } else {
                None
            }
        }

        /// This value as a float, or `None` if it is not a number.
        pub fn as_f64(&self) -> Option<f64> {
            if let Json::Num(number) = self {
                Some(*number)
            } else {
                None
            }
        }

        /// This value as an integer — a number with no fractional part.
        /// A `1.5` is `None` rather than a silently truncated `1`.
        pub fn as_i64(&self) -> Option<i64> {
            self.as_f64()
                .filter(|number| number.fract() == 0.0)
                .map(|number| number as i64)
        }

        /// This value as a bool, or `None` if it is not one.
        pub fn as_bool(&self) -> Option<bool> {
            if let Json::Bool(value) = self {
                Some(*value)
            } else {
                None
            }
        }

        /// This value's items, or `None` if it is not an array.
        pub fn as_array(&self) -> Option<&[Json]> {
            if let Json::Arr(items) = self {
                Some(items)
            } else {
                None
            }
        }

        /// Parse one complete JSON document.
        ///
        /// Trailing data is an error rather than being ignored: a frame the
        /// host wrote is exactly one value, and anything after it means the
        /// guest read the wrong bytes.
        pub fn parse(text: &str) -> Result<Json, String> {
            let mut parser = Parser {
                s: text.as_bytes(),
                i: 0,
            };
            let value = parser.value()?;
            parser.ws();
            if parser.i != parser.s.len() {
                return Err(format!("trailing data at byte {}", parser.i));
            }
            Ok(value)
        }

        /// Render this value as compact JSON.
        pub fn render(&self) -> String {
            let mut out = String::new();
            render_into(self, &mut out);
            out
        }
    }

    /// Render `s` as a quoted JSON string, escapes included.
    ///
    /// Public because the ABI exports build two frames by hand — the
    /// lifecycle `Result` and the response meta — and both have to escape a
    /// message that came from block code.
    pub fn escape(s: &str) -> String {
        let mut out = String::with_capacity(s.len() + 2);
        out.push('"');
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                c => out.push(c),
            }
        }
        out.push('"');
        out
    }

    /// Append `v`'s rendering to `out`.
    fn render_into(v: &Json, out: &mut String) {
        match v {
            Json::Null => out.push_str("null"),
            Json::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
            // An integral number renders without a `.0` tail: the host's
            // `serde_json` decodes `3` as an integer and `3.0` as a float,
            // and a wire field typed `i64` refuses the latter.
            Json::Num(number) => {
                if !number.is_finite() {
                    // JSON has no `Infinity` and no `NaN`. The parser refuses
                    // to produce one, but block code can still build one by
                    // arithmetic (`1.0 / 0.0`), and `f64::to_string` would
                    // then write the bare token `inf` — which makes the WHOLE
                    // document unparseable to the host, losing the response
                    // rather than one field. `null` is the value every JSON
                    // decoder accepts for "no number here".
                    out.push_str("null");
                } else if number.fract() == 0.0 && number.abs() < 1e15 {
                    out.push_str(&(*number as i64).to_string());
                } else {
                    out.push_str(&number.to_string());
                }
            }
            Json::Str(text) => out.push_str(&escape(text)),
            Json::Arr(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    render_into(item, out);
                }
                out.push(']');
            }
            Json::Obj(members) => {
                out.push('{');
                for (i, (key, value)) in members.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&escape(key));
                    out.push(':');
                    render_into(value, out);
                }
                out.push('}');
            }
        }
    }

    /// A recursive-descent JSON parser over the document's bytes.
    struct Parser<'a> {
        /// The document.
        s: &'a [u8],
        /// Read cursor, in bytes.
        i: usize,
    }

    impl Parser<'_> {
        /// A parse error naming the byte it was found at.
        fn err<T>(&self, what: &str) -> Result<T, String> {
            Err(format!("{what} at byte {}", self.i))
        }

        /// Skip JSON whitespace.
        fn ws(&mut self) {
            while self.i < self.s.len() && matches!(self.s[self.i], b' ' | b'\n' | b'\r' | b'\t') {
                self.i += 1;
            }
        }

        /// Parse one value.
        fn value(&mut self) -> Result<Json, String> {
            self.ws();
            match self.s.get(self.i) {
                Some(b'{') => self.object(),
                Some(b'[') => self.array(),
                Some(b'"') => Ok(Json::Str(self.string()?)),
                Some(b't') => self.lit("true", Json::Bool(true)),
                Some(b'f') => self.lit("false", Json::Bool(false)),
                Some(b'n') => self.lit("null", Json::Null),
                Some(_) => self.number(),
                None => self.err("unexpected end of input"),
            }
        }

        /// Parse one of the three bare-word literals.
        fn lit(&mut self, word: &str, value: Json) -> Result<Json, String> {
            if self.s[self.i..].starts_with(word.as_bytes()) {
                self.i += word.len();
                Ok(value)
            } else {
                self.err("bad literal")
            }
        }

        /// Parse a number.
        ///
        /// The span is taken greedily and handed to `f64::from_str`, which is
        /// stricter than the span rule — so `1e`, `--1` and `1.2.3` are
        /// errors even though every byte is in the set.
        fn number(&mut self) -> Result<Json, String> {
            let start = self.i;
            while self.i < self.s.len()
                && matches!(
                    self.s[self.i],
                    b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9'
                )
            {
                self.i += 1;
            }
            std::str::from_utf8(&self.s[start..self.i])
                .ok()
                .and_then(|text| text.parse::<f64>().ok())
                // `f64::from_str` answers `inf` for a literal too large to
                // represent (`1e999`). Accepting it would put a value in the
                // tree that JSON cannot express, and the renderer would turn
                // it back into `null` — a silent change of the host's data.
                // A number this codec cannot hold is a parse error instead.
                .filter(|number| number.is_finite())
                .map(Json::Num)
                .ok_or_else(|| format!("bad number at byte {start}"))
        }

        /// Parse a quoted string, unescaping as it goes.
        fn string(&mut self) -> Result<String, String> {
            self.i += 1; // opening quote
            let mut out = String::new();
            loop {
                let Some(&byte) = self.s.get(self.i) else {
                    return self.err("unterminated string");
                };
                self.i += 1;
                match byte {
                    b'"' => return Ok(out),
                    b'\\' => {
                        let Some(&escape) = self.s.get(self.i) else {
                            return self.err("bad escape");
                        };
                        self.i += 1;
                        match escape {
                            b'"' => out.push('"'),
                            b'\\' => out.push('\\'),
                            b'/' => out.push('/'),
                            b'b' => out.push('\u{8}'),
                            b'f' => out.push('\u{c}'),
                            b'n' => out.push('\n'),
                            b'r' => out.push('\r'),
                            b't' => out.push('\t'),
                            b'u' => {
                                let mut code = self.hex4()?;
                                // A high surrogate is only half a character;
                                // JSON encodes astral code points as a pair,
                                // and `char::from_u32` refuses either half on
                                // its own.
                                if (0xD800..0xDC00).contains(&code) {
                                    if self.s.get(self.i..self.i + 2) != Some(b"\\u") {
                                        return self.err("lone surrogate");
                                    }
                                    self.i += 2;
                                    let low = self.hex4()?;
                                    // Range-checked, not masked: `\\uD83D\\u0041`
                                    // is two legal escapes that are not a pair,
                                    // and folding the second one's bits into the
                                    // first would invent a code point the
                                    // document never contained.
                                    if !(0xDC00..=0xDFFF).contains(&low) {
                                        return self.err("expected a low surrogate");
                                    }
                                    code = 0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00);
                                }
                                out.push(
                                    char::from_u32(code).ok_or_else(|| {
                                        format!("bad code point at byte {}", self.i)
                                    })?,
                                );
                            }
                            _ => return self.err("bad escape"),
                        }
                    }
                    _ => {
                        // Copy one whole UTF-8 sequence: the length is in the
                        // lead byte, and `from_utf8` is what refuses a
                        // malformed one.
                        let width = match byte {
                            0x00..=0x7F => 1,
                            0xC0..=0xDF => 2,
                            0xE0..=0xEF => 3,
                            _ => 4,
                        };
                        let start = self.i - 1;
                        self.i = start + width;
                        let bytes = self
                            .s
                            .get(start..self.i)
                            .ok_or_else(|| "truncated utf-8".to_string())?;
                        out.push_str(
                            std::str::from_utf8(bytes).map_err(|_| "bad utf-8".to_string())?,
                        );
                    }
                }
            }
        }

        /// Read the four hex digits of a `\u` escape.
        fn hex4(&mut self) -> Result<u32, String> {
            let text = std::str::from_utf8(
                self.s
                    .get(self.i..self.i + 4)
                    .ok_or_else(|| "short \\u escape".to_string())?,
            )
            .map_err(|_| "bad \\u escape".to_string())?;
            self.i += 4;
            u32::from_str_radix(text, 16).map_err(|_| "bad \\u escape".to_string())
        }

        /// Parse an array.
        fn array(&mut self) -> Result<Json, String> {
            self.i += 1;
            let mut items = Vec::new();
            self.ws();
            if self.s.get(self.i) == Some(&b']') {
                self.i += 1;
                return Ok(Json::Arr(items));
            }
            loop {
                items.push(self.value()?);
                self.ws();
                match self.s.get(self.i) {
                    Some(b',') => self.i += 1,
                    Some(b']') => {
                        self.i += 1;
                        return Ok(Json::Arr(items));
                    }
                    _ => return self.err("expected `,` or `]`"),
                }
            }
        }

        /// Parse an object.
        fn object(&mut self) -> Result<Json, String> {
            self.i += 1;
            let mut members = Vec::new();
            self.ws();
            if self.s.get(self.i) == Some(&b'}') {
                self.i += 1;
                return Ok(Json::Obj(members));
            }
            loop {
                self.ws();
                if self.s.get(self.i) != Some(&b'"') {
                    return self.err("expected a key");
                }
                let key = self.string()?;
                self.ws();
                if self.s.get(self.i) != Some(&b':') {
                    return self.err("expected `:`");
                }
                self.i += 1;
                let value = self.value()?;
                members.push((key, value));
                self.ws();
                match self.s.get(self.i) {
                    Some(b',') => self.i += 1,
                    Some(b'}') => {
                        self.i += 1;
                        return Ok(Json::Obj(members));
                    }
                    _ => return self.err("expected `,` or `}`"),
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Schemas
// ---------------------------------------------------------------------------

/// A JSON Schema fragment, built by chaining.
///
/// Attached to an endpoint with [`Endpoint::input`] / [`Endpoint::output`],
/// it becomes the endpoint's published contract: `/openapi.json` renders it,
/// and an agent tool built from the endpoint uses it as the tool's
/// `inputSchema`. So a schema is not documentation — it is what an agent
/// reads to decide how to call the block.
#[derive(Clone, Debug, PartialEq)]
pub struct Schema {
    /// The fragment, as it will be rendered.
    node: json::Json,
}

impl Schema {
    /// An object schema with no properties yet.
    pub fn object() -> Schema {
        Schema {
            node: json::Json::obj()
                .set("type", json::Json::str("object"))
                .set("properties", json::Json::obj()),
        }
    }

    /// A string schema.
    pub fn string() -> Schema {
        Schema::of("string")
    }

    /// An integer schema.
    pub fn integer() -> Schema {
        Schema::of("integer")
    }

    /// A number (possibly fractional) schema.
    pub fn number() -> Schema {
        Schema::of("number")
    }

    /// A boolean schema.
    pub fn boolean() -> Schema {
        Schema::of("boolean")
    }

    /// An array schema whose items match `items`.
    pub fn array(items: Schema) -> Schema {
        Schema {
            node: json::Json::obj()
                .set("type", json::Json::str("array"))
                .set("items", items.node),
        }
    }

    /// A string schema restricted to `values`.
    pub fn enum_of(values: &[&str]) -> Schema {
        Schema {
            node: json::Json::obj()
                .set("type", json::Json::str("string"))
                .set(
                    "enum",
                    json::Json::Arr(values.iter().map(|value| json::Json::str(value)).collect()),
                ),
        }
    }

    /// Add a property to an object schema. A no-op on any other schema —
    /// `properties` only exists on the object node [`Schema::object`] builds.
    pub fn prop(mut self, name: &str, schema: Schema) -> Schema {
        let properties = self
            .node
            .get("properties")
            .cloned()
            .unwrap_or_else(json::Json::obj);
        self.node = self
            .node
            .set("properties", properties.set(name, schema.node));
        self
    }

    /// Mark `names` as required.
    pub fn required(mut self, names: &[&str]) -> Schema {
        self.node = self.node.set(
            "required",
            json::Json::Arr(names.iter().map(|name| json::Json::str(name)).collect()),
        );
        self
    }

    /// Attach a description. This is the text an agent reads for the field,
    /// so write it for the caller, not for the reader of the source.
    pub fn describe(mut self, text: &str) -> Schema {
        self.node = self.node.set("description", json::Json::str(text));
        self
    }

    /// A bare `{"type": …}` node.
    fn of(kind: &str) -> Schema {
        Schema {
            node: json::Json::obj().set("type", json::Json::str(kind)),
        }
    }

    /// The fragment as JSON.
    fn to_json(&self) -> json::Json {
        self.node.clone()
    }
}

// ---------------------------------------------------------------------------
// Requests and responses
// ---------------------------------------------------------------------------

/// The HTTP methods a block may declare.
///
/// Exactly the four `wafer_block::HttpMethod` carries. `PUT` is deliberately
/// absent: the host's endpoint type has no such variant, so a block that
/// declared one would fail validation with an unparseable `BlockInfo` rather
/// than serve anything. Use `PATCH` for a partial update and `POST` for a
/// replacement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    /// `GET`.
    Get,
    /// `POST`.
    Post,
    /// `PATCH`.
    Patch,
    /// `DELETE`.
    Delete,
}

impl Method {
    /// The wire spelling, which is also what a request's `http.method` meta
    /// carries — so routing compares one string against one string.
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Patch => "PATCH",
            Method::Delete => "DELETE",
        }
    }
}

/// The access tier the router enforces for an endpoint.
///
/// The router is the gate, not the handler: a `Public` endpoint is served to
/// anyone, and an `Admin` one never reaches the handler without the `admin`
/// role. A path under the block's prefix that no endpoint declares falls back
/// to `Authenticated`, so forgetting to declare an endpoint fails closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Auth {
    /// Anyone, signed in or not.
    Public,
    /// Any signed-in user.
    Authenticated,
    /// The `admin` role.
    Admin,
}

impl Auth {
    /// The wire spelling (`wafer_block::AuthLevel`'s lowercase serde form).
    pub fn as_str(self) -> &'static str {
        match self {
            Auth::Public => "public",
            Auth::Authenticated => "authenticated",
            Auth::Admin => "admin",
        }
    }
}

/// One decoded HTTP request.
///
/// Every field is already decoded: `query` and `headers` come from the
/// request's meta (the host splits and percent-decodes the URL), `params`
/// from the route template that matched, and `body` is the raw bytes.
#[derive(Clone, Debug, Default)]
pub struct Request {
    /// Uppercased HTTP method.
    pub method: String,
    /// Request path, without the query string.
    pub path: String,
    /// Path parameters captured by the matched route's `{name}` segments.
    pub params: Vec<(String, String)>,
    /// Decoded query parameters, in the order the host reported them.
    pub query: Vec<(String, String)>,
    /// Request headers, names lowercased by the host.
    pub headers: Vec<(String, String)>,
    /// Raw request body.
    pub body: Vec<u8>,
    /// The signed-in user's id, or `None` for an anonymous request.
    pub user_id: Option<String>,
    /// The signed-in user's email, when the deployment resolved one.
    pub user_email: Option<String>,
    /// The signed-in user's roles.
    pub roles: Vec<String>,
}

impl Request {
    /// Decode the `__wafer_handle` frame: `[Message, [byte, …]]`.
    ///
    /// Public so the sandbox's parity test can decode a frame it wrote by
    /// hand and compare it against what the host actually sends.
    pub fn from_frame(frame: &[u8]) -> Result<Request, String> {
        let text = std::str::from_utf8(frame).map_err(|e| format!("frame is not utf-8: {e}"))?;
        let parsed = json::Json::parse(text)?;
        let parts = parsed
            .as_array()
            .ok_or_else(|| "frame is not a [message, body] array".to_string())?;
        let message = parts
            .first()
            .ok_or_else(|| "frame carries no message".to_string())?;

        let mut request = Request {
            // The body is a JSON integer array — the v1 ABI's encoding of a
            // byte string. A non-integer entry is dropped rather than
            // failing the whole request: the host writes this field, so a
            // malformed one is a host bug and an empty body is the safer
            // reading of it.
            body: match parts.get(1) {
                Some(json::Json::Arr(bytes)) => bytes
                    .iter()
                    .filter_map(|byte| byte.as_i64())
                    .map(|byte| byte as u8)
                    .collect(),
                _ => Vec::new(),
            },
            ..Request::default()
        };

        if let Some(json::Json::Arr(entries)) = message.get("meta") {
            for entry in entries {
                let (Some(key), Some(value)) = (
                    entry.get("key").and_then(json::Json::as_str),
                    entry.get("value").and_then(json::Json::as_str),
                ) else {
                    continue;
                };
                match key {
                    "http.method" => request.method = value.to_string(),
                    "http.path" => request.path = value.to_string(),
                    // Present-but-empty is how the host spells "nobody is
                    // signed in", so it must read as `None` rather than as a
                    // user whose id happens to be the empty string.
                    "auth.user_id" => request.user_id = non_empty(value),
                    "auth.user_email" => request.user_email = non_empty(value),
                    "auth.user_roles" => {
                        request.roles = value
                            .split(',')
                            .filter(|role| !role.is_empty())
                            .map(str::to_string)
                            .collect();
                    }
                    _ => {
                        if let Some(name) = key.strip_prefix("http.query.") {
                            request.query.push((name.to_string(), value.to_string()));
                        } else if let Some(name) = key.strip_prefix("http.header.") {
                            request.headers.push((name.to_string(), value.to_string()));
                        }
                    }
                }
            }
        }

        // `kind` is `{METHOD}:{path}` and carries the same two values as the
        // meta above. It is the fallback, not the source: a message built by
        // something other than the HTTP boundary may have only the kind.
        let kind = message
            .get("kind")
            .and_then(json::Json::as_str)
            .unwrap_or("");
        let (kind_method, kind_path) = kind.split_once(':').unwrap_or(("", kind));
        if request.method.is_empty() {
            request.method = kind_method.to_string();
        }
        if request.path.is_empty() {
            request.path = kind_path.to_string();
        }
        Ok(request)
    }

    /// Parse the body as JSON.
    pub fn json(&self) -> Result<json::Json, String> {
        let text =
            std::str::from_utf8(&self.body).map_err(|e| format!("body is not utf-8: {e}"))?;
        json::Json::parse(text)
    }

    /// The path parameter `name`, captured by the route's `{name}` segment.
    pub fn param(&self, name: &str) -> Option<&str> {
        lookup(&self.params, name)
    }

    /// The query parameter `name`.
    pub fn query(&self, name: &str) -> Option<&str> {
        lookup(&self.query, name)
    }

    /// The header `name`. Names are lowercase on the wire.
    pub fn header(&self, name: &str) -> Option<&str> {
        lookup(&self.headers, name)
    }

    /// Whether the caller holds `role`.
    pub fn has_role(&self, role: &str) -> bool {
        self.roles.iter().any(|held| held == role)
    }
}

/// `Some(value)` unless `value` is empty.
fn non_empty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_string())
}

/// The first value stored under `name`.
fn lookup<'a>(pairs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    pairs
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

/// One HTTP response.
#[derive(Clone, Debug)]
pub struct Response {
    /// HTTP status code.
    pub status: u16,
    /// `Content-Type` of the body.
    pub content_type: String,
    /// Extra response headers.
    pub headers: Vec<(String, String)>,
    /// Response body.
    pub body: Vec<u8>,
}

impl Response {
    /// A JSON response.
    pub fn json(status: u16, value: &json::Json) -> Response {
        Response::bytes(status, "application/json", value.render().into_bytes())
    }

    /// A `text/plain` response.
    pub fn text(status: u16, text: &str) -> Response {
        Response::bytes(
            status,
            "text/plain; charset=utf-8",
            text.as_bytes().to_vec(),
        )
    }

    /// A response with an explicit content type.
    pub fn bytes(status: u16, content_type: &str, body: Vec<u8>) -> Response {
        Response {
            status,
            content_type: content_type.to_string(),
            headers: Vec::new(),
            body,
        }
    }

    /// Add a response header.
    pub fn header(mut self, key: &str, value: &str) -> Response {
        self.headers.push((key.to_string(), value.to_string()));
        self
    }
}

/// The handle a handler and `init` are given.
///
/// A unit type today: every host call goes through this module's imports,
/// which are global to the instance, so there is nothing per-request to
/// carry. It is a parameter all the same because it is the seam where
/// per-request authority would live if the host ever hands any over — a
/// signature that already takes it does not have to change.
pub struct Ctx;

/// A request handler.
///
/// A plain `fn` pointer, not a closure: a `Box<dyn Fn>` would need an
/// allocation per endpoint on a runtime whose whole memory budget is the
/// guest's linear memory, and a block's handlers are always free functions.
pub type Handler = fn(&Request, &Ctx) -> Response;

// ---------------------------------------------------------------------------
// The block declaration
// ---------------------------------------------------------------------------

/// One declared HTTP endpoint.
pub struct Endpoint {
    /// Method this endpoint answers.
    pub method: Method,
    /// Absolute path template, `{name}` for a path parameter.
    pub path: String,
    /// One-line summary, published in `/openapi.json`.
    pub summary: String,
    /// Access tier the router enforces.
    pub auth: Auth,
    /// Request body schema, if any.
    pub input: Option<Schema>,
    /// Response body schema, if any.
    pub output: Option<Schema>,
    /// Agent tool name and description, when the endpoint opts in.
    pub tool: Option<(String, String)>,
    /// The function that serves it.
    pub handler: Handler,
}

impl Endpoint {
    /// Declare an endpoint. `path` must sit under the block's own
    /// `/b/{name}/` prefix — the sandbox refuses a block that declares one
    /// outside it, because the router would never send it those requests.
    pub fn new(method: Method, path: &str, handler: Handler) -> Endpoint {
        Endpoint {
            method,
            path: path.to_string(),
            summary: String::new(),
            auth: Auth::Public,
            input: None,
            output: None,
            tool: None,
            handler,
        }
    }

    /// Set the access tier. Defaults to [`Auth::Public`].
    pub fn auth(mut self, auth: Auth) -> Endpoint {
        self.auth = auth;
        self
    }

    /// Set the one-line summary.
    pub fn summary(mut self, text: &str) -> Endpoint {
        self.summary = text.to_string();
        self
    }

    /// Declare the request body's schema.
    pub fn input(mut self, schema: Schema) -> Endpoint {
        self.input = Some(schema);
        self
    }

    /// Declare the response body's schema.
    pub fn output(mut self, schema: Schema) -> Endpoint {
        self.output = Some(schema);
        self
    }

    /// Expose this endpoint as an agent tool.
    ///
    /// `name` must be unique across the whole deployment — the sandbox
    /// refuses a block whose tool name another block already claims, because
    /// an MCP client shown two tools with one name drops one of them
    /// silently. `description` is what an agent reads to decide whether to
    /// call it, so name the side effect.
    pub fn agent_tool(mut self, name: &str, description: &str) -> Endpoint {
        self.tool = Some((name.to_string(), description.to_string()));
        self
    }
}

/// A block: its identity, the resources it claims, and the endpoints it
/// serves.
///
/// The claims are what become the block's capabilities. A block reaches
/// exactly the collections, storage folders and config keys it declares here
/// and nothing else — the sandbox turns the declaration into the capability
/// set the runtime enforces, so an undeclared table is a `PermissionDenied`
/// at run time rather than a silent read of somebody else's rows.
pub struct Block {
    /// Registered block id, always `site/{name}`.
    pub name: String,
    /// Semantic version. `0.1.0` unless [`Block::version`] says otherwise.
    pub version: String,
    /// One-line summary.
    pub summary: String,
    /// Platform services the block calls.
    pub requires: Vec<String>,
    /// Database collections it may reach (`site__{name}__*`, `-` as `_`).
    pub collections: Vec<String>,
    /// Storage folders it may reach (`site/{name}` and below).
    pub storage_folders: Vec<String>,
    /// Config keys it may read (`SITE__{NAME}__*`, `-` as `_`).
    pub config_keys: Vec<String>,
    /// Endpoints it serves, in declaration order — which is also route
    /// precedence.
    pub endpoints: Vec<Endpoint>,
}

impl Block {
    /// Declare a block. `name` is the registered id, `site/{short-name}`.
    pub fn new(name: &str, summary: &str) -> Block {
        Block {
            name: name.to_string(),
            version: "0.1.0".to_string(),
            summary: summary.to_string(),
            requires: Vec::new(),
            collections: Vec::new(),
            storage_folders: Vec::new(),
            config_keys: Vec::new(),
            endpoints: Vec::new(),
        }
    }

    /// Set the version reported in `BlockInfo`.
    pub fn version(mut self, version: &str) -> Block {
        self.version = version.to_string();
        self
    }

    /// Declare the platform services this block calls: any of
    /// [`DATABASE`], [`STORAGE`] and [`CONFIG`].
    ///
    /// Logging is not one of them, which is why there is no constant for it:
    /// [`log`] reaches the host directly (`__wafer_host_log`) rather than
    /// through a block call, so it is available without being declared.
    ///
    /// It is also the block's `callable_blocks` capability — the two are two
    /// spellings of one fact, so this module renders both from this one list
    /// and they cannot disagree.
    pub fn requires(mut self, blocks: &[&str]) -> Block {
        self.requires = blocks.iter().map(|block| (*block).to_string()).collect();
        self
    }

    /// Claim a database collection. Must be `site__{name}__{table}` with the
    /// block's own name, a hyphen in it spelled `_` (`my-shop` claims
    /// `site__my_shop__*`), and only lowercase letters, digits and `_`.
    ///
    /// Claiming one also turns on the `schema` capability, which is what lets
    /// [`db::ensure_table`] create it. Raw DDL is never granted.
    pub fn collection(mut self, name: &str) -> Block {
        self.collections.push(name.to_string());
        self
    }

    /// Claim a storage folder. Must be `site/{name}` or a folder under it.
    pub fn storage_folder(mut self, folder: &str) -> Block {
        self.storage_folders.push(folder.to_string());
        self
    }

    /// Claim a config key. Must start with `SITE__{NAME}__`, the block's own
    /// name uppercased with a hyphen spelled `_`, as in
    /// [`collection`](Block::collection), so `my-shop` claims
    /// `SITE__MY_SHOP__*`.
    pub fn config_key(mut self, key: &str) -> Block {
        self.config_keys.push(key.to_string());
        self
    }

    /// Add an endpoint.
    pub fn endpoint(mut self, endpoint: Endpoint) -> Block {
        self.endpoints.push(endpoint);
        self
    }

    /// Find the endpoint serving `method` `path`, with the path parameters
    /// its template captured.
    ///
    /// First declaration wins. Matching is per whole segment, so
    /// `/b/x/items/{id}` matches `/b/x/items/7` and never `/b/x/items/7/tags`
    /// — a prefix match would let one endpoint swallow another's routes.
    pub fn route(&self, method: &str, path: &str) -> Option<(&Endpoint, Vec<(String, String)>)> {
        for endpoint in &self.endpoints {
            if endpoint.method.as_str() != method {
                continue;
            }
            if let Some(params) = match_path(&endpoint.path, path) {
                return Some((endpoint, params));
            }
        }
        None
    }
}

/// Match `path` against a route `template`, capturing `{name}` segments.
fn match_path(template: &str, path: &str) -> Option<Vec<(String, String)>> {
    let expected: Vec<&str> = template.split('/').collect();
    let actual: Vec<&str> = path.split('/').collect();
    if expected.len() != actual.len() {
        return None;
    }
    let mut params = Vec::new();
    for (expected, actual) in expected.iter().zip(actual.iter()) {
        match expected
            .strip_prefix('{')
            .and_then(|name| name.strip_suffix('}'))
        {
            // An empty capture would make `/b/x/items/` match
            // `/b/x/items/{id}` with an id of `""`.
            Some(_) if actual.is_empty() => return None,
            Some(name) => params.push((name.to_string(), (*actual).to_string())),
            None if expected != actual => return None,
            None => {}
        }
    }
    Some(params)
}

/// Route one request to its handler.
///
/// A handler that panics aborts the instance (`panic = "abort"`), so the host
/// sees a trap and the request fails with a 500 — there is no unwinding to
/// catch. Return an error [`Response`] rather than panicking on anything a
/// caller can cause.
pub fn dispatch(block: &Block, request: &Request) -> Response {
    match block.route(&request.method, &request.path) {
        Some((endpoint, params)) => {
            let mut request = request.clone();
            request.params = params;
            (endpoint.handler)(&request, &Ctx)
        }
        None => Response::text(404, "not found"),
    }
}

// ---------------------------------------------------------------------------
// Rendering the two frames the host reads
// ---------------------------------------------------------------------------

/// Render a [`Response`] as the `GuestResult` the host decodes.
///
/// Public so the parity test can render one and parse it back with the real
/// `wafer_block::abi::GuestResult` — the only way to prove the two agree
/// without a runtime.
pub fn render_result(response: &Response) -> String {
    let data: Vec<json::Json> = response
        .body
        .iter()
        .map(|byte| json::Json::int(*byte as i64))
        .collect();
    let mut meta = vec![
        meta_entry("resp.status", &response.status.to_string()),
        meta_entry("resp.content_type", &response.content_type),
    ];
    for (key, value) in &response.headers {
        meta.push(meta_entry(&format!("resp.header.{key}"), value));
    }
    json::Json::obj()
        .set("action", json::Json::str("Respond"))
        .set(
            "response",
            json::Json::obj()
                .set("data", json::Json::Arr(data))
                .set("meta", json::Json::Arr(meta)),
        )
        // Both arms are `Option`s the host's decoder requires to be present,
        // so they are written as explicit nulls rather than omitted.
        .set("error", json::Json::Null)
        .set("message", json::Json::Null)
        .render()
}

/// One `MetaEntry`.
fn meta_entry(key: &str, value: &str) -> json::Json {
    json::Json::obj()
        .set("key", json::Json::str(key))
        .set("value", json::Json::str(value))
}

/// Render a [`Block`] as the `BlockInfo` the host parses from `__wafer_info`.
///
/// Every field the sandbox's validator reads is written out, and the
/// capability set is written **in full** — including the flags that are
/// always false. A capability the guest does not name is denied either way,
/// but spelling out `raw_sql: false` and `ddl: false` makes the block's whole
/// authority visible in one place, in the artifact itself.
pub fn render_block_info(block: &Block) -> String {
    let capabilities = json::Json::obj()
        .set("collections", allowlist(&block.collections))
        .set("raw_sql", json::Json::Bool(false))
        // Never: raw DDL runs an arbitrary statement, which no sandboxed
        // block is granted. `schema` below is the structured replacement.
        .set("ddl", json::Json::Bool(false))
        // The structured table ops, authorized against `collections` as well
        // as the schema sentinel — so a block can create its own tables and
        // reach nothing else. On whenever the block claims a collection,
        // because `init` creating that collection is what a claim is for.
        .set("schema", json::Json::Bool(!block.collections.is_empty()))
        .set("storage_folders", allowlist(&block.storage_folders))
        .set("config", allowlist(&block.config_keys))
        // The same list as `requires`: the two must name the same services,
        // and rendering both from one field is what keeps them equal.
        .set("callable_blocks", allowlist(&block.requires));

    let endpoints: Vec<json::Json> = block
        .endpoints
        .iter()
        .map(|endpoint| {
            let mut rendered = json::Json::obj()
                .set("method", json::Json::str(endpoint.method.as_str()))
                .set("path", json::Json::str(&endpoint.path))
                .set("summary", json::Json::str(&endpoint.summary))
                .set("auth", json::Json::str(endpoint.auth.as_str()));
            if let Some(schema) = &endpoint.input {
                rendered = rendered.set("input_schema", schema.to_json());
            }
            if let Some(schema) = &endpoint.output {
                rendered = rendered.set("output_schema", schema.to_json());
            }
            if let Some((name, description)) = &endpoint.tool {
                rendered = rendered.set(
                    "agent_tool",
                    json::Json::obj()
                        .set("name", json::Json::str(name))
                        .set("description", json::Json::str(description)),
                );
            }
            rendered
        })
        .collect();

    json::Json::obj()
        .set("name", json::Json::str(&block.name))
        .set("version", json::Json::str(&block.version))
        .set("interface", json::Json::str(INTERFACE))
        .set("summary", json::Json::str(&block.summary))
        .set("requires", strings(&block.requires))
        .set("capabilities", capabilities)
        .set("endpoints", json::Json::Arr(endpoints))
        .render()
}

/// Render a claim list as a `wafer_block::Allowlist`.
///
/// An empty list is `Allowlist::None` — deny everything — not an empty
/// `Only`, and never `Any`.
fn allowlist(entries: &[String]) -> json::Json {
    if entries.is_empty() {
        json::Json::str("None")
    } else {
        json::Json::obj().set("Only", strings(entries))
    }
}

/// Render a `&[String]` as a JSON array of strings.
fn strings(entries: &[String]) -> json::Json {
    json::Json::Arr(entries.iter().map(|entry| json::Json::str(entry)).collect())
}

// ---------------------------------------------------------------------------
// Host calls
// ---------------------------------------------------------------------------

/// A refusal from a platform service.
///
/// `code` is the `wafer_block::ErrorCode` variant name as the host spells it
/// — `NotFound`, `PermissionDenied`, `InvalidArgument`, `Internal`, … — so a
/// handler matches on it rather than on the message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostError {
    /// The host's error code, by name.
    pub code: String,
    /// What went wrong.
    pub message: String,
}

impl HostError {
    /// Build an error from a negative ABI sentinel.
    ///
    /// The streaming ABI reports a failure as `-(ordinal)` of the error code,
    /// which is the only signal available when the call never produced an
    /// error payload.
    fn sentinel(sentinel: i32, what: &str) -> HostError {
        HostError {
            code: error_code_name(-sentinel).to_string(),
            message: format!("{what} failed with host sentinel {sentinel}"),
        }
    }

    /// Decode the `{"code":…,"message":…}` payload `take_error` hands back.
    fn from_json(text: &str) -> HostError {
        match json::Json::parse(text) {
            Ok(parsed) => HostError {
                code: parsed
                    .get("code")
                    .and_then(json::Json::as_str)
                    .unwrap_or("Internal")
                    .to_string(),
                message: parsed
                    .get("message")
                    .and_then(json::Json::as_str)
                    .unwrap_or(text)
                    .to_string(),
            },
            // A payload that is not JSON is still the host's own words about
            // the failure, so it becomes the message rather than being lost.
            Err(_) => HostError {
                code: "Internal".to_string(),
                message: text.to_string(),
            },
        }
    }

    /// Build an error raised by this module rather than by the host.
    fn internal(message: String) -> HostError {
        HostError {
            code: "Internal".to_string(),
            message,
        }
    }
}

impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

/// `wafer_block::ErrorCode`'s ordinal → name table.
///
/// The ordinals are the streaming ABI's own wire mapping (`ErrorCode::
/// to_ordinal`), hand-carried because a guest has no `wafer_block` to read
/// them from. An ordinal outside the table is `Unknown` rather than a
/// panic — a future host code must not crash a block.
fn error_code_name(ordinal: i32) -> &'static str {
    match ordinal {
        0 => "Ok",
        1 => "Cancelled",
        3 => "InvalidArgument",
        4 => "DeadlineExceeded",
        5 => "NotFound",
        6 => "AlreadyExists",
        7 => "PermissionDenied",
        8 => "ResourceExhausted",
        9 => "FailedPrecondition",
        10 => "Aborted",
        11 => "OutOfRange",
        12 => "Unimplemented",
        13 => "Internal",
        14 => "Unavailable",
        15 => "DataLoss",
        16 => "Unauthenticated",
        _ => "Unknown",
    }
}

/// Reinterpret a host-returned `(ptr << 32) | len` as the bytes the host
/// wrote into linear memory.
///
/// # Safety
///
/// `packed` must be a positive value the host returned from `read_chunk` or
/// `take_error`. Those name allocations made through `__wafer_alloc`, which
/// are leaked, so the `'static` lifetime is real.
unsafe fn unpack(packed: i64) -> &'static [u8] {
    let ptr = (packed >> 32) as u32 as *const u8;
    let len = (packed & 0xffff_ffff) as usize;
    std::slice::from_raw_parts(ptr, len)
}

/// One buffered host call, with the response frames kept separate.
///
/// The boundary matters for exactly one op: `storage.get` answers with an
/// `ObjectInfo` header frame followed by the object's bytes **verbatim**, so
/// joining them first would leave the body wedged inside a JSON document.
/// Every other op answers with one frame, and [`call`] joins them.
pub fn call_frames(target: &str, kind: &str, body: &json::Json) -> Result<Vec<Vec<u8>>, HostError> {
    let message = json::Json::obj()
        .set("kind", json::Json::str(kind))
        .set("meta", json::Json::Arr(Vec::new()))
        .render();
    let body = body.render();
    unsafe {
        let handle = __wafer_host_stream_init(
            target.as_ptr() as i32,
            target.len() as i32,
            message.as_ptr() as i32,
            message.len() as i32,
        );
        if handle < 0 {
            return Err(HostError::sentinel(handle as i32, "stream_init"));
        }
        let written =
            __wafer_host_stream_write_chunk(handle, body.as_ptr() as i32, body.len() as i32);
        if written != 0 {
            __wafer_host_stream_close(handle);
            return Err(HostError::sentinel(written, "stream_write_chunk"));
        }
        let status = __wafer_host_stream_finish(handle);
        let mut frames: Vec<Vec<u8>> = Vec::new();
        if status == 0 {
            loop {
                // 0 ends the stream; a negative value is an error sentinel
                // whose detail arrives through `take_error` below.
                let packed = __wafer_host_stream_read_chunk(handle);
                if packed <= 0 {
                    break;
                }
                frames.push(unpack(packed).to_vec());
            }
        }
        let packed_error = __wafer_host_stream_take_error(handle);
        let error = if packed_error > 0 {
            Some(String::from_utf8_lossy(unpack(packed_error)).into_owned())
        } else {
            None
        };
        __wafer_host_stream_close(handle);
        // The error payload is authoritative: a WRAP denial or a backend
        // `NotFound` happens after dispatch, so `status` is still 0 and only
        // `take_error` knows.
        match error {
            Some(text) => Err(HostError::from_json(&text)),
            None if status != 0 => Err(HostError::sentinel(status, "stream_finish")),
            None => Ok(frames),
        }
    }
}

/// One buffered host call whose response is a single JSON document.
///
/// An empty response is [`json::Json::Null`] — several ops (`database.delete`,
/// `storage.put`) answer with no frame at all.
pub fn call(target: &str, kind: &str, body: &json::Json) -> Result<json::Json, HostError> {
    let joined: Vec<u8> = call_frames(target, kind, body)?.concat();
    if joined.is_empty() {
        return Ok(json::Json::Null);
    }
    let text = String::from_utf8_lossy(&joined).into_owned();
    json::Json::parse(&text)
        .map_err(|detail| HostError::internal(format!("{kind} answered with bad JSON: {detail}")))
}

// ---------------------------------------------------------------------------
// Schema definitions
// ---------------------------------------------------------------------------

/// One column of a [`TableDef`].
///
/// Columns are nullable by default; [`Column::not_null`] and
/// [`Column::primary_key`] are what tighten that. The default direction is
/// deliberate: adding a column to a table that already has rows can only
/// succeed if the column is nullable or carries a default.
#[derive(Clone, Debug)]
pub struct Column {
    /// Column name.
    pub name: String,
    /// Column type: one of `string`, `text`, `int`, `int64`, `float`,
    /// `bool`, `datetime`, `json`, `blob`.
    pub kind: String,
    /// Whether the column accepts `NULL`.
    pub nullable: bool,
    /// Whether the column is the table's primary key.
    pub primary_key: bool,
    /// Whether an integer primary key auto-increments.
    pub auto_increment: bool,
    /// Whether the column carries a `UNIQUE` constraint.
    pub unique: bool,
    /// Column default: `("null", …)`, `("now", …)` or `("value", literal)`.
    pub default: Option<(String, json::Json)>,
}

impl Column {
    /// A short string column (`VARCHAR`-shaped).
    pub fn string(name: &str) -> Column {
        Column::of(name, "string")
    }

    /// A long text column.
    pub fn text(name: &str) -> Column {
        Column::of(name, "text")
    }

    /// A 32-bit integer column.
    pub fn int(name: &str) -> Column {
        Column::of(name, "int")
    }

    /// A 64-bit integer column.
    pub fn int64(name: &str) -> Column {
        Column::of(name, "int64")
    }

    /// A floating-point column.
    pub fn float(name: &str) -> Column {
        Column::of(name, "float")
    }

    /// A boolean column.
    pub fn bool(name: &str) -> Column {
        Column::of(name, "bool")
    }

    /// A timestamp column.
    pub fn datetime(name: &str) -> Column {
        Column::of(name, "datetime")
    }

    /// A JSON-document column.
    pub fn json(name: &str) -> Column {
        Column::of(name, "json")
    }

    /// A binary column.
    pub fn blob(name: &str) -> Column {
        Column::of(name, "blob")
    }

    /// Make this the table's primary key.
    ///
    /// Also clears `nullable`: a primary key cannot be null, and letting the
    /// two disagree would emit a table definition the backend has to
    /// reconcile.
    pub fn primary_key(mut self) -> Column {
        self.primary_key = true;
        self.nullable = false;
        self
    }

    /// Forbid `NULL` in this column.
    pub fn not_null(mut self) -> Column {
        self.nullable = false;
        self
    }

    /// Add a `UNIQUE` constraint.
    pub fn unique(mut self) -> Column {
        self.unique = true;
        self
    }

    /// Auto-increment this integer primary key.
    pub fn auto_increment(mut self) -> Column {
        self.auto_increment = true;
        self
    }

    /// Default to the time the row is inserted.
    pub fn default_now(mut self) -> Column {
        self.default = Some(("now".to_string(), json::Json::Null));
        self
    }

    /// Default to a literal.
    pub fn default_value(mut self, value: json::Json) -> Column {
        self.default = Some(("value".to_string(), value));
        self
    }

    /// A nullable column of `kind`.
    fn of(name: &str, kind: &str) -> Column {
        Column {
            name: name.to_string(),
            kind: kind.to_string(),
            nullable: true,
            primary_key: false,
            auto_increment: false,
            unique: false,
            default: None,
        }
    }

    /// The wire `ColumnDef`.
    fn to_json(&self) -> json::Json {
        let mut rendered = json::Json::obj()
            .set("name", json::Json::str(&self.name))
            .set("kind", json::Json::str(&self.kind))
            .set("nullable", json::Json::Bool(self.nullable))
            .set("primary_key", json::Json::Bool(self.primary_key))
            .set("auto_increment", json::Json::Bool(self.auto_increment))
            .set("unique", json::Json::Bool(self.unique));
        if let Some((kind, value)) = &self.default {
            rendered = rendered.set(
                "default",
                json::Json::obj()
                    .set("kind", json::Json::str(kind))
                    .set("value", value.clone()),
            );
        }
        rendered
    }
}

/// A table for [`db::ensure_table`].
///
/// `name` must be one of the collections the block claimed with
/// [`Block::collection`]; the host authorizes the op against that claim as
/// well as against the schema capability, so an unclaimed table is a
/// `PermissionDenied`.
#[derive(Clone, Debug)]
pub struct TableDef {
    /// Table name (`site__{block}__{table}`).
    pub name: String,
    /// Columns, in declaration order.
    pub columns: Vec<Column>,
    /// Secondary indexes: the columns, and whether the index is unique.
    pub indexes: Vec<(Vec<String>, bool)>,
}

impl TableDef {
    /// An empty table definition.
    pub fn new(name: &str) -> TableDef {
        TableDef {
            name: name.to_string(),
            columns: Vec::new(),
            indexes: Vec::new(),
        }
    }

    /// Add a column.
    pub fn column(mut self, column: Column) -> TableDef {
        self.columns.push(column);
        self
    }

    /// Add a secondary index over `columns`. The host derives its name.
    pub fn index(mut self, columns: &[&str], unique: bool) -> TableDef {
        self.indexes.push((
            columns.iter().map(|column| (*column).to_string()).collect(),
            unique,
        ));
        self
    }

    /// The wire `TableDef`.
    fn to_json(&self) -> json::Json {
        json::Json::obj()
            .set("name", json::Json::str(&self.name))
            .set(
                "columns",
                json::Json::Arr(self.columns.iter().map(Column::to_json).collect()),
            )
            .set(
                "indexes",
                json::Json::Arr(
                    self.indexes
                        .iter()
                        .map(|(columns, unique)| {
                            json::Json::obj()
                                .set("name", json::Json::str(""))
                                .set("columns", strings(columns))
                                .set("unique", json::Json::Bool(*unique))
                        })
                        .collect(),
                ),
            )
    }
}

// ---------------------------------------------------------------------------
// Queries
// ---------------------------------------------------------------------------

/// One `WHERE` predicate: `field <operator> value`.
#[derive(Clone, Debug)]
pub struct Filter {
    /// Column to compare.
    pub field: String,
    /// One of `eq`, `neq`, `gt`, `gte`, `lt`, `lte`, `like`, `in`,
    /// `is_null`, `is_not_null`. Anything else is refused by the host.
    pub operator: String,
    /// Value to compare against. Ignored by `is_null` / `is_not_null`.
    pub value: json::Json,
}

impl Filter {
    /// Build a predicate.
    pub fn new(field: &str, operator: &str, value: json::Json) -> Filter {
        Filter {
            field: field.to_string(),
            operator: operator.to_string(),
            value,
        }
    }

    /// The wire `FilterDef`.
    fn to_json(&self) -> json::Json {
        json::Json::obj()
            .set("field", json::Json::str(&self.field))
            .set("operator", json::Json::str(&self.operator))
            .set("value", self.value.clone())
    }
}

/// Filters, sort order and paging for [`db::list`].
///
/// `limit` is unset by default, which returns every matching row. Set one
/// for anything a user can grow without bound. An `offset` needs a `limit`:
/// the host refuses a skip with no page size.
#[derive(Clone, Debug, Default)]
pub struct ListOptions {
    /// Predicates, combined with `AND`.
    pub filters: Vec<Filter>,
    /// Sort order: the column, and whether it is descending.
    pub sort: Vec<(String, bool)>,
    /// Maximum rows, at least 1; `None` returns every matching row.
    pub limit: Option<u32>,
    /// Rows to skip.
    pub offset: i64,
}

impl ListOptions {
    /// Unfiltered, unsorted, unpaged.
    pub fn new() -> ListOptions {
        ListOptions::default()
    }

    /// Add a predicate. See [`Filter::operator`] for the operators.
    pub fn filter(mut self, field: &str, operator: &str, value: json::Json) -> ListOptions {
        self.filters.push(Filter::new(field, operator, value));
        self
    }

    /// Sort by `field`, descending when `desc`.
    pub fn sort(mut self, field: &str, desc: bool) -> ListOptions {
        self.sort.push((field.to_string(), desc));
        self
    }

    /// Return at most `limit` rows; the host refuses `0`.
    pub fn limit(mut self, limit: u32) -> ListOptions {
        self.limit = Some(limit);
        self
    }

    /// Skip `offset` rows.
    pub fn offset(mut self, offset: i64) -> ListOptions {
        self.offset = offset;
        self
    }
}

/// Render a filter list as the wire `filters` array.
fn filters_json(filters: &[Filter]) -> json::Json {
    json::Json::Arr(filters.iter().map(Filter::to_json).collect())
}

// ---------------------------------------------------------------------------
// The platform services
// ---------------------------------------------------------------------------

/// The database service (`wafer-run/database`).
///
/// Every op names a **collection**, and the host authorizes it against the
/// collections the block claimed. A record is `{"id": …, "data": {…}}`: the
/// id is the primary key, `data` is the row's columns. The host stamps
/// `created_at` on insert and `updated_at` on every write, and generates an
/// id when the data carries none.
pub mod db {
    use super::{
        call, filters_json, json::Json, Ctx, Filter, HostError, ListOptions, TableDef, DATABASE,
    };

    /// Create `table` and its indexes if they are not already there.
    ///
    /// Idempotent, and the right thing to call from `init` on every start —
    /// a block has no separate migration step.
    pub fn ensure_table(_ctx: &Ctx, table: TableDef) -> Result<(), HostError> {
        call(
            DATABASE,
            "database.ensure_table",
            &Json::obj().set("table", table.to_json()),
        )
        .map(|_| ())
    }

    /// Insert a row and return the record as stored.
    pub fn create(_ctx: &Ctx, collection: &str, data: Json) -> Result<Json, HostError> {
        call(
            DATABASE,
            "database.create",
            &Json::obj()
                .set("collection", Json::str(collection))
                .set("data", data),
        )
    }

    /// Read one row by id. A missing row is a `NotFound` [`HostError`].
    pub fn get(_ctx: &Ctx, collection: &str, id: &str) -> Result<Json, HostError> {
        call(
            DATABASE,
            "database.get",
            &Json::obj()
                .set("collection", Json::str(collection))
                .set("id", Json::str(id)),
        )
    }

    /// List rows, returning one record per row.
    pub fn list(
        _ctx: &Ctx,
        collection: &str,
        options: ListOptions,
    ) -> Result<Vec<Json>, HostError> {
        let sort = Json::Arr(
            options
                .sort
                .iter()
                .map(|(field, desc)| {
                    Json::obj()
                        .set("field", Json::str(field))
                        .set("desc", Json::Bool(*desc))
                })
                .collect(),
        );
        let mut request = Json::obj()
            .set("collection", Json::str(collection))
            .set("filters", filters_json(&options.filters))
            .set("sort", sort)
            .set("offset", Json::int(options.offset));
        // Absent is "every row"; the host has no in-band spelling for it.
        if let Some(limit) = options.limit {
            request = request.set("limit", Json::int(i64::from(limit)));
        }
        let response = call(DATABASE, "database.list", &request)?;
        Ok(response
            .get("records")
            .and_then(Json::as_array)
            .map(|records| records.to_vec())
            .unwrap_or_default())
    }

    /// Update the named columns of one row and return the record as stored.
    pub fn update(_ctx: &Ctx, collection: &str, id: &str, data: Json) -> Result<Json, HostError> {
        call(
            DATABASE,
            "database.update",
            &Json::obj()
                .set("collection", Json::str(collection))
                .set("id", Json::str(id))
                .set("data", data),
        )
    }

    /// Delete one row by id.
    pub fn delete(_ctx: &Ctx, collection: &str, id: &str) -> Result<(), HostError> {
        call(
            DATABASE,
            "database.delete",
            &Json::obj()
                .set("collection", Json::str(collection))
                .set("id", Json::str(id)),
        )
        .map(|_| ())
    }

    /// Count the rows matching `filters`.
    pub fn count(_ctx: &Ctx, collection: &str, filters: &[Filter]) -> Result<i64, HostError> {
        let response = call(
            DATABASE,
            "database.count",
            &Json::obj()
                .set("collection", Json::str(collection))
                .set("filters", filters_json(filters)),
        )?;
        Ok(response
            .get("count")
            .and_then(Json::as_i64)
            .unwrap_or_default())
    }
}

/// The object-storage service (`wafer-run/storage`).
///
/// Objects live at `{folder}/{key}`, and the host authorizes on that whole
/// string against the folders the block claimed. A key with a `.` or `..`
/// segment is refused outright, whatever the claim says.
pub mod storage {
    use super::{call, call_frames, json::Json, Ctx, HostError, STORAGE};

    /// Write an object.
    pub fn put(
        _ctx: &Ctx,
        folder: &str,
        key: &str,
        data: &[u8],
        content_type: &str,
    ) -> Result<(), HostError> {
        let bytes = Json::Arr(data.iter().map(|byte| Json::int(*byte as i64)).collect());
        call(
            STORAGE,
            "storage.put",
            &Json::obj()
                .set("folder", Json::str(folder))
                .set("key", Json::str(key))
                .set("data", bytes)
                .set("content_type", Json::str(content_type)),
        )
        .map(|_| ())
    }

    /// Read an object: its bytes and its content type.
    ///
    /// The one op whose response is two frames — an `ObjectInfo` header and
    /// the body verbatim — so it reads them separately rather than through
    /// `call`.
    pub fn get(_ctx: &Ctx, folder: &str, key: &str) -> Result<(Vec<u8>, String), HostError> {
        let frames = call_frames(
            STORAGE,
            "storage.get",
            &Json::obj()
                .set("folder", Json::str(folder))
                .set("key", Json::str(key)),
        )?;
        let mut frames = frames.into_iter();
        let header = frames
            .next()
            .ok_or_else(|| HostError::internal("storage.get sent no header".to_string()))?;
        let info = Json::parse(&String::from_utf8_lossy(&header)).map_err(|detail| {
            HostError::internal(format!("storage.get header is not JSON: {detail}"))
        })?;
        let content_type = info
            .get("content_type")
            .and_then(Json::as_str)
            .unwrap_or("application/octet-stream")
            .to_string();
        let body: Vec<u8> = frames.flatten().collect();
        Ok((body, content_type))
    }

    /// Delete an object.
    pub fn delete(_ctx: &Ctx, folder: &str, key: &str) -> Result<(), HostError> {
        call(
            STORAGE,
            "storage.delete",
            &Json::obj()
                .set("folder", Json::str(folder))
                .set("key", Json::str(key)),
        )
        .map(|_| ())
    }

    /// List the keys in `folder` that start with `prefix`.
    pub fn list(_ctx: &Ctx, folder: &str, prefix: &str) -> Result<Vec<String>, HostError> {
        let response = call(
            STORAGE,
            "storage.list",
            &Json::obj()
                .set("folder", Json::str(folder))
                .set("prefix", Json::str(prefix))
                .set("limit", Json::int(0))
                .set("offset", Json::int(0)),
        )?;
        Ok(response
            .get("objects")
            .and_then(Json::as_array)
            .map(|objects| {
                objects
                    .iter()
                    .filter_map(|object| object.get("key").and_then(Json::as_str))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default())
    }
}

/// The configuration service (`wafer-run/config`).
pub mod config {
    use super::{call, json::Json, Ctx, HostError, CONFIG};

    /// Read a config key, or `None` when it is unset.
    ///
    /// The key must be one the block claimed with `Block::config_key`; the
    /// deployment's admin sets its value.
    pub fn get(_ctx: &Ctx, key: &str) -> Result<Option<String>, HostError> {
        let response = call(
            CONFIG,
            "config.get",
            &Json::obj().set("key", Json::str(key)),
        )?;
        // The service answers `""` for an unset key, which is not the same
        // fact as a key deliberately set to the empty string — but it is the
        // only distinction the wire carries.
        Ok(response
            .get("value")
            .and_then(Json::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string))
    }
}

/// Structured logging into the host's log sink.
///
/// Fire-and-forget: there is no result, and a log line never fails a request.
/// Lines land in the deployment's logs, not in any HTTP response, so they are
/// for the operator rather than the caller.
pub mod log {
    /// Log at `error`.
    pub fn error(message: &str) {
        emit("error", message);
    }

    /// Log at `warn`.
    pub fn warn(message: &str) {
        emit("warn", message);
    }

    /// Log at `info`.
    pub fn info(message: &str) {
        emit("info", message);
    }

    /// Log at `debug`.
    pub fn debug(message: &str) {
        emit("debug", message);
    }

    /// Hand one line to the host.
    fn emit(level: &str, message: &str) {
        unsafe {
            super::__wafer_host_log(
                level.as_ptr() as i32,
                level.len() as i32,
                message.as_ptr() as i32,
                message.len() as i32,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Edge cases of the JSON codec that no template exercises.
///
/// They live here, beside the code, rather than in the sandbox's own test
/// suite: the sandbox reaches them anyway (its parity test includes this file
/// with `#[path]`, and an integration-test crate is compiled with `cfg(test)`),
/// and a block author running `cargo test` in their own crate gets them too.
/// Nothing here is compiled into a block's `.wasm` — `cargo build` does not
/// set `cfg(test)`.
#[cfg(test)]
mod tests {
    use super::json::Json;

    /// A `\u` pair whose second half is not a low surrogate is a parse error,
    /// not a code point folded together out of two unrelated escapes.
    #[test]
    fn a_surrogate_pair_needs_a_real_low_surrogate() {
        // The escapes below are written with `\\u` in ordinary string
        // literals, so the JSON text really contains a backslash — a raw
        // literal holding the character itself would exercise the UTF-8
        // copy path instead, which is a different branch.
        assert_eq!(
            Json::parse("\"\\uD83D\\uDE80\"")
                .expect("a real pair")
                .as_str(),
            Some("\u{1F680}"),
        );
        // Two legal escapes that are not a pair. `\u0041` is `A`; masking the
        // low half instead of range-checking it folds the two together into
        // U+1F441, a code point the document never contained.
        assert!(Json::parse("\"\\uD83D\\u0041\"").is_err());
        // A high surrogate with nothing after it.
        assert!(Json::parse("\"\\uD83D\"").is_err());
        // A low surrogate with nothing before it is not a code point at all.
        assert!(Json::parse("\"\\uDE80\"").is_err());
    }

    /// A literal too large for an `f64` is a parse error, not an infinity the
    /// renderer would silently turn back into `null`.
    #[test]
    fn a_number_that_overflows_is_a_parse_error() {
        assert!(Json::parse("1e999").is_err());
        assert!(Json::parse("-1e999").is_err());
        assert_eq!(
            Json::parse("1e308").expect("in range").as_f64(),
            Some(1e308)
        );
    }

    /// A non-finite number block code built by arithmetic renders as `null`.
    /// JSON has no `Infinity`, and writing the bare token would cost the
    /// whole document rather than one field.
    #[test]
    fn a_non_finite_number_renders_as_null() {
        assert_eq!(Json::Num(f64::INFINITY).render(), "null");
        assert_eq!(Json::Num(f64::NEG_INFINITY).render(), "null");
        assert_eq!(Json::Num(f64::NAN).render(), "null");
        let rendered = Json::obj().set("n", Json::Num(f64::NAN)).render();
        assert_eq!(rendered, r#"{"n":null}"#);
        // The point of the substitution: what comes out still parses.
        assert!(Json::parse(&rendered).is_ok());
    }
}
