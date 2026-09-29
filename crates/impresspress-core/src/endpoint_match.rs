//! Shared route table and request-to-endpoint matcher for impresspress blocks.
//!
//! A block declares its HTTP surface once, as a `const` table of
//! [`EndpointRoute`] rows: method, wire-path template, handler key, auth level
//! and the OpenAPI/WebMCP metadata. [`declare`] turns that table into the
//! block's `info().endpoints`, and [`dispatch`] matches an incoming request
//! (its [`RequestAction`]-style action plus resource path) against the same
//! rows, extracts `{name}`, `{rest...}` and `{rest...}/` path variables into
//! `req.param.*` meta, and yields the matched handler key. The per-block
//! `path.starts_with(...)` / `strip_prefix(...)` guard chains and the manual
//! single-segment param parsing that used to live in every `handle()` are
//! gone; the one deliberate path read left outside `dispatch` is llm's
//! inter-block `/b/llm/api/internal/default-target` guard, which is not an
//! HTTP endpoint and is documented at its site.
//!
//! ## Percent-encoding
//!
//! Every adapter hands this module the path **as it appeared on the wire**,
//! still percent-encoded: axum's `Uri::path`, `url::Url::path` on Cloudflare,
//! and `Url.pathname` in the Service Worker all preserve the escapes. That is
//! what makes an id containing `/` addressable at all — `%2F` is one segment
//! to the matcher, whereas a decoded `/` would split the route.
//!
//! Matching therefore happens on the encoded path (templates are literal ASCII
//! and need no decoding), and [`dispatch`] decodes each bound variable before
//! it lands in `req.param.*`, so a handler reads the value the caller encoded
//! rather than the escape sequence. Without that decode the encoding a page
//! must apply when it builds a URL has no inverse, and the round trip silently
//! misses the record it names.
//!
//! ## Template syntax
//!
//! - A literal segment matches itself exactly.
//! - `{name}` matches exactly one path segment and binds it to `req.param.name`.
//! - `{name...}` (trailing, "rest") matches one or more remaining segments
//!   (joined by `/`) and binds the whole remainder to `req.param.name`.
//! - `{name...}/` (rest followed by a trailing slash, a folder-style listing)
//!   requires the path to end in `/` and binds the non-empty remainder before
//!   that final slash, so `/b/x/y/` matches `/b/x/{name}/` and not
//!   `/b/x/{name}/{rest...}/`.
//! - A trailing `/` in the template requires a trailing `/` in the path
//!   (templates and paths are compared segment-by-segment, with the empty
//!   trailing segment from a trailing slash preserved).
//!
//! The matcher is platform-neutral: it works the same on native, Cloudflare,
//! and browser targets because it operates purely on the already-normalized
//! `req.action` / `req.resource` meta.
//!
//! ## Why not `wafer_block::Router`?
//!
//! `wafer_block::Router` / `match_path` / `extract_path_vars` were deleted in
//! the wafer-run quality program (phase 1) — they had zero consumers. This is
//! the impresspress-local replacement. The matcher is small and impresspress-specific
//! (it keys off impresspress's `(action, path)` dispatch convention); if it ever
//! needs to be shared with another wafer-run consumer it should be proposed as
//! a fresh `wafer_block` module rather than resurrecting the old `Router`.

use wafer_run::{AuthLevel, BlockEndpoint, HttpMethod, Message};

/// Map an [`HttpMethod`] to the canonical wire action string impresspress routes on
/// (`req.action`). Mirrors `wafer_block::http_codec::action_for_http_method`
/// for the four methods endpoints declare, so a block's `[(method, template)]`
/// table compares against the same action the pipeline already set.
pub fn action_for_method(method: HttpMethod) -> &'static str {
    match method {
        HttpMethod::Get => "retrieve",
        HttpMethod::Post => "create",
        HttpMethod::Patch => "update",
        HttpMethod::Delete => "delete",
    }
}

/// Match `path` against `template`, returning the bound `{name}` path variables
/// (in template order) when it matches, or `None` when it does not.
///
/// Both inputs are split on `/`; a trailing slash therefore yields a trailing
/// empty segment that must match on both sides. `{name...}` (rest) is valid
/// as the final template segment, where it greedily binds the remainder, or
/// as `{name...}/`, where the path must end in `/` and it binds the non-empty
/// remainder before that slash.
///
/// Values are returned **as they appear in `path`**, so still percent-encoded
/// — matching must happen on the encoded form (see the module docs). Callers
/// that hand a variable to a handler percent-decode it first; [`dispatch`]
/// does that for every route it binds.
pub fn match_template<'p>(template: &str, path: &'p str) -> Option<Vec<(String, &'p str)>> {
    let t_segs: Vec<&str> = template.split('/').collect();
    let p_segs: Vec<&str> = path.split('/').collect();
    let mut params: Vec<(String, &'p str)> = Vec::new();

    for (i, t) in t_segs.iter().enumerate() {
        // Rest-parameter: bind every remaining path segment.
        if let Some(name) = t.strip_suffix("...}").and_then(|s| s.strip_prefix('{')) {
            // Either the final template segment, or the second-to-last with
            // an empty final segment: `{rest...}/`, the folder-listing shape,
            // where the path must end in `/` too.
            let last = t_segs.len() - 1;
            let folder = i + 1 == last && t_segs[last].is_empty();
            if i != last && !folder {
                return None;
            }
            // Need at least one remaining segment, and it must be non-empty
            // (so `/b/x/` does NOT match `/b/x/{rest...}`).
            let rest_start = i;
            if rest_start >= p_segs.len() {
                return None;
            }
            let mut joined = &path[byte_offset_of_segment(path, rest_start)..];
            if folder {
                // The trailing slash is the template's, not the value's; what
                // precedes it must be non-empty, so `/b/x/y/` still resolves
                // to `/b/x/{name}/` alone and `/b/x/y//` binds nothing.
                joined = joined.strip_suffix('/')?;
            }
            if joined.is_empty() {
                return None;
            }
            params.push((name.to_string(), joined));
            return Some(params);
        }

        // Out of path segments: no match.
        let p = p_segs.get(i)?;

        if let Some(name) = t.strip_suffix('}').and_then(|s| s.strip_prefix('{')) {
            // Single-segment variable — reject empty segments so `/b/x//` does
            // not bind an empty id.
            if p.is_empty() {
                return None;
            }
            params.push((name.to_string(), p));
        } else if t != p {
            return None;
        }
    }

    // Every template segment consumed; the path must not have extra segments.
    if p_segs.len() != t_segs.len() {
        return None;
    }
    Some(params)
}

/// Byte offset where the `n`-th `/`-split segment starts in `path`.
fn byte_offset_of_segment(path: &str, n: usize) -> usize {
    if n == 0 {
        return 0;
    }
    let mut seen = 0;
    for (idx, b) in path.bytes().enumerate() {
        if b == b'/' {
            seen += 1;
            if seen == n {
                return idx + 1;
            }
        }
    }
    path.len()
}

/// A function that produces a JSON Schema on demand.
///
/// Rows hold one of these instead of a `serde_json::Value` so a block's table
/// can stay a `const`; [`declare`] calls it once per `info()`. For a
/// `schemars` type pass [`request_schema_of::<T>`] or
/// [`response_schema_of::<T>`] uncalled; for a hand-written schema pass the
/// function that builds it.
pub type SchemaFn = fn() -> serde_json::Value;

/// JSON Schema for a request body, path params or query params of type `T`,
/// exactly as `BlockEndpoint::input::<T>()` / `::path_params::<T>()` /
/// `::query_params::<T>()` derive it: draft 2020-12, subschemas inlined, no
/// `$schema`, under the **deserialize** contract (what a client sends).
///
/// Those settings live in wafer-block and are not public, so this goes
/// through the upstream builder on a throwaway endpoint rather than copying
/// them; a row that names `request_schema_of::<T>` therefore serializes the
/// same bytes the hand-written `info()` list did.
pub fn request_schema_of<T: schemars::JsonSchema>() -> serde_json::Value {
    BlockEndpoint::get("")
        .input::<T>()
        .input_schema
        .expect("BlockEndpoint::input always sets the schema")
}

/// JSON Schema for a response body of type `T`, exactly as
/// `BlockEndpoint::output::<T>()` derives it: same settings as
/// [`request_schema_of`] but under the **serialize** contract (what the server
/// guarantees to emit). The two contracts differ for `#[serde(default)]`,
/// `skip_serializing_if` and `skip_deserializing` fields, which is why a row
/// names one or the other rather than one shared producer.
pub fn response_schema_of<T: schemars::JsonSchema>() -> serde_json::Value {
    BlockEndpoint::get("")
        .output::<T>()
        .output_schema
        .expect("BlockEndpoint::output always sets the schema")
}

/// One row of a block's route table: what `handle()` dispatches on **and**
/// what `info().endpoints` is generated from (see [`declare`]).
///
/// `method`, `template` and `handler` drive matching; everything else is the
/// declaration the router and the OpenAPI/WebMCP projections read. A row
/// always names its [`AuthLevel`] through [`Self::public`],
/// [`Self::authenticated`] or [`Self::admin`]; there is no constructor that
/// defaults to `Public`, because the upstream `BlockEndpoint` default of
/// `Public` is how an unmarked endpoint used to become world-readable by
/// omission.
pub struct EndpointRoute<H> {
    /// HTTP method this route answers (mapped to a wire action internally).
    pub method: HttpMethod,
    /// Path template (`/b/x/{id}`, `/b/x/{rest...}`, …) as it appears on the wire.
    pub template: &'static str,
    /// Block-defined handler discriminator returned to `handle()`.
    pub handler: H,
    /// Level the router enforces before dispatching to this row.
    pub auth: AuthLevel,
    /// Short summary shown in the admin/OpenAPI UI.
    pub summary: &'static str,
    /// Longer description for OpenAPI / docs.
    pub description: &'static str,
    /// Request-body schema producer, if the endpoint takes a body.
    pub input: Option<SchemaFn>,
    /// Response-body schema producer, if the endpoint answers JSON.
    pub output: Option<SchemaFn>,
    /// URL path-parameter schema producer.
    pub path_params: Option<SchemaFn>,
    /// Query-parameter schema producer.
    pub query_params: Option<SchemaFn>,
    /// OpenAPI tags.
    pub tags: &'static [&'static str],
    /// Whether the endpoint is published as deprecated.
    pub deprecated: bool,
    /// `(name, description)` when the endpoint is exposed as a WebMCP tool.
    pub agent_tool: Option<(&'static str, &'static str)>,
}

impl<H: Copy> EndpointRoute<H> {
    const fn with_auth(
        method: HttpMethod,
        template: &'static str,
        handler: H,
        auth: AuthLevel,
    ) -> Self {
        Self {
            method,
            template,
            handler,
            auth,
            summary: "",
            description: "",
            input: None,
            output: None,
            path_params: None,
            query_params: None,
            tags: &[],
            deprecated: false,
            agent_tool: None,
        }
    }

    /// A row anyone may call. Every public row is a decision: the handler
    /// must gate itself by token, signature or shared secret, or need no gate.
    pub const fn public(method: HttpMethod, template: &'static str, handler: H) -> Self {
        Self::with_auth(method, template, handler, AuthLevel::Public)
    }

    /// A row any logged-in caller may call.
    pub const fn authenticated(method: HttpMethod, template: &'static str, handler: H) -> Self {
        Self::with_auth(method, template, handler, AuthLevel::Authenticated)
    }

    /// A row only an admin may call.
    pub const fn admin(method: HttpMethod, template: &'static str, handler: H) -> Self {
        Self::with_auth(method, template, handler, AuthLevel::Admin)
    }

    /// Set the short summary text.
    pub const fn summary(mut self, summary: &'static str) -> Self {
        self.summary = summary;
        self
    }

    /// Set the longer description text.
    pub const fn description(mut self, description: &'static str) -> Self {
        self.description = description;
        self
    }

    /// Declare the request-body schema.
    pub const fn input(mut self, schema: SchemaFn) -> Self {
        self.input = Some(schema);
        self
    }

    /// Declare the response-body schema.
    pub const fn output(mut self, schema: SchemaFn) -> Self {
        self.output = Some(schema);
        self
    }

    /// Declare the path-parameter schema.
    pub const fn path_params(mut self, schema: SchemaFn) -> Self {
        self.path_params = Some(schema);
        self
    }

    /// Declare the query-parameter schema.
    pub const fn query_params(mut self, schema: SchemaFn) -> Self {
        self.query_params = Some(schema);
        self
    }

    /// Set the OpenAPI tag list.
    pub const fn tags(mut self, tags: &'static [&'static str]) -> Self {
        self.tags = tags;
        self
    }

    /// Publish the endpoint as deprecated.
    pub const fn deprecated(mut self) -> Self {
        self.deprecated = true;
        self
    }

    /// Expose the endpoint as a WebMCP tool with this name and description.
    pub const fn agent_tool(mut self, name: &'static str, description: &'static str) -> Self {
        self.agent_tool = Some((name, description));
        self
    }
}

/// The `BlockEndpoint`s a table declares, in table order, built through the
/// upstream builders so the result is what a hand-written `info()` list
/// produced. Each schema producer is called once.
pub fn declare<H: Copy>(table: &[EndpointRoute<H>]) -> Vec<BlockEndpoint> {
    table
        .iter()
        .map(|row| {
            let mut ep = match row.method {
                HttpMethod::Get => BlockEndpoint::get(row.template),
                HttpMethod::Post => BlockEndpoint::post(row.template),
                HttpMethod::Patch => BlockEndpoint::patch(row.template),
                HttpMethod::Delete => BlockEndpoint::delete(row.template),
            }
            .summary(row.summary)
            .description(row.description)
            .auth(row.auth)
            .tags(row.tags);
            if let Some(schema) = row.input {
                ep = ep.input_schema(schema());
            }
            if let Some(schema) = row.output {
                ep = ep.output_schema(schema());
            }
            if let Some(schema) = row.path_params {
                ep = ep.path_params_schema(schema());
            }
            if let Some(schema) = row.query_params {
                ep = ep.query_params_schema(schema());
            }
            if row.deprecated {
                ep = ep.deprecated();
            }
            if let Some((name, description)) = row.agent_tool {
                ep = ep.agent_tool(name, description);
            }
            ep
        })
        .collect()
}

/// Find the first route in `table` whose method+template matches the request,
/// writing any extracted path variables (percent-decoded) into `msg`'s
/// `req.param.*` meta and returning the matched handler key.
///
/// Routes are tried in declaration order, so blocks list more-specific
/// templates before generic ones (the same ordering discipline the old
/// `starts_with` chains relied on). Returns `None` when nothing matches, so the
/// caller emits its own 404.
///
/// The path is matched exactly first. Only when that fails, and the path has
/// no trailing slash, is it retried with one appended: index routes are
/// declared with a trailing slash (`/b/messages/`), and [`match_template`]
/// compares segment counts, so `/b/messages` -- three segments against the
/// template's four -- could not match it and 404'd while `/b/messages/`
/// served fine. The sidebar always links the slashed form, so this only bit
/// someone typing or bookmarking the bare path, but it bit inconsistently
/// while some blocks tolerated it through hand-written prefix matchers.
/// Handling it here rather than with a second row in every table keeps each
/// table a faithful mirror of `info().endpoints`. Because the retry runs only
/// after an exact match has failed, no request that resolves exactly can be
/// re-routed by it; [`endpoint_auth`] mirrors the retry so the router gates
/// the bare form at the row's declared level.
pub fn dispatch<H: Copy>(msg: &mut Message, table: &[EndpointRoute<H>]) -> Option<H> {
    let action = msg.action().to_string();
    let path = msg.path().to_string();
    let with_slash = (!path.ends_with('/')).then(|| format!("{path}/"));

    for candidate in std::iter::once(path).chain(with_slash) {
        for route in table {
            if action_for_method(route.method) != action {
                continue;
            }
            if let Some(params) = match_template(route.template, &candidate) {
                for (name, value) in params {
                    msg.set_meta(
                        format!("{}{}", wafer_run::META_REQ_PARAM_PREFIX, name),
                        crate::util::url_path_decode(value),
                    );
                }
                return Some(route.handler);
            }
        }
    }
    None
}

/// The access policy a path resolves to for a single block, combining its
/// declared endpoint [`AuthLevel`]s. Used by the central router to enforce the
/// declared level before dispatch.
///
/// Returns the **strictest** [`AuthLevel`] (`Public < Authenticated < Admin`)
/// among ALL declared endpoints whose method+path template matches `(action,
/// path)`, or `None` when no declared endpoint covers the request (the caller
/// then falls back to the coarse prefix tier).
///
/// Strictest-match — not first-match — is deliberate and load-bearing for
/// security: a path can be covered by more than one template (e.g.
/// `/b/storage/admin/` matches both the generic `/b/storage/{bucket}/` and the
/// specific `/b/storage/admin/`). If this returned the *first* match, a
/// permissive generic endpoint declared before a stricter specific one would
/// silently weaken it — letting an authenticated non-admin reach an admin page
/// purely because of endpoint declaration order. Taking the max means
/// declaration order can never lower the required auth level; the worst a
/// mis-ordered declaration can do is over-protect, which fails safe. This
/// mirrors the `RouteAccess::max` discipline the router already applies between
/// the prefix tier and the declared level.
pub fn endpoint_auth(
    endpoints: &[wafer_run::BlockEndpoint],
    action: &str,
    path: &str,
) -> Option<AuthLevel> {
    if let Some(level) = endpoint_auth_exact(endpoints, action, path) {
        return Some(level);
    }
    // Mirror `dispatch`'s trailing-slash retry, and only after an exact
    // match has failed. A block's `dispatch` serves `GET /b/llm` from its
    // `/b/llm/` row, so the router must gate the bare form at that row's
    // declared level; without this it fell back to the `Authenticated`
    // default and a logged-in non-admin reached an `Admin` page.
    if !path.ends_with('/') {
        return endpoint_auth_exact(endpoints, action, &format!("{path}/"));
    }
    None
}

/// The strict, single-pass resolver behind [`endpoint_auth`].
fn endpoint_auth_exact(
    endpoints: &[wafer_run::BlockEndpoint],
    action: &str,
    path: &str,
) -> Option<AuthLevel> {
    let mut strictest: Option<AuthLevel> = None;
    for ep in endpoints {
        if action_for_method(ep.method) != action {
            continue;
        }
        if match_template(&ep.path, path).is_some() {
            strictest = Some(match strictest {
                Some(cur) if auth_rank(cur) >= auth_rank(ep.auth) => cur,
                _ => ep.auth,
            });
        }
    }
    strictest
}

/// Strictness rank for an [`AuthLevel`] (`Public < Authenticated < Admin`).
///
/// `AuthLevel` is defined upstream (wafer-block) without an `Ord` derive, so
/// this is the local ordering used by [`endpoint_auth`] to pick the strictest
/// matching endpoint. The numbers are an internal detail — only their relative
/// order matters.
///
/// `pub(crate)` so the gate tests that assert "every wire spelling of a route
/// is enforced at least as strictly as its declaration" compare with the same
/// ordering the router itself applies. A test-local copy would keep passing
/// against its own idea of strictness while this one changed underneath it —
/// which is the failure mode those gates exist to catch, wearing their name.
pub(crate) fn auth_rank(level: AuthLevel) -> u8 {
    match level {
        AuthLevel::Public => 0,
        AuthLevel::Authenticated => 1,
        AuthLevel::Admin => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(params: &[(String, &str)]) -> Vec<(String, String)> {
        params
            .iter()
            .map(|(k, v)| (k.clone(), v.to_string()))
            .collect()
    }

    #[test]
    fn literal_exact_match() {
        assert_eq!(match_template("/b/x/api", "/b/x/api"), Some(vec![]));
    }

    #[test]
    fn literal_mismatch() {
        assert!(match_template("/b/x/api", "/b/x/other").is_none());
    }

    #[test]
    fn extra_path_segments_do_not_match() {
        // The old `starts_with` matched `/b/x/api/extra`; the template matcher
        // must not (it routes the suffix to a different, more-specific entry).
        assert!(match_template("/b/x/api", "/b/x/api/extra").is_none());
    }

    #[test]
    fn single_param_extracts_segment() {
        let m = match_template("/b/x/api/contexts/{id}", "/b/x/api/contexts/abc").unwrap();
        assert_eq!(names(&m), vec![("id".to_string(), "abc".to_string())]);
    }

    #[test]
    fn single_param_rejects_extra_segment() {
        // `{id}` is ONE segment — `/contexts/abc/entries` must not match the
        // get-context template (it belongs to the entries template).
        assert!(
            match_template("/b/x/api/contexts/{id}", "/b/x/api/contexts/abc/entries").is_none()
        );
    }

    #[test]
    fn nested_literal_after_param() {
        let m = match_template(
            "/b/x/api/contexts/{id}/entries",
            "/b/x/api/contexts/abc/entries",
        )
        .unwrap();
        assert_eq!(names(&m), vec![("id".to_string(), "abc".to_string())]);
    }

    #[test]
    fn two_params() {
        let m = match_template(
            "/b/llm/api/models/{backend_id}/{model_id}/status",
            "/b/llm/api/models/ollama/llama3/status",
        )
        .unwrap();
        assert_eq!(
            names(&m),
            vec![
                ("backend_id".to_string(), "ollama".to_string()),
                ("model_id".to_string(), "llama3".to_string()),
            ]
        );
    }

    #[test]
    fn empty_param_segment_rejected() {
        // `/b/x/api/contexts//` must not bind an empty id.
        assert!(match_template("/b/x/api/contexts/{id}", "/b/x/api/contexts/").is_none());
    }

    #[test]
    fn rest_param_binds_remaining_segments() {
        let m = match_template(
            "/b/storage/{bucket}/{prefix...}",
            "/b/storage/photos/2024/x",
        )
        .unwrap();
        assert_eq!(
            names(&m),
            vec![
                ("bucket".to_string(), "photos".to_string()),
                ("prefix".to_string(), "2024/x".to_string()),
            ]
        );
    }

    #[test]
    fn rest_param_requires_at_least_one_segment() {
        assert!(match_template("/b/storage/{bucket}/{prefix...}", "/b/storage/photos/").is_none());
    }

    #[test]
    fn trailing_slash_significant() {
        assert!(match_template("/b/x/", "/b/x").is_none());
        assert!(match_template("/b/x/", "/b/x/").is_some());
    }

    #[test]
    fn dispatch_extracts_param_into_meta() {
        let mut msg = Message::new("test");
        msg.set_meta("req.action", "retrieve");
        msg.set_meta("req.resource", "/b/messages/api/contexts/ctx-7");
        let table = [EndpointRoute::admin(
            HttpMethod::Get,
            "/b/messages/api/contexts/{id}",
            1u8,
        )];
        let h = dispatch(&mut msg, &table);
        assert_eq!(h, Some(1u8));
        assert_eq!(msg.var("id"), "ctx-7");
    }

    /// A `{name}` binding is the inverse of `util::url_path_encode`, which is
    /// what a page applies when it puts a record id in a URL. Matching happens
    /// on the still-encoded path — `%2F` is one segment, which is the only
    /// reason an id holding `/` is addressable at all — and the decode is owed
    /// on the bound value.
    #[test]
    fn dispatch_percent_decodes_a_bound_param() {
        let raw_id = "prod/1?x#y";
        let path = format!(
            "/b/messages/api/contexts/{}",
            crate::util::url_path_encode(raw_id)
        );
        assert_eq!(path, "/b/messages/api/contexts/prod%2F1%3Fx%23y");

        let mut msg = Message::new("test");
        msg.set_meta("req.action", "retrieve");
        msg.set_meta("req.resource", &path);
        let table = [EndpointRoute::admin(
            HttpMethod::Get,
            "/b/messages/api/contexts/{id}",
            1u8,
        )];
        assert_eq!(dispatch(&mut msg, &table), Some(1u8));
        assert_eq!(msg.var("id"), raw_id);
    }

    /// Rest params carry object keys, which have the same problem for the same
    /// reason: a key holding a space reaches the wire as `%20`.
    #[test]
    fn dispatch_percent_decodes_a_bound_rest_param() {
        let mut msg = Message::new("test");
        msg.set_meta("req.action", "retrieve");
        msg.set_meta(
            "req.resource",
            "/b/storage/api/buckets/photos/objects/holiday%20snaps/a%2Bb.txt",
        );
        let table = [EndpointRoute::admin(
            HttpMethod::Get,
            "/b/storage/api/buckets/{name}/objects/{key...}",
            1u8,
        )];
        assert_eq!(dispatch(&mut msg, &table), Some(1u8));
        assert_eq!(msg.var("key"), "holiday snaps/a+b.txt");
    }

    /// An escape that is not valid UTF-8 passes through unchanged rather than
    /// failing the match or being lossily substituted: the handler then looks
    /// it up and answers "not found", which is a failure the caller can read.
    #[test]
    fn dispatch_leaves_an_undecodable_param_alone() {
        let mut msg = Message::new("test");
        msg.set_meta("req.action", "retrieve");
        msg.set_meta("req.resource", "/b/messages/api/contexts/%FF%FE");
        let table = [EndpointRoute::admin(
            HttpMethod::Get,
            "/b/messages/api/contexts/{id}",
            1u8,
        )];
        assert_eq!(dispatch(&mut msg, &table), Some(1u8));
        assert_eq!(msg.var("id"), "%FF%FE");
    }

    #[test]
    fn dispatch_respects_method() {
        let mut msg = Message::new("test");
        msg.set_meta("req.action", "create");
        msg.set_meta("req.resource", "/b/messages/api/contexts");
        let table = [
            EndpointRoute::admin(HttpMethod::Get, "/b/messages/api/contexts", 1u8),
            EndpointRoute::admin(HttpMethod::Post, "/b/messages/api/contexts", 2u8),
        ];
        assert_eq!(dispatch(&mut msg, &table), Some(2u8));
    }

    #[test]
    fn dispatch_matches_an_index_route_without_its_trailing_slash() {
        // `/b/messages` used to 404 while `/b/messages/` served the page:
        // `match_template` compares segment counts, and the bare path has one
        // fewer. Only `messages` and `vector` were affected -- the blocks on
        // this shared table -- while admin/userportal/products tolerated it
        // via their own prefix matchers.
        let mut msg = Message::new("test");
        msg.set_meta("req.action", "retrieve");
        msg.set_meta("req.resource", "/b/messages");
        let table = [EndpointRoute::admin(HttpMethod::Get, "/b/messages/", 1u8)];
        assert_eq!(dispatch(&mut msg, &table), Some(1u8));
    }

    #[test]
    fn dispatch_slash_retry_does_not_invent_a_route() {
        // The retry only appends a slash; it must not make an unrelated path
        // resolve. `/b/messages/api` has no route with or without one.
        let mut msg = Message::new("test");
        msg.set_meta("req.action", "retrieve");
        msg.set_meta("req.resource", "/b/messages/api");
        let table = [
            EndpointRoute::admin(HttpMethod::Get, "/b/messages/", 1u8),
            EndpointRoute::admin(HttpMethod::Get, "/b/messages/api/contexts", 2u8),
        ];
        assert_eq!(dispatch(&mut msg, &table), None);
    }

    #[test]
    fn dispatch_slash_retry_never_shadows_an_exact_match() {
        // An exact match is always taken first, so adding the retry cannot
        // re-route a request that already resolved.
        let mut msg = Message::new("test");
        msg.set_meta("req.action", "retrieve");
        msg.set_meta("req.resource", "/b/x/thing");
        let table = [
            EndpointRoute::admin(HttpMethod::Get, "/b/x/thing", 1u8),
            EndpointRoute::admin(HttpMethod::Get, "/b/x/thing/", 2u8),
        ];
        assert_eq!(dispatch(&mut msg, &table), Some(1u8));
    }

    #[test]
    fn dispatch_slash_retry_does_not_bind_an_empty_path_param() {
        // `/b/vector/api/indexes` must not reach `{name}` by growing a slash
        // and binding an empty segment.
        let mut msg = Message::new("test");
        msg.set_meta("req.action", "retrieve");
        msg.set_meta("req.resource", "/b/vector/api/indexes");
        let table = [EndpointRoute::admin(
            HttpMethod::Get,
            "/b/vector/api/indexes/{name}",
            1u8,
        )];
        assert_eq!(dispatch(&mut msg, &table), None);
    }

    #[test]
    fn dispatch_ordering_specific_first() {
        // A specific template listed first must win over a generic one.
        let mut msg = Message::new("test");
        msg.set_meta("req.action", "delete");
        msg.set_meta("req.resource", "/b/vector/api/indexes/my-index");
        let table = [
            EndpointRoute::admin(HttpMethod::Delete, "/b/vector/api/indexes/{name}", 1u8),
            EndpointRoute::admin(HttpMethod::Delete, "/b/vector/api/{index}/{id}", 2u8),
        ];
        assert_eq!(dispatch(&mut msg, &table), Some(1u8));
        assert_eq!(msg.var("name"), "my-index");
    }

    #[test]
    fn endpoint_auth_reads_declared_level() {
        use wafer_run::BlockEndpoint;
        let eps = vec![
            BlockEndpoint::get("/b/legalpages/terms").auth(AuthLevel::Public),
            BlockEndpoint::get("/b/legalpages/admin").auth(AuthLevel::Admin),
            BlockEndpoint::patch("/b/legalpages/api/documents/{id}").auth(AuthLevel::Admin),
        ];
        assert_eq!(
            endpoint_auth(&eps, "retrieve", "/b/legalpages/terms"),
            Some(AuthLevel::Public)
        );
        assert_eq!(
            endpoint_auth(&eps, "retrieve", "/b/legalpages/admin"),
            Some(AuthLevel::Admin)
        );
        assert_eq!(
            endpoint_auth(&eps, "update", "/b/legalpages/api/documents/d-1"),
            Some(AuthLevel::Admin)
        );
        // Undeclared path → None (caller falls back to prefix tier).
        assert_eq!(
            endpoint_auth(&eps, "retrieve", "/b/legalpages/api/documents"),
            None
        );
    }

    #[test]
    fn endpoint_auth_takes_strictest_match_regardless_of_order() {
        use wafer_run::BlockEndpoint;
        // `/b/storage/admin/` matches BOTH the generic `{bucket}/`
        // (Authenticated) and the specific `admin/` (Admin). The generic is
        // declared FIRST — first-match would resolve to Authenticated and let a
        // non-admin through. Strictest-match must resolve to Admin.
        let eps = vec![
            BlockEndpoint::get("/b/storage/{bucket}/").auth(AuthLevel::Authenticated),
            BlockEndpoint::get("/b/storage/admin/").auth(AuthLevel::Admin),
        ];
        assert_eq!(
            endpoint_auth(&eps, "retrieve", "/b/storage/admin/"),
            Some(AuthLevel::Admin),
            "a stricter specific endpoint must win over a permissive generic one \
             declared earlier"
        );
        // A genuine bucket name still resolves to the generic Authenticated tier.
        assert_eq!(
            endpoint_auth(&eps, "retrieve", "/b/storage/photos/"),
            Some(AuthLevel::Authenticated)
        );
    }

    #[test]
    fn endpoint_auth_strictest_independent_of_declaration_order() {
        use wafer_run::BlockEndpoint;
        // Same as above but with the Admin endpoint declared FIRST — the result
        // must be identical (Admin), proving order-independence in both
        // directions.
        let eps = vec![
            BlockEndpoint::get("/b/storage/admin/").auth(AuthLevel::Admin),
            BlockEndpoint::get("/b/storage/{bucket}/").auth(AuthLevel::Authenticated),
        ];
        assert_eq!(
            endpoint_auth(&eps, "retrieve", "/b/storage/admin/"),
            Some(AuthLevel::Admin)
        );
    }

    fn probe_schema() -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": { "id": { "type": "string" } } })
    }

    #[test]
    fn constructors_set_the_auth_they_name() {
        assert_eq!(
            EndpointRoute::public(HttpMethod::Get, "/b/x/", 1u8).auth,
            AuthLevel::Public
        );
        assert_eq!(
            EndpointRoute::authenticated(HttpMethod::Get, "/b/x/", 1u8).auth,
            AuthLevel::Authenticated
        );
        assert_eq!(
            EndpointRoute::admin(HttpMethod::Get, "/b/x/", 1u8).auth,
            AuthLevel::Admin
        );
    }

    #[test]
    fn declare_maps_every_row_field() {
        use wafer_run::BlockEndpoint;
        const TABLE: &[EndpointRoute<u8>] =
            &[
                EndpointRoute::admin(HttpMethod::Post, "/b/x/api/things/{id}", 1u8)
                    .summary("Make a thing")
                    .description("Longer text")
                    .input(probe_schema)
                    .output(probe_schema)
                    .path_params(probe_schema)
                    .query_params(probe_schema)
                    .tags(&["x", "things"])
                    .deprecated()
                    .agent_tool("make_thing", "Makes a thing"),
            ];

        let eps: Vec<BlockEndpoint> = declare(TABLE);
        assert_eq!(eps.len(), 1);
        let ep = &eps[0];
        assert_eq!(ep.method, HttpMethod::Post);
        assert_eq!(ep.path, "/b/x/api/things/{id}");
        assert_eq!(ep.auth, AuthLevel::Admin);
        assert_eq!(ep.summary, "Make a thing");
        assert_eq!(ep.description, "Longer text");
        assert_eq!(ep.input_schema, Some(probe_schema()));
        assert_eq!(ep.output_schema, Some(probe_schema()));
        assert_eq!(ep.path_params, Some(probe_schema()));
        assert_eq!(ep.query_params, Some(probe_schema()));
        assert_eq!(ep.tags, vec!["x".to_string(), "things".to_string()]);
        assert!(ep.deprecated);
        let tool = ep.agent_tool.as_ref().expect("agent tool declared");
        assert_eq!(tool.name, "make_thing");
        assert_eq!(tool.description, "Makes a thing");
    }

    /// A row with no metadata must produce exactly what the upstream builders
    /// produce from `BlockEndpoint::get(path)` alone, so a block that only
    /// ever set method, path and auth serializes the same bytes as before.
    #[test]
    fn declare_leaves_unset_metadata_at_the_upstream_defaults() {
        use wafer_run::BlockEndpoint;
        let eps = declare(&[EndpointRoute::public(HttpMethod::Get, "/b/x/", 1u8)]);
        let ep = &eps[0];
        let bare = BlockEndpoint::get("/b/x/");
        assert_eq!(ep.auth, AuthLevel::Public);
        assert_eq!(ep.summary, bare.summary);
        assert_eq!(ep.description, bare.description);
        assert_eq!(ep.input_schema, bare.input_schema);
        assert_eq!(ep.output_schema, bare.output_schema);
        assert_eq!(ep.path_params, bare.path_params);
        assert_eq!(ep.query_params, bare.query_params);
        assert_eq!(ep.tags, bare.tags);
        assert_eq!(ep.deprecated, bare.deprecated);
        assert!(ep.agent_tool.is_none());
    }

    #[test]
    fn declare_preserves_table_order() {
        let eps = declare(&[
            EndpointRoute::public(HttpMethod::Get, "/b/x/api/things", 1u8),
            EndpointRoute::public(HttpMethod::Post, "/b/x/api/things", 2u8),
        ]);
        assert_eq!(eps[0].method, HttpMethod::Get);
        assert_eq!(eps[1].method, HttpMethod::Post);
    }

    /// A field with `#[serde(default)]` is optional for a client to send but
    /// always present in what the server emits, so the two contracts publish
    /// different `required` lists. The probe carries one so these tests
    /// cannot pass by both producers happening to agree.
    #[derive(serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
    struct ContractProbe {
        id: String,
        #[serde(default)]
        count: u32,
    }

    #[test]
    fn request_schema_of_matches_the_upstream_request_builders() {
        use wafer_run::BlockEndpoint;
        let expected = BlockEndpoint::get("/b/x")
            .input::<ContractProbe>()
            .input_schema
            .expect("upstream derive sets the schema");
        assert_eq!(request_schema_of::<ContractProbe>(), expected);
        assert_eq!(
            BlockEndpoint::get("/b/x")
                .path_params::<ContractProbe>()
                .path_params,
            Some(request_schema_of::<ContractProbe>()),
            "path params derive under the same (deserialize) contract as a body"
        );
        assert_eq!(
            expected["required"],
            serde_json::json!(["id"]),
            "a client may omit a defaulted field"
        );
    }

    #[test]
    fn response_schema_of_matches_the_upstream_response_builder() {
        use wafer_run::BlockEndpoint;
        let expected = BlockEndpoint::get("/b/x")
            .output::<ContractProbe>()
            .output_schema
            .expect("upstream derive sets the schema");
        assert_eq!(response_schema_of::<ContractProbe>(), expected);
        assert_eq!(
            expected["required"],
            serde_json::json!(["id", "count"]),
            "the server always emits a defaulted field"
        );
        assert_ne!(
            request_schema_of::<ContractProbe>(),
            response_schema_of::<ContractProbe>()
        );
    }

    /// Metadata is declaration only; the matcher reads method, template and
    /// handler and nothing else.
    #[test]
    fn dispatch_ignores_row_metadata() {
        let mut msg = Message::new("test");
        msg.set_meta("req.action", "retrieve");
        msg.set_meta("req.resource", "/b/x/api/things/t-1");
        let table = [
            EndpointRoute::admin(HttpMethod::Get, "/b/x/api/things/{id}", 7u8)
                .summary("s")
                .tags(&["x"]),
        ];
        assert_eq!(dispatch(&mut msg, &table), Some(7u8));
        assert_eq!(msg.var("id"), "t-1");
    }

    /// The router and the block must agree on which row serves a request.
    /// `dispatch` retries a bare index path with a trailing slash, so
    /// `GET /b/llm` reaches the `Admin` chat page; `endpoint_auth` has to
    /// resolve the same row, or the router gates the request at the
    /// fail-closed `Authenticated` default and a logged-in non-admin gets an
    /// admin page.
    #[test]
    fn endpoint_auth_matches_an_index_route_without_its_trailing_slash() {
        use wafer_run::BlockEndpoint;
        let eps = vec![BlockEndpoint::get("/b/llm/").auth(AuthLevel::Admin)];
        assert_eq!(
            endpoint_auth(&eps, "retrieve", "/b/llm"),
            Some(AuthLevel::Admin)
        );
    }

    #[test]
    fn endpoint_auth_slash_retry_never_shadows_an_exact_match() {
        use wafer_run::BlockEndpoint;
        let eps = vec![
            BlockEndpoint::get("/b/x/thing").auth(AuthLevel::Public),
            BlockEndpoint::get("/b/x/thing/").auth(AuthLevel::Admin),
        ];
        assert_eq!(
            endpoint_auth(&eps, "retrieve", "/b/x/thing"),
            Some(AuthLevel::Public)
        );
    }

    #[test]
    fn endpoint_auth_slash_retry_does_not_bind_an_empty_path_param() {
        use wafer_run::BlockEndpoint;
        let eps = vec![BlockEndpoint::get("/b/vector/api/indexes/{name}").auth(AuthLevel::Admin)];
        assert_eq!(
            endpoint_auth(&eps, "retrieve", "/b/vector/api/indexes"),
            None
        );
    }

    /// `files` declares `GET /b/storage/{bucket}/{prefix...}/` for nested
    /// folder pages: a rest parameter followed by a trailing slash. The path
    /// must end in `/`, and the bound remainder is what sits between the
    /// fixed prefix and that final slash.
    #[test]
    fn rest_param_may_be_followed_by_a_trailing_slash() {
        let m = match_template(
            "/b/storage/{bucket}/{prefix...}/",
            "/b/storage/photos/2024/x/",
        )
        .unwrap();
        assert_eq!(
            names(&m),
            vec![
                ("bucket".to_string(), "photos".to_string()),
                ("prefix".to_string(), "2024/x".to_string()),
            ]
        );
    }

    /// The slash form is exact: a path without the trailing slash does not
    /// match it. (`dispatch`'s slash retry then finds it, see below.)
    #[test]
    fn rest_param_with_trailing_slash_requires_the_slash() {
        assert!(match_template(
            "/b/storage/{bucket}/{prefix...}/",
            "/b/storage/photos/2024/x"
        )
        .is_none());
    }

    /// The remainder before the final slash must be non-empty, so a bare
    /// bucket page keeps resolving to `/b/storage/{bucket}/` alone and an
    /// empty segment is never bound.
    #[test]
    fn rest_param_with_trailing_slash_requires_a_non_empty_remainder() {
        assert!(match_template("/b/storage/{bucket}/{prefix...}/", "/b/storage/photos/").is_none());
        assert!(
            match_template("/b/storage/{bucket}/{prefix...}/", "/b/storage/photos//").is_none()
        );
    }

    /// A single-segment path such as the public share link never reaches the
    /// folder row, and a rest parameter that is neither last nor followed
    /// only by the trailing slash still matches nothing.
    #[test]
    fn rest_param_with_trailing_slash_does_not_match_a_single_segment_path() {
        assert!(
            match_template("/b/storage/{bucket}/{prefix...}/", "/b/storage/direct/abc").is_none()
        );
        assert!(match_template("/b/x/{rest...}/y", "/b/x/a/b/y").is_none());
    }

    /// `GET /b/storage/photos/2024/x` (no slash) is served by the folder row
    /// through the same retry that serves `/b/messages` from `/b/messages/`.
    #[test]
    fn dispatch_slash_retry_reaches_a_folder_listing() {
        let mut msg = Message::new("test");
        msg.set_meta("req.action", "retrieve");
        msg.set_meta("req.resource", "/b/storage/photos/2024/x");
        let table = [
            EndpointRoute::admin(HttpMethod::Get, "/b/storage/{bucket}/", 1u8),
            EndpointRoute::admin(HttpMethod::Get, "/b/storage/{bucket}/{prefix...}/", 2u8),
        ];
        assert_eq!(dispatch(&mut msg, &table), Some(2u8));
        assert_eq!(msg.var("bucket"), "photos");
        assert_eq!(msg.var("prefix"), "2024/x");
    }

    /// With the folder row declared `Authenticated` beside the `Public` share
    /// link, strictest-match must not raise the share link: the folder
    /// template requires a trailing slash the share path does not have.
    #[test]
    fn endpoint_auth_keeps_a_public_share_link_public_beside_a_folder_listing() {
        use wafer_run::BlockEndpoint;
        let eps = vec![
            BlockEndpoint::get("/b/storage/direct/{token}").auth(AuthLevel::Public),
            BlockEndpoint::get("/b/storage/{bucket}/{prefix...}/").auth(AuthLevel::Authenticated),
        ];
        assert_eq!(
            endpoint_auth(&eps, "retrieve", "/b/storage/direct/abc"),
            Some(AuthLevel::Public)
        );
        assert_eq!(
            endpoint_auth(&eps, "retrieve", "/b/storage/photos/2024/x/"),
            Some(AuthLevel::Authenticated)
        );
    }
}
