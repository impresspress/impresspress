//! Shared request-to-endpoint matcher for impresspress blocks.
//!
//! Blocks declare their HTTP surface once as [`wafer_run::BlockEndpoint`]s in
//! `info().endpoints`. This module matches an incoming request (its
//! [`RequestAction`]-style action plus resource path) against a slice of
//! path templates, extracts `{name}` / `{rest...}` path variables into
//! `req.param.*` meta, and yields the matched handler key. It replaces the
//! per-block `path.starts_with(...)` / `strip_prefix(...)` guard chains and the
//! manual single-segment param parsing that used to live in every `handle()`.
//!
//! ## Template syntax
//!
//! - A literal segment matches itself exactly.
//! - `{name}` matches exactly one path segment and binds it to `req.param.name`.
//! - `{name...}` (trailing, "rest") matches one or more remaining segments
//!   (joined by `/`) and binds the whole remainder to `req.param.name`.
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

use wafer_run::{AuthLevel, HttpMethod, Message};

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
/// empty segment that must match on both sides. `{name...}` (rest) is only
/// valid as the final template segment and greedily binds the remainder.
pub fn match_template<'p>(template: &str, path: &'p str) -> Option<Vec<(String, &'p str)>> {
    let t_segs: Vec<&str> = template.split('/').collect();
    let p_segs: Vec<&str> = path.split('/').collect();
    let mut params: Vec<(String, &'p str)> = Vec::new();

    for (i, t) in t_segs.iter().enumerate() {
        // Trailing rest-parameter: bind every remaining path segment.
        if let Some(name) = t.strip_suffix("...}").and_then(|s| s.strip_prefix('{')) {
            // Must be the final template segment.
            if i != t_segs.len() - 1 {
                return None;
            }
            // Need at least one remaining segment, and it must be non-empty
            // (so `/b/x/` does NOT match `/b/x/{rest...}`).
            let rest_start = i;
            if rest_start >= p_segs.len() {
                return None;
            }
            let joined = &path[byte_offset_of_segment(path, rest_start)..];
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

/// One row of a block's dispatch table: the HTTP method, the path template
/// (typically copied from the block's declared endpoint path), and an opaque
/// handler key `H` the block matches on.
pub struct EndpointRoute<H> {
    /// HTTP method this route answers (mapped to a wire action internally).
    pub method: HttpMethod,
    /// Path template (`/b/x/{id}`, `/b/x/{rest...}`, …).
    pub template: &'static str,
    /// Block-defined handler discriminator returned to `handle()`.
    pub handler: H,
}

impl<H: Copy> EndpointRoute<H> {
    /// Convenience constructor.
    pub const fn new(method: HttpMethod, template: &'static str, handler: H) -> Self {
        Self {
            method,
            template,
            handler,
        }
    }
}

/// Find the first route in `table` whose method+template matches the request,
/// writing any extracted `{name}` path variables into `msg`'s `req.param.*`
/// meta and returning the matched handler key.
///
/// Routes are tried in declaration order, so blocks list more-specific
/// templates before generic ones (the same ordering discipline the old
/// `starts_with` chains relied on). Returns `None` when nothing matches, so the
/// caller emits its own 404.
pub fn dispatch<H: Copy>(msg: &mut Message, table: &[EndpointRoute<H>]) -> Option<H> {
    let action = msg.action().to_string();
    let path = msg.path().to_string();
    dispatch_path(msg, &action, &path, table)
}

/// Like [`dispatch`], but matches against an explicitly supplied `action` +
/// `path` rather than reading them from the message.
///
/// Used by blocks that mount their sub-handlers under a normalized sub-path
/// (e.g. the products admin/user split): the caller passes the normalized path
/// as an explicit argument instead of mutating `req.resource` in place, and
/// extracted `{name}` vars still land in `req.param.*` so the sub-handlers'
/// id readers work unchanged.
pub fn dispatch_path<H: Copy>(
    msg: &mut Message,
    action: &str,
    path: &str,
    table: &[EndpointRoute<H>],
) -> Option<H> {
    for route in table {
        if action_for_method(route.method) != action {
            continue;
        }
        if let Some(params) = match_template(route.template, path) {
            let owned: Vec<(String, String)> = params
                .into_iter()
                .map(|(k, v)| (k, v.to_string()))
                .collect();
            for (name, value) in owned {
                msg.set_meta(
                    format!("{}{}", wafer_run::META_REQ_PARAM_PREFIX, name),
                    value,
                );
            }
            return Some(route.handler);
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
    let mut strictest: Option<AuthLevel> = None;
    for ep in endpoints {
        if action_for_method(ep.method) != action {
            continue;
        }
        if match_template(&normalize_template(&ep.path), path).is_some() {
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
fn auth_rank(level: AuthLevel) -> u8 {
    match level {
        AuthLevel::Public => 0,
        AuthLevel::Authenticated => 1,
        AuthLevel::Admin => 2,
    }
}

/// Normalize a declared endpoint template to the matcher's `{rest...}` syntax.
///
/// Endpoint paths historically use a few spellings for a trailing
/// rest-parameter (`{prefix...}`, `:hash`). The matcher's canonical form is
/// `{name}` / `{name...}`; this converts the legacy `:name` colon style to
/// `{name}` so declared endpoints and dispatch tables agree without forcing a
/// rewrite of every `info()` block at once.
fn normalize_template(template: &str) -> String {
    template
        .split('/')
        .map(|seg| {
            if let Some(name) = seg.strip_prefix(':') {
                format!("{{{name}}}")
            } else {
                seg.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("/")
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
        let table = [EndpointRoute::new(
            HttpMethod::Get,
            "/b/messages/api/contexts/{id}",
            1u8,
        )];
        let h = dispatch(&mut msg, &table);
        assert_eq!(h, Some(1u8));
        assert_eq!(msg.var("id"), "ctx-7");
    }

    #[test]
    fn dispatch_respects_method() {
        let mut msg = Message::new("test");
        msg.set_meta("req.action", "create");
        msg.set_meta("req.resource", "/b/messages/api/contexts");
        let table = [
            EndpointRoute::new(HttpMethod::Get, "/b/messages/api/contexts", 1u8),
            EndpointRoute::new(HttpMethod::Post, "/b/messages/api/contexts", 2u8),
        ];
        assert_eq!(dispatch(&mut msg, &table), Some(2u8));
    }

    #[test]
    fn dispatch_ordering_specific_first() {
        // A specific template listed first must win over a generic one.
        let mut msg = Message::new("test");
        msg.set_meta("req.action", "delete");
        msg.set_meta("req.resource", "/b/vector/api/indexes/my-index");
        let table = [
            EndpointRoute::new(HttpMethod::Delete, "/b/vector/api/indexes/{name}", 1u8),
            EndpointRoute::new(HttpMethod::Delete, "/b/vector/api/{index}/{id}", 2u8),
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

    #[test]
    fn normalize_colon_style() {
        assert_eq!(
            normalize_template("/b/userportal/sessions/:hash"),
            "/b/userportal/sessions/{hash}"
        );
    }
}
