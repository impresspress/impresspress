//! Generic utility helpers shared across the runtime: time stamps, JSON
//! coercion, record-field access, auth-meta forwarding, URL encoding, and form
//! parsing. None of these are block-specific — infrastructure (routing,
//! pipeline, ui) and feature blocks alike depend on them, so they live at the
//! crate root (`crate::util`) rather than under `blocks/`.

use std::collections::HashMap;

use wafer_core::clients::database::Record;
/// Hashing/hex helpers re-exported from `wafer_block` (the single canonical
/// implementation). Re-exported here so the many `util::{hex_encode, sha256,
/// sha256_hex}` call sites across the blocks keep one import path.
pub use wafer_run::{hex_encode, sha256, sha256_hex};

/// Current UTC time as RFC 3339 string.
pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Current time in milliseconds (wasm-safe — uses chrono which uses js_sys on wasm32).
pub fn now_millis() -> u64 {
    chrono::Utc::now().timestamp_millis() as u64
}

/// Extract a single path id from a request, preferring the router-populated
/// `var` and falling back to stripping `prefix` off `msg.path()` and taking the
/// first remaining segment.
///
/// Native axum routing populates path variables (`msg.var("id")`); the
/// Cloudflare/browser adapters register a single block prefix and leave path
/// extraction to the handler, so the prefix-strip fallback covers them (and
/// direct handler calls in tests). Returns `""` when neither yields a value.
pub fn path_param<'a>(msg: &'a wafer_run::Message, var: &str, prefix: &str) -> &'a str {
    let v = msg.var(var);
    if !v.is_empty() {
        return v;
    }
    msg.path()
        .strip_prefix(prefix)
        .unwrap_or("")
        .split('/')
        .next()
        .unwrap_or("")
}

/// Convert a serde_json::json!({...}) value into a HashMap for the database client.
pub fn json_map(val: serde_json::Value) -> HashMap<String, serde_json::Value> {
    match val {
        serde_json::Value::Object(map) => map.into_iter().collect(),
        _ => HashMap::new(),
    }
}

/// Coerce a JSON value to `i64`, accepting both numbers and numeric strings.
///
/// The SQLite service stores auto-created (lazily added) columns as TEXT, so
/// integer values can round-trip as JSON strings (e.g. `"384"`). Try the
/// number first for backends/columns that round-trip faithfully, then fall
/// back to parsing the string.
pub fn json_as_i64(v: &serde_json::Value) -> Option<i64> {
    v.as_i64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

/// Coerce a JSON value to `u64`, accepting both numbers and numeric strings.
/// Same TEXT-column rationale as [`json_as_i64`].
pub fn json_as_u64(v: &serde_json::Value) -> Option<u64> {
    v.as_u64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

/// Extension trait for convenient field access on database Records.
///
/// The numeric accessors accept both JSON numbers and numeric strings
/// (see [`json_as_i64`]) so TEXT-stored values never silently collapse
/// to the zero default.
pub trait RecordExt {
    fn str_field(&self, key: &str) -> &str;
    /// Field as `i64`, defaulting to `0` when missing/non-numeric.
    fn i64_field(&self, key: &str) -> i64;
    /// Field as `Option<i64>` — use when the caller needs a non-zero
    /// default or must distinguish "absent" from "0".
    fn opt_i64_field(&self, key: &str) -> Option<i64>;
    /// Field as `u64`, defaulting to `0` when missing/non-numeric/negative.
    fn u64_field(&self, key: &str) -> u64;
    fn bool_field(&self, key: &str) -> bool;
}

impl RecordExt for Record {
    fn str_field(&self, key: &str) -> &str {
        self.data.get(key).and_then(|v| v.as_str()).unwrap_or("")
    }

    fn i64_field(&self, key: &str) -> i64 {
        self.opt_i64_field(key).unwrap_or(0)
    }

    fn opt_i64_field(&self, key: &str) -> Option<i64> {
        self.data.get(key).and_then(json_as_i64)
    }

    fn u64_field(&self, key: &str) -> u64 {
        self.data.get(key).and_then(json_as_u64).unwrap_or(0)
    }

    fn bool_field(&self, key: &str) -> bool {
        match self.data.get(key) {
            Some(serde_json::Value::Bool(b)) => *b,
            Some(serde_json::Value::Number(n)) => n.as_i64().unwrap_or(0) != 0,
            Some(serde_json::Value::String(s)) => s == "true" || s == "1",
            _ => false,
        }
    }
}

/// Get a field value as a string regardless of whether the DB returned it as string or number.
pub fn field_as_string(record: &Record, key: &str) -> String {
    match record.data.get(key) {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

/// Humanize a byte count for table/stat display: `105` → `"105 B"`,
/// `1_234` → `"1.2 KB"`, and so on up through GB (binary units).
pub fn format_bytes(bytes: i64) -> String {
    if bytes >= 1_073_741_824 {
        format!("{:.1} GB", bytes as f64 / 1_073_741_824.0)
    } else if bytes >= 1_048_576 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

/// Humanize an RFC 3339 timestamp for visible table text: `"2026-07-11 19:13"`
/// (UTC, minute precision) instead of the raw nanosecond-resolution string
/// [`now_rfc3339`] produces. Returns the input unchanged when it doesn't
/// parse, so a malformed stored value degrades to what we have rather than
/// hiding the row's timestamp — callers keep the full raw value in the
/// machine-readable `<time datetime=...>` attribute either way.
pub fn format_timestamp(rfc3339: &str) -> String {
    match chrono::DateTime::parse_from_rfc3339(rfc3339) {
        Ok(dt) => dt
            .with_timezone(&chrono::Utc)
            .format("%Y-%m-%d %H:%M")
            .to_string(),
        Err(_) => rfc3339.to_string(),
    }
}

/// Insert created_at + updated_at timestamps into a data map.
pub fn stamp_created(data: &mut std::collections::HashMap<String, serde_json::Value>) {
    let now = now_rfc3339();
    data.entry("created_at".to_string())
        .or_insert_with(|| serde_json::Value::String(now.clone()));
    data.entry("updated_at".to_string())
        .or_insert_with(|| serde_json::Value::String(now));
}

/// Insert updated_at timestamp into a data map.
pub fn stamp_updated(data: &mut std::collections::HashMap<String, serde_json::Value>) {
    data.insert(
        "updated_at".to_string(),
        serde_json::Value::String(now_rfc3339()),
    );
}

/// Check if the current user has admin role from the message metadata.
pub fn is_admin(msg: &wafer_run::Message) -> bool {
    msg.get_meta("auth.user_roles")
        .split(',')
        .any(|r| r.trim() == "admin")
}

/// Build a `Message` for an inter-block `ctx.call_block` dispatch, forwarding
/// the caller's auth identity from the originating request.
///
/// Sets the routing metas the receiving block's dispatcher keys off
/// (`req.action`, `req.resource`, `http.method`, `http.path`) and forwards
/// `auth.user_id` / `auth.user_email` / `auth.user_roles` when present, so the
/// callee sees the same caller identity it would on a direct HTTP request.
/// Empty auth fields are skipped rather than forwarded as empty strings.
///
/// All three identity fields forward on every call; a previous hand-rolled
/// copy dropped `auth.user_email` on read paths, an unexplained asymmetry that
/// this single source removes.
pub fn block_request(
    action: &str,
    method: &str,
    resource: &str,
    original: &wafer_run::Message,
) -> wafer_run::Message {
    let mut msg = wafer_run::Message::new(format!("{action}:{resource}"));
    msg.set_meta("req.action", action);
    msg.set_meta("req.resource", resource);
    msg.set_meta("http.method", method);
    msg.set_meta("http.path", resource);
    forward_auth_meta(&mut msg, original);
    msg
}

/// Forward the caller's auth identity (`auth.user_id` / `auth.user_email` /
/// `auth.user_roles`) from `original` onto `msg`, skipping empty fields.
pub fn forward_auth_meta(msg: &mut wafer_run::Message, original: &wafer_run::Message) {
    for key in ["auth.user_id", "auth.user_email", "auth.user_roles"] {
        let value = original.get_meta(key);
        if !value.is_empty() {
            msg.set_meta(key, value);
        }
    }
}

/// The RFC 3986 unreserved characters (`A-Z a-z 0-9 - _ . ~`) — the only bytes
/// [`url_path_encode`] leaves untouched. Built from `NON_ALPHANUMERIC` (which
/// encodes every non-alphanumeric ASCII byte) by removing the four unreserved
/// punctuation marks, so everything else (space → `%20`, `/` → `%2F`, …) is
/// percent-encoded.
const PATH_SEGMENT: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

/// Percent-encode a string for use as a URL path segment. Encodes everything
/// except RFC 3986 unreserved characters (`A-Z a-z 0-9 - _ . ~`). Spaces become
/// `%20`, `/` becomes `%2F`, etc. Use this when constructing `<a href>` URLs
/// from caller-supplied data (object keys, bucket names, etc.) — maud's HTML
/// escaping does NOT URL-encode.
pub fn url_path_encode(s: &str) -> String {
    percent_encoding::utf8_percent_encode(s, PATH_SEGMENT).to_string()
}

/// Validate a URL-type config value against SSRF attacks.
///
/// Empty values are allowed (clears the setting). Relative paths starting with
/// a single `/` are allowed. Otherwise the value must be HTTPS (or
/// `http://localhost` for local development), must not contain newlines (header
/// injection), and must not resolve to a private/internal/loopback IP.
///
/// Parses with [`url::Url`] rather than hand-rolled string splitting: `Url`
/// canonicalizes the scheme/host and `Url::host()` strips userinfo and port
/// and unwraps IPv6 brackets, so there is no separate "is this really
/// localhost" string check to go stale relative to the parse.
///
/// Single source of truth for the `InputType::Url` write rule, shared by every
/// config-value write surface — the admin variables page (`blocks::admin::ops`)
/// and the generic settings form (`ui::settings_form::save_settings`) — so a
/// value one surface rejects can't be smuggled in through another.
pub(crate) fn validate_url_value(value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Ok(());
    }
    // Allow relative paths. Checked before `Url::parse`, which errors on a
    // bare relative path (no scheme) rather than accepting it.
    if value.starts_with('/') && !value.starts_with("//") {
        return Ok(());
    }
    // Block newlines (header injection). `Url::parse` also rejects ASCII
    // control characters, but keep this as an explicit, readable check with
    // its own error message rather than relying on parse-error wording.
    if value.contains('\n') || value.contains('\r') {
        return Err("URL must not contain newlines".to_string());
    }

    let parsed = url::Url::parse(value).map_err(|e| format!("invalid URL: {e}"))?;

    // Must be https:// or http://localhost for dev. `host()` is the
    // canonical, userinfo-stripped host (e.g. `https://user@localhost/`
    // still yields `Host::Domain("localhost")` here), so there is no
    // separate prefix test that a crafted authority can dodge. The dev
    // exception is scoped to the `localhost` *domain* specifically:
    // loopback IPs (`127.0.0.1`, `::1`) are intentionally NOT granted it —
    // they're rejected below by the private/loopback-IP block regardless,
    // so exempting them here would be misleading. `Host::Ipv6` also never
    // matches the bare string `"::1"` (it serializes bracketed, `"[::1]"`),
    // so a string-based check would silently never apply anyway.
    let is_dev_localhost =
        matches!(parsed.host(), Some(url::Host::Domain(h)) if h.eq_ignore_ascii_case("localhost"));
    match parsed.scheme() {
        "https" => {}
        "http" if is_dev_localhost => {}
        _ => {
            return Err("URL must use HTTPS (or http://localhost for development)".to_string());
        }
    }

    // Check for private/internal IPs using the parsed host, which is
    // guaranteed to have userinfo and port stripped and IPv6 brackets
    // removed — unlike the old string-split extraction.
    match parsed.host() {
        Some(url::Host::Ipv4(v4)) => {
            let is_blocked = v4.is_private()       // 10.x, 172.16-31.x, 192.168.x
                || v4.is_loopback()                // 127.x
                || v4.is_link_local()              // 169.254.x
                || v4.octets()[0] == 0; // 0.0.0.0/8
            if is_blocked {
                return Err("URL must not point to private/internal IP addresses".to_string());
            }
        }
        Some(url::Host::Ipv6(v6)) => {
            if v6.is_loopback() {
                return Err("URL must not point to loopback address".to_string());
            }
            // Block IPv4-mapped IPv6 addresses (::ffff:10.x.x.x etc.)
            if let Some(v4) = v6.to_ipv4_mapped() {
                if v4.is_private() || v4.is_loopback() || v4.is_link_local() {
                    return Err("URL must not point to private/internal IP addresses".to_string());
                }
            }
        }
        Some(url::Host::Domain(_)) | None => {}
    }
    Ok(())
}

/// Masked placeholder shown in place of a sensitive value.
pub(crate) const MASKED_VALUE: &str = "********";

/// SEC-060: a config value is sensitive when it's explicitly flagged
/// sensitive **or** the key follows the `_SECRET` / `_KEY` suffix
/// convention. "Explicitly flagged" means different things on each caller's
/// substrate — the admin Variables table's DB `sensitive` column for ad hoc
/// rows, or a declared [`ConfigVar`](wafer_run::ConfigVar)'s
/// `InputType::Password` for the generic settings form — so callers pass
/// their own flag in as `1`/`0`. The suffix half of the rule is what both
/// sides share: masking on the flag alone leaked a `*_SECRET` value whenever
/// a var/row wasn't explicitly marked.
///
/// Single source of truth for "is this key sensitive", used by both the
/// admin Variables page (`blocks::admin::ops`, re-exported from here) and
/// the generic ConfigVar-driven settings form (`ui::settings_form`) so the
/// two admin surfaces can't disagree on what gets redacted.
pub(crate) fn is_sensitive_key(key: &str, sensitive_flag: i64) -> bool {
    sensitive_flag == 1 || key.ends_with("_SECRET") || key.ends_with("_KEY")
}

/// Percent-encode a string for use as an OAuth / `application/x-www-form-urlencoded`
/// query parameter or form-body value. Delegates to
/// [`url::form_urlencoded::byte_serialize`] which encodes spaces as `+` and
/// everything outside the unreserved set as `%XX`. This is the single form
/// encoder — use it for OAuth params, HTTP form bodies, and any value placed in
/// a query string (verification/reset links, Mailgun fields, …).
pub fn urlencode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

/// Parse a URL-encoded form body (htmx default) into a HashMap. Thin wrapper
/// over [`url::form_urlencoded::parse`], which handles `+`→space and `%XX`
/// decoding. Repeated keys collapse to the last value (the existing behaviour).
pub fn parse_form_body(data: &[u8]) -> HashMap<String, String> {
    url::form_urlencoded::parse(data).into_owned().collect()
}

/// Parse a request body as either JSON or URL-encoded form into a JSON Value.
///
/// Inspects the first non-whitespace byte: `{` → JSON, anything else →
/// URL-encoded form (then promoted to a flat object). Lets one handler
/// accept both htmx form posts and programmatic JSON clients without
/// duplicating parse logic.
pub fn parse_body_value(data: &[u8]) -> serde_json::Value {
    let trimmed_start = data
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(0);
    if data.get(trimmed_start) == Some(&b'{') || data.get(trimmed_start) == Some(&b'[') {
        serde_json::from_slice(data).unwrap_or(serde_json::Value::Null)
    } else {
        let mut obj = serde_json::Map::new();
        for (k, v) in parse_form_body(data) {
            obj.insert(k, serde_json::Value::String(v));
        }
        serde_json::Value::Object(obj)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_form_body_decodes_plus_to_space() {
        let parsed = parse_form_body(b"k=a+b");
        assert_eq!(parsed.get("k"), Some(&"a b".to_string()));
    }

    #[test]
    fn parse_form_body_decodes_percent_escapes() {
        let parsed = parse_form_body(b"k=a%2Fb");
        assert_eq!(parsed.get("k"), Some(&"a/b".to_string()));
    }

    #[test]
    fn parse_form_body_multiple_pairs_and_decoded_keys() {
        let parsed = parse_form_body(b"first+name=John+Doe&email=a%40b.com");
        assert_eq!(parsed.get("first name"), Some(&"John Doe".to_string()));
        assert_eq!(parsed.get("email"), Some(&"a@b.com".to_string()));
    }

    #[test]
    fn now_rfc3339_parses() {
        let s = now_rfc3339();
        let _: chrono::DateTime<chrono::Utc> = s.parse().expect("rfc3339 round-trip");
    }

    #[test]
    fn format_bytes_humanizes_each_magnitude() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(105), "105 B");
        assert_eq!(format_bytes(1024), "1.0 KB");
        assert_eq!(format_bytes(1_234), "1.2 KB");
        assert_eq!(format_bytes(5 * 1_048_576), "5.0 MB");
        assert_eq!(format_bytes(2_500_000_000), "2.3 GB");
    }

    #[test]
    fn format_timestamp_humanizes_rfc3339_to_utc_minutes() {
        // Nanosecond-resolution output of `now_rfc3339` (chrono to_rfc3339).
        assert_eq!(
            format_timestamp("2026-07-11T19:13:45.123456789+00:00"),
            "2026-07-11 19:13"
        );
        // Z-suffixed and offset forms normalize to UTC.
        assert_eq!(format_timestamp("2026-05-06T10:00:00Z"), "2026-05-06 10:00");
        assert_eq!(
            format_timestamp("2026-05-06T12:30:00+02:00"),
            "2026-05-06 10:30"
        );
    }

    #[test]
    fn format_timestamp_passes_unparseable_values_through() {
        assert_eq!(format_timestamp("not a date"), "not a date");
        assert_eq!(format_timestamp(""), "");
    }

    #[test]
    fn urlencode_space_becomes_plus() {
        assert_eq!(urlencode("a b"), "a+b");
    }

    #[test]
    fn urlencode_special_chars() {
        // Slash and ampersand must be percent-encoded in form values.
        let encoded = urlencode("a/b&c=d");
        assert!(!encoded.contains('/'));
        assert!(!encoded.contains('&'));
        assert!(!encoded.contains('='));
    }

    #[test]
    fn url_path_encode_basic() {
        assert_eq!(url_path_encode("hello"), "hello");
        assert_eq!(url_path_encode("hello world"), "hello%20world");
        assert_eq!(url_path_encode("a+b=c&d"), "a%2Bb%3Dc%26d");
        assert_eq!(url_path_encode("a/b"), "a%2Fb");
        assert_eq!(url_path_encode("café"), "caf%C3%A9");
    }

    #[test]
    fn urlencode_form_value_round_trips_through_parse_form_body() {
        // `urlencode` is the single form encoder (space → '+', reserved → %XX).
        assert_eq!(urlencode("hello world"), "hello+world");
        assert_eq!(urlencode("a+b=c&d"), "a%2Bb%3Dc%26d");
        assert_eq!(urlencode("a/b"), "a%2Fb");
        assert_eq!(urlencode("café"), "caf%C3%A9");
        // Round-trip: encode → form body → parse decodes '+' back to ' '.
        let encoded = urlencode("hello world & friends");
        let parsed = parse_form_body(format!("k={encoded}").as_bytes());
        assert_eq!(parsed.get("k"), Some(&"hello world & friends".to_string()));
    }

    #[test]
    fn test_now_rfc3339_format() {
        let ts = now_rfc3339();
        assert!(ts.contains('T'), "RFC 3339 must contain 'T' separator");
        assert!(
            ts.contains('+') || ts.ends_with('Z'),
            "RFC 3339 must have timezone"
        );
    }

    #[test]
    fn test_json_map_from_object() {
        let val = serde_json::json!({"name": "Alice", "age": 30});
        let map = json_map(val);
        assert_eq!(map.get("name").unwrap(), "Alice");
        assert_eq!(map.get("age").unwrap(), 30);
    }

    #[test]
    fn test_json_map_from_non_object() {
        let map = json_map(serde_json::json!("not an object"));
        assert!(map.is_empty());
        let map = json_map(serde_json::json!(42));
        assert!(map.is_empty());
        let map = json_map(serde_json::json!(null));
        assert!(map.is_empty());
    }

    #[test]
    fn test_record_ext_str_field() {
        let mut data = HashMap::new();
        data.insert("name".to_string(), serde_json::json!("Alice"));
        data.insert("count".to_string(), serde_json::json!(42));
        let record = Record {
            id: "1".to_string(),
            data,
        };

        assert_eq!(record.str_field("name"), "Alice");
        assert_eq!(record.str_field("missing"), "");
        assert_eq!(record.str_field("count"), ""); // number is not a string
    }

    #[test]
    fn test_record_ext_i64_field() {
        let mut data = HashMap::new();
        data.insert("count".to_string(), serde_json::json!(42));
        data.insert("name".to_string(), serde_json::json!("Alice"));
        let record = Record {
            id: "1".to_string(),
            data,
        };

        assert_eq!(record.i64_field("count"), 42);
        assert_eq!(record.i64_field("missing"), 0);
        assert_eq!(record.i64_field("name"), 0);
    }

    /// Regression: SQLite stores auto-created columns as TEXT, so integer
    /// values round-trip as JSON strings. `i64_field` used to silently
    /// return 0 for them (the silent-zero bug class — e.g. quota
    /// enforcement ignoring TEXT-stored per-user overrides).
    #[test]
    fn test_record_ext_i64_field_parses_text_stored_numbers() {
        let mut data = HashMap::new();
        data.insert("count".to_string(), serde_json::json!("42"));
        data.insert("negative".to_string(), serde_json::json!("-7"));
        data.insert("not_a_number".to_string(), serde_json::json!("abc"));
        let record = Record {
            id: "1".to_string(),
            data,
        };

        assert_eq!(
            record.i64_field("count"),
            42,
            "TEXT-stored \"42\" must parse, not silently default to 0"
        );
        assert_eq!(record.i64_field("negative"), -7);
        assert_eq!(record.i64_field("not_a_number"), 0);
    }

    #[test]
    fn test_record_ext_opt_i64_field() {
        let mut data = HashMap::new();
        data.insert("num".to_string(), serde_json::json!(7));
        data.insert("text_num".to_string(), serde_json::json!("9"));
        data.insert("junk".to_string(), serde_json::json!("x"));
        let record = Record {
            id: "1".to_string(),
            data,
        };

        assert_eq!(record.opt_i64_field("num"), Some(7));
        assert_eq!(record.opt_i64_field("text_num"), Some(9));
        assert_eq!(record.opt_i64_field("junk"), None);
        assert_eq!(
            record.opt_i64_field("missing"),
            None,
            "absent field must be distinguishable from 0"
        );
    }

    #[test]
    fn test_record_ext_u64_field() {
        let mut data = HashMap::new();
        data.insert("dims".to_string(), serde_json::json!(384));
        data.insert("text_dims".to_string(), serde_json::json!("384"));
        data.insert("negative".to_string(), serde_json::json!(-2));
        data.insert("text_negative".to_string(), serde_json::json!("-2"));
        let record = Record {
            id: "1".to_string(),
            data,
        };

        assert_eq!(record.u64_field("dims"), 384);
        assert_eq!(record.u64_field("text_dims"), 384);
        assert_eq!(record.u64_field("negative"), 0);
        assert_eq!(record.u64_field("text_negative"), 0);
        assert_eq!(record.u64_field("missing"), 0);
    }

    #[test]
    fn test_json_as_i64_and_u64() {
        assert_eq!(json_as_i64(&serde_json::json!(5)), Some(5));
        assert_eq!(json_as_i64(&serde_json::json!("5")), Some(5));
        assert_eq!(json_as_i64(&serde_json::json!("-5")), Some(-5));
        assert_eq!(json_as_i64(&serde_json::json!("nope")), None);
        assert_eq!(json_as_i64(&serde_json::json!(null)), None);
        assert_eq!(json_as_u64(&serde_json::json!(5)), Some(5));
        assert_eq!(json_as_u64(&serde_json::json!("5")), Some(5));
        assert_eq!(json_as_u64(&serde_json::json!("-5")), None);
        assert_eq!(json_as_u64(&serde_json::json!(-5)), None);
    }

    #[test]
    fn test_record_ext_bool_field() {
        let mut data = HashMap::new();
        data.insert("active".to_string(), serde_json::json!(true));
        data.insert("disabled".to_string(), serde_json::json!(false));
        let record = Record {
            id: "1".to_string(),
            data,
        };

        assert!(record.bool_field("active"));
        assert!(!record.bool_field("disabled"));
        assert!(!record.bool_field("missing"));
    }

    #[test]
    fn test_field_as_string_variants() {
        let mut data = HashMap::new();
        data.insert("str".to_string(), serde_json::json!("hello"));
        data.insert("num".to_string(), serde_json::json!(42));
        data.insert("bool".to_string(), serde_json::json!(true));
        let record = Record {
            id: "1".to_string(),
            data,
        };

        assert_eq!(field_as_string(&record, "str"), "hello");
        assert_eq!(field_as_string(&record, "num"), "42");
        assert_eq!(field_as_string(&record, "bool"), "");
        assert_eq!(field_as_string(&record, "missing"), "");
    }

    #[test]
    fn test_stamp_created() {
        let mut data = HashMap::new();
        stamp_created(&mut data);
        assert!(data.contains_key("created_at"));
        assert!(data.contains_key("updated_at"));

        // Should not overwrite existing values
        let mut data2 = HashMap::new();
        data2.insert("created_at".to_string(), serde_json::json!("custom"));
        stamp_created(&mut data2);
        assert_eq!(data2.get("created_at").unwrap(), "custom");
    }

    #[test]
    fn test_stamp_updated() {
        let mut data = HashMap::new();
        data.insert("updated_at".to_string(), serde_json::json!("old"));
        stamp_updated(&mut data);
        assert_ne!(data.get("updated_at").unwrap(), "old");
    }

    #[test]
    fn test_hex_encode() {
        assert_eq!(hex_encode(&[0x00, 0xff, 0x0a, 0xbc]), "00ff0abc");
        assert_eq!(hex_encode(&[]), "");
    }

    #[test]
    fn test_sha256_hex_deterministic() {
        let hash1 = sha256_hex(b"hello");
        let hash2 = sha256_hex(b"hello");
        assert_eq!(hash1, hash2);
        assert_eq!(hash1.len(), 64); // 32 bytes = 64 hex chars

        let hash3 = sha256_hex(b"world");
        assert_ne!(hash1, hash3);
    }

    #[test]
    fn test_sha256_known_value() {
        // SHA-256 of empty string
        let hash = sha256_hex(b"");
        assert_eq!(
            hash,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn block_request_sets_routing_metas_and_kind() {
        let original = wafer_run::Message::new("retrieve:/x");
        let msg = block_request("create", "POST", "/b/messages/api/x", &original);
        assert_eq!(msg.get_meta("req.action"), "create");
        assert_eq!(msg.get_meta("req.resource"), "/b/messages/api/x");
        assert_eq!(msg.get_meta("http.method"), "POST");
        assert_eq!(msg.get_meta("http.path"), "/b/messages/api/x");
    }

    #[test]
    fn block_request_forwards_all_three_auth_fields() {
        // Both read and write paths must forward the full caller identity —
        // the previous hand-rolled list path dropped `auth.user_email`.
        let mut original = wafer_run::Message::new("get:/x");
        original.set_meta("auth.user_id", "u-1");
        original.set_meta("auth.user_email", "u@example.com");
        original.set_meta("auth.user_roles", "user,beta");

        let msg = block_request("retrieve", "GET", "/b/messages/api/x", &original);
        assert_eq!(msg.get_meta("auth.user_id"), "u-1");
        assert_eq!(msg.get_meta("auth.user_email"), "u@example.com");
        assert_eq!(msg.get_meta("auth.user_roles"), "user,beta");
    }

    #[test]
    fn forward_auth_meta_skips_empty_fields() {
        let mut original = wafer_run::Message::new("get:/x");
        original.set_meta("auth.user_id", "u-1");
        // email + roles unset on the original.

        let mut msg = wafer_run::Message::new("get:/y");
        forward_auth_meta(&mut msg, &original);
        assert_eq!(msg.get_meta("auth.user_id"), "u-1");
        // Absent fields are not materialized as empty strings.
        assert_eq!(msg.get_meta("auth.user_email"), "");
        assert_eq!(msg.get_meta("auth.user_roles"), "");
    }

    #[test]
    fn validate_url_value_blocks_ssrf_and_allows_safe() {
        assert!(validate_url_value("").is_ok());
        assert!(validate_url_value("/relative/path").is_ok());
        assert!(validate_url_value("https://example.com/ok").is_ok());
        assert!(validate_url_value("http://localhost:8080").is_ok());
        // SSRF vectors.
        assert!(validate_url_value("http://example.com").is_err()); // not https
        assert!(validate_url_value("https://10.0.0.1/admin").is_err());
        assert!(validate_url_value("https://192.168.1.1").is_err());
        assert!(validate_url_value("https://127.0.0.1").is_err());
        assert!(validate_url_value("https://example.com\r\nHost: evil").is_err());
    }

    /// Bypass #1: a raw `starts_with("http://localhost")` prefix test treats
    /// any host merely beginning with the string "localhost" as exempt from
    /// the HTTPS requirement, letting external plain-HTTP hosts through.
    #[test]
    fn rejects_localhost_prefixed_external_host_over_http() {
        assert!(validate_url_value("http://localhost.evil.com/").is_err());
        assert!(validate_url_value("http://localhostfoo/").is_err());
    }

    /// Bypass #2: hand-rolled host extraction never stripped userinfo, so
    /// `user@10.0.0.1` failed to parse as an `IpAddr` and the private-IP
    /// block was skipped entirely. Combined with bypass #1, a userinfo of
    /// literally "localhost" also smuggled a private IP in over plain HTTP.
    #[test]
    fn rejects_userinfo_masked_private_ip() {
        assert!(validate_url_value("https://user@10.0.0.1/").is_err());
        assert!(validate_url_value("http://localhost@10.0.0.1/").is_err());
    }

    #[test]
    fn still_allows_plain_https_and_real_localhost() {
        assert!(validate_url_value("https://api.example.com/x").is_ok());
        assert!(validate_url_value("http://localhost:8080/x").is_ok());
    }

    /// The http-localhost dev exception is scoped to the `localhost` domain
    /// only. Loopback IPs are NOT granted it — they fall through to (and are
    /// rejected by) the private/loopback-IP block below regardless. IPv6
    /// loopback in particular would never have matched a bare `"::1"` string
    /// check anyway, since `Url::host_str()` serializes it bracketed.
    #[test]
    fn rejects_loopback_ips_over_http() {
        assert!(validate_url_value("http://[::1]/x").is_err());
        assert!(validate_url_value("http://127.0.0.1:8080/x").is_err());
    }

    /// Userinfo-stripped host must still be `localhost` the domain, not
    /// merely contain the substring — `http://localhost@evil.com` has host
    /// `evil.com`, so it must be rejected (not confused with the userinfo).
    #[test]
    fn rejects_userinfo_localhost_with_external_host() {
        assert!(validate_url_value("http://localhost@evil.com").is_err());
    }

    #[test]
    fn is_sensitive_key_honors_flag_and_suffix() {
        // Flag set → sensitive regardless of name.
        assert!(is_sensitive_key("PLAIN", 1));
        // SEC-060: suffix makes it sensitive even when the flag is clear.
        assert!(is_sensitive_key("STRIPE_SECRET", 0));
        assert!(is_sensitive_key("JWT_KEY", 0));
        // Neither flag nor suffix → not sensitive.
        assert!(!is_sensitive_key("SITE_NAME", 0));
    }
}
