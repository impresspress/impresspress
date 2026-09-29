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

/// Whether a stored BOOLEAN-ish column is set, for every shape a backend or a
/// fixture can hand back: a JSON bool, any non-zero number, or the strings
/// `"true"` / `"1"`.
///
/// THE truth table for such a column, and the reason it is a free function
/// rather than only a [`RecordExt`] method: three readers of
/// `impresspress__admin__variables.sensitive` each had their own, and they
/// disagreed exactly where it mattered. `RecordExt::bool_field` accepted all
/// the shapes above; [`is_sensitive_key`] compared an `i64` to `1`; and
/// `cache_key::row_is_sensitive` converted with [`json_as_i64`], which yields
/// `None` for a JSON bool and for `"true"`. So a row whose flag held `2`,
/// `true` or `"true"` read as flagged by the repair pass — which therefore
/// skipped it — while the settings API served it in the clear and the KV cache
/// judged it cacheable. One predicate, used by every reader, is what stops
/// that class of disagreement rather than the one instance of it.
///
/// The canonical stored form is still the integer `1`: the column is
/// `INTEGER NOT NULL DEFAULT 0` on both backends and
/// `VariableRow::to_data` writes `i64::from(bool)`. This predicate exists to
/// read what is already there, not to license new spellings.
pub fn flag_is_set(v: &serde_json::Value) -> bool {
    match v {
        serde_json::Value::Bool(b) => *b,
        // `as_i64` alone is `None` for a float, so `1.0` read as UNSET while
        // the doc said "any non-zero number". That is a shape this change set
        // anticipates elsewhere — `variable_is_exportable` lists it, and
        // `load_rows` names it — and getting it wrong was doubly bad: the row
        // would have been served in the clear AND rewritten on every boot by a
        // repair pass that could never make `flag_is_canonical_one` true. The
        // truth table has to be total over the numbers a backend can produce.
        serde_json::Value::Number(n) => n
            .as_i64()
            .map_or_else(|| n.as_f64().unwrap_or(0.0) != 0.0, |i| i != 0),
        serde_json::Value::String(s) => s == "true" || s == "1",
        _ => false,
    }
}

/// Extension trait for convenient field access on database Records.
///
/// The numeric accessors accept both JSON numbers and numeric strings
/// (see [`json_as_i64`]) so TEXT-stored values never silently collapse
/// to the zero default.
pub trait RecordExt {
    /// A `TEXT` column as the string it holds, `""` when missing or when the
    /// value is not a JSON string.
    ///
    /// A JSON-encoded column is *not* one of these: the SQLite and browser
    /// backends re-parse JSON-shaped text on read, so such a column arrives as
    /// a `Value::Object` and this accessor answers `""` for a payload that is
    /// plainly there. Reach for [`Self::json_value_field`],
    /// [`Self::json_object_field`] or [`Self::json_text_field`] instead.
    fn str_field(&self, key: &str) -> &str;
    /// Field as `i64`, defaulting to `0` when missing/non-numeric.
    fn i64_field(&self, key: &str) -> i64;
    /// Field as `Option<i64>` — use when the caller needs a non-zero
    /// default or must distinguish "absent" from "0".
    fn opt_i64_field(&self, key: &str) -> Option<i64>;
    /// Field as `u64`, defaulting to `0` when missing/non-numeric/negative.
    fn u64_field(&self, key: &str) -> u64;
    fn bool_field(&self, key: &str) -> bool;

    /// A nullable `TEXT` column as `Option<String>`: a SQL `NULL` (JSON
    /// `null`), an absent key and a non-string value all read as `None`.
    ///
    /// [`Self::str_field`] collapses all three onto `""`, which would make a
    /// non-nullable `"string"` schema look correct while erasing the
    /// difference between "never set" and "set to the empty string".
    fn opt_str_field(&self, key: &str) -> Option<String>;

    /// A JSON-encoded `TEXT` column as the value it encodes, whichever way
    /// the backend returned it.
    ///
    /// Such columns are written by [`serde_json::to_string`] and read back as
    /// a real value by the SQLite backend (`row_to_record` sniffs JSON-shaped
    /// text) but as the literal string by Postgres and D1. Normalizing here is
    /// what lets a view declare `object` / `array` truthfully on all three.
    /// `Null` when the column is absent or does not decode.
    fn json_value_field(&self, key: &str) -> serde_json::Value;

    /// A JSON-encoded `TEXT` column that holds an object, or an empty map
    /// when it holds anything else.
    fn json_object_field(&self, key: &str) -> serde_json::Map<String, serde_json::Value>;

    /// A JSON-encoded `TEXT` column that holds an array, or empty when it
    /// holds anything else.
    fn json_array_field(&self, key: &str) -> Vec<serde_json::Value>;

    /// A JSON-encoded `TEXT` column as its JSON *text*, for a caller that
    /// carries the payload onward rather than inspecting it — re-persisting it
    /// on another row, or rendering it.
    ///
    /// A backend that handed back the raw string returns it verbatim (it is
    /// already the encoded text); a backend that re-parsed it gets the value
    /// re-encoded, which can reorder object keys but never loses a field. `""`
    /// when the column is absent, so an `is_empty()` guard still means
    /// "nothing stored".
    fn json_text_field(&self, key: &str) -> String;

    /// A JSON-encoded `TEXT` column that holds an array of strings. Elements
    /// that are not strings are dropped; a column holding anything but an
    /// array reads as empty. These columns are advisory metadata, not an
    /// authorization input, so an unexpected shape degrades rather than errs.
    fn string_list_field(&self, key: &str) -> Vec<String>;
}

/// The one implementation: every `Record` shape the runtime hands out —
/// `wafer_core::clients::database::Record` under WRAP and
/// `wafer_core::interfaces::database::service::Record` at boot — carries a
/// `data: HashMap<String, Value>` column map, and the platform-state codecs
/// decode that map for both. Implemented on the map so the two flavours
/// share one accessor set; the `Record` impl below forwards to it.
impl RecordExt for HashMap<String, serde_json::Value> {
    fn str_field(&self, key: &str) -> &str {
        self.get(key).and_then(|v| v.as_str()).unwrap_or("")
    }

    fn i64_field(&self, key: &str) -> i64 {
        self.opt_i64_field(key).unwrap_or(0)
    }

    fn opt_i64_field(&self, key: &str) -> Option<i64> {
        self.get(key).and_then(json_as_i64)
    }

    fn u64_field(&self, key: &str) -> u64 {
        self.get(key).and_then(json_as_u64).unwrap_or(0)
    }

    fn bool_field(&self, key: &str) -> bool {
        self.get(key).is_some_and(flag_is_set)
    }

    fn opt_str_field(&self, key: &str) -> Option<String> {
        match self.get(key) {
            Some(serde_json::Value::String(value)) => Some(value.clone()),
            _ => None,
        }
    }

    fn json_value_field(&self, key: &str) -> serde_json::Value {
        match self.get(key) {
            Some(serde_json::Value::String(raw)) => {
                serde_json::from_str(raw).unwrap_or(serde_json::Value::Null)
            }
            Some(value) => value.clone(),
            None => serde_json::Value::Null,
        }
    }

    fn json_object_field(&self, key: &str) -> serde_json::Map<String, serde_json::Value> {
        match self.json_value_field(key) {
            serde_json::Value::Object(map) => map,
            _ => serde_json::Map::new(),
        }
    }

    fn json_array_field(&self, key: &str) -> Vec<serde_json::Value> {
        match self.json_value_field(key) {
            serde_json::Value::Array(items) => items,
            _ => Vec::new(),
        }
    }

    fn json_text_field(&self, key: &str) -> String {
        match self.get(key) {
            Some(serde_json::Value::String(raw)) => raw.clone(),
            Some(serde_json::Value::Null) | None => String::new(),
            Some(value) => value.to_string(),
        }
    }

    fn string_list_field(&self, key: &str) -> Vec<String> {
        self.json_array_field(key)
            .into_iter()
            .filter_map(|item| match item {
                serde_json::Value::String(value) => Some(value),
                _ => None,
            })
            .collect()
    }
}

impl RecordExt for Record {
    fn str_field(&self, key: &str) -> &str {
        self.data.str_field(key)
    }

    fn i64_field(&self, key: &str) -> i64 {
        self.data.i64_field(key)
    }

    fn opt_i64_field(&self, key: &str) -> Option<i64> {
        self.data.opt_i64_field(key)
    }

    fn u64_field(&self, key: &str) -> u64 {
        self.data.u64_field(key)
    }

    fn bool_field(&self, key: &str) -> bool {
        self.data.bool_field(key)
    }

    fn opt_str_field(&self, key: &str) -> Option<String> {
        self.data.opt_str_field(key)
    }

    fn json_value_field(&self, key: &str) -> serde_json::Value {
        self.data.json_value_field(key)
    }

    fn json_object_field(&self, key: &str) -> serde_json::Map<String, serde_json::Value> {
        self.data.json_object_field(key)
    }

    fn json_array_field(&self, key: &str) -> Vec<serde_json::Value> {
        self.data.json_array_field(key)
    }

    fn json_text_field(&self, key: &str) -> String {
        self.data.json_text_field(key)
    }

    fn string_list_field(&self, key: &str) -> Vec<String> {
        self.data.string_list_field(key)
    }
}

/// Parse `column` of `record` into the enum that defines its value set.
///
/// The one decode door for an enum'd column, crate-wide. A column whose
/// values are a fixed set has exactly one type that names them and exactly
/// one function that turns the stored text into that type, so a value the
/// contract does not define cannot be read as a default by one caller and
/// as an error by another — which is what having two doors
/// (`products::contracts::enum_column` and `products::repo::offers::wire_enum`,
/// which disagreed on the empty column) produced.
///
/// A stored value outside the set is a data-integrity fault: it is reported
/// as [`ErrorCode::Internal`](wafer_run::ErrorCode::Internal) naming the row,
/// the column and the value — the three things an operator needs to go and
/// look at the row — and is never mapped onto a variant. An absent column, a
/// SQL `NULL` and an empty string all read as `""`. That is refused unless
/// `T` names `""` as a variant (`#[serde(rename = "")]`), which an enum does
/// when its column's `NOT NULL DEFAULT ''` is itself a state, e.g.
/// `products::contracts::SubscriptionStatus::Unset`. A column that is empty
/// on some rows without `""` being a state of its enum wants
/// [`enum_column_or`] and a typed fallback.
pub fn enum_column<T: serde::de::DeserializeOwned>(
    record: &Record,
    column: &str,
) -> Result<T, wafer_run::WaferError> {
    let value = record.str_field(column);
    serde_json::from_value(serde_json::Value::String(value.to_string())).map_err(|_| {
        wafer_run::WaferError::new(
            wafer_run::ErrorCode::Internal,
            format!(
                "row {} holds {column} {value:?}, which the contract does not define",
                record.id
            ),
        )
    })
}

/// [`enum_column`] for a column that is legitimately empty on some rows.
///
/// `empty` is a **typed variant**, not a spelling: the `&str` fallback this
/// replaces let a caller name a default the enum did not define, so a
/// mis-typed fallback failed at the same place — and with the same message —
/// as genuinely corrupt data. Only the empty case takes the fallback; a
/// non-empty value outside the set is still the fault [`enum_column`]
/// reports.
pub fn enum_column_or<T: serde::de::DeserializeOwned>(
    record: &Record,
    column: &str,
    empty: T,
) -> Result<T, wafer_run::WaferError> {
    if record.str_field(column).is_empty() {
        return Ok(empty);
    }
    enum_column(record, column)
}

/// An enum as the string it is stored, filtered and published as.
///
/// The counterpart of [`enum_column`], for the places a typed value has to
/// go back out as text: a rendered badge, an `href` segment, a hidden form
/// field the page's own JavaScript posts back. Serializing is what keeps
/// those the same spelling the column holds without a per-variant label
/// table beside the serde one to drift from it — the duplication
/// `RefundReason::as_str` is the standing example of.
pub fn wire_str<T: serde::Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(text)) => text,
        // Unreachable for the unit-variant enums this exists for; a type
        // that serializes to something else has no single wire spelling to
        // render and should not be asked for one.
        _ => String::new(),
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

/// Group a count with thousands separators for visible text: `10_000` →
/// `"10,000"`, `999` → `"999"`, `-1_234` → `"-1,234"`.
///
/// For prose and stat text that states a limit, where an ungrouped `10000`
/// reads as a different number at a glance. Table cells that render a raw
/// count are left alone; this is not a general number formatter.
pub fn format_count(count: i64) -> String {
    let negative = count < 0;
    let digits = count.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    if negative {
        out.push('-');
    }
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Humanize an RFC 3339 timestamp for visible table text: `"2026-07-11 19:13"`
/// (UTC, minute precision) instead of the raw nanosecond-resolution string
/// [`now_rfc3339`] produces. Returns the input unchanged when it doesn't
/// parse, so a malformed stored value degrades to what we have rather than
/// hiding the row's timestamp. `ui::components::timestamp` pairs this text
/// with the machine-readable instant in a `<time datetime=...>`.
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
    // A query string is split off the path and decoded into `req.query.*`,
    // exactly as `wafer_block::http_codec::request_from_http` does for a real
    // request. Without this the whole `path?a=b` string landed in
    // `req.resource`, where `endpoint_match::dispatch` compares it segment by
    // segment against the route template — so `contexts/{id}/entries?kind=message`
    // matched no row and the callee answered 404, while `msg.query("kind")`
    // (which reads `req.query.kind`, never `req.resource`) read `""` anyway.
    // Both `blocks::llm` calls that carry a filter were affected: the model
    // history and the chat sidebar came back empty on every request.
    let (path, query) = resource.split_once('?').unwrap_or((resource, ""));
    let mut msg = wafer_run::Message::new(format!("{action}:{path}"));
    msg.set_meta("req.action", action);
    msg.set_meta("req.resource", path);
    msg.set_meta("http.method", method);
    msg.set_meta("http.path", path);
    msg.set_meta("http.raw_query", query);
    for (name, value) in parse_form_body(query.as_bytes()) {
        msg.set_meta(format!("http.query.{name}"), value.clone());
        msg.set_meta(format!("req.query.{name}"), value);
    }
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

/// Percent-decode a URL path segment — the inverse of [`url_path_encode`].
///
/// Every part of impresspress that reads an id or key back out of a path owes
/// this: adapters hand the pipeline the path exactly as it appeared on the
/// wire (axum's `Uri::path`, `url::Url::path` on Cloudflare, `Url.pathname`
/// in the Service Worker), so an id encoded into an `href` arrives with its
/// escapes intact. Route matching has to happen on that encoded form — a
/// decoded `/` would split the route — so the decode belongs on the extracted
/// value, which is what `endpoint_match::dispatch` does with every variable
/// it binds.
///
/// A sequence that does not decode to valid UTF-8 yields `s` unchanged.
/// Non-text bytes are never a record id or an object key here, so the
/// alternatives are to lose them silently (`decode_utf8_lossy` substitutes
/// U+FFFD) or to reject the path outright; passing the segment through leaves
/// the failure where the caller can read it — a lookup that answers "not
/// found".
pub fn url_path_decode(s: &str) -> String {
    percent_encoding::percent_decode_str(s)
        .decode_utf8()
        .map(|decoded| decoded.into_owned())
        .unwrap_or_else(|_| s.to_string())
}

/// Validate a URL-type config value against SSRF attacks.
///
/// Empty values are allowed (clears the setting). Relative paths starting with
/// a single `/` are allowed. Otherwise the value must be HTTPS (or
/// `http://localhost` for local development), must not contain newlines (header
/// injection), and must not resolve to a private/internal/loopback IP —
/// including the IPv6 unique-local (`fc00::/7`) and link-local (`fe80::/10`)
/// ranges alongside their IPv4 counterparts.
///
/// Literal IP classification is delegated to the shared `wafer-net-security`
/// predicates ([`wafer_net_security::is_blocked_ipv4`] /
/// [`is_blocked_ipv6`](wafer_net_security::is_blocked_ipv6)) so this write
/// gate stays in lock-step with the outbound fetch layer instead of
/// hand-rolling its own (narrower) range list. That covers, beyond the RFC
/// 1918 private ranges, the CGNAT (`100.64.0.0/10`), link-local, benchmarking,
/// multicast, reserved and broadcast ranges, and every IPv6-embedded-v4 form
/// (IPv4-mapped, NAT64, 6to4, IPv4-compatible). Known cloud-metadata *DNS
/// hostnames* (`metadata.google.internal`) are rejected too, via
/// [`crate::ssrf::is_cloud_metadata_host`].
///
/// It intentionally does NOT resolve arbitrary hostnames or revalidate
/// redirect destinations: a hostname that resolves to a private/internal
/// address at connect time (DNS rebinding), or a `3xx` redirect `Location`
/// pointing at one, both bypass this *literal* write-time check. Those belong
/// at the outbound fetch layer (the actual HTTP call site), not at
/// config-write time, since the resolved address — and any redirect target —
/// can differ from what was seen when the value was saved. The native LLM
/// provider client does exactly the redirect half: it installs a redirect
/// policy that re-runs [`crate::ssrf::is_ssrf_blocked_url`] on every hop (see
/// [`crate::blocks::llm::providers`]), so on that path redirect-to-internal is
/// closed and DNS rebinding is the sole residual. A resolve-before-connect
/// guard (DNS rebinding) still belongs to the network layer.
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
    // removed — unlike the old string-split extraction. The range list is the
    // shared `wafer-net-security` classifier (same one the outbound fetch
    // layer enforces), so this write gate cannot silently allow a range the
    // fetch layer blocks (CGNAT, multicast, NAT64/6to4 embeddings, …).
    match parsed.host() {
        Some(url::Host::Ipv4(v4)) => {
            if wafer_net_security::is_blocked_ipv4(v4) {
                return Err("URL must not point to private/internal IP addresses".to_string());
            }
        }
        Some(url::Host::Ipv6(v6)) => {
            if wafer_net_security::is_blocked_ipv6(v6) {
                return Err("URL must not point to private/internal IP addresses".to_string());
            }
        }
        // A hostname is not resolved here (see the doc's DNS-rebinding note),
        // but a *literal* well-known cloud-metadata hostname is still a
        // config-write mistake worth rejecting up front.
        Some(url::Host::Domain(h)) if crate::ssrf::is_cloud_metadata_host(h) => {
            return Err("URL must not point to a cloud metadata endpoint".to_string());
        }
        Some(url::Host::Domain(_)) | None => {}
    }
    Ok(())
}

/// The write rule for a config value, keyed by its key: the single check
/// every config-value write surface runs before storing anything — the admin
/// variables create/update paths (`blocks::admin::ops`) and the `config.set`
/// writer (`blocks::config`) — so a value one surface refuses cannot be stored
/// through another. The settings form runs [`validate_declared_config_value`],
/// the same rule with the var's declaration in hand.
///
/// - A `_URL` key runs [`validate_url_value`] (SSRF).
/// - A key with a declared [`crate::config_vars::ConfigValueRule`] runs it,
///   so a write accepts exactly the values its reader can use.
pub(crate) fn validate_config_value(key: &str, value: &str) -> Result<(), String> {
    config_value_rule(key, key.ends_with("_URL"), value)
}

/// [`validate_config_value`] for a declared var: its `InputType::Url` also
/// selects the SSRF check, whatever the key's suffix.
pub(crate) fn validate_declared_config_value(
    var: &wafer_run::ConfigVar,
    value: &str,
) -> Result<(), String> {
    config_value_rule(&var.key, var.is_url() || var.key.ends_with("_URL"), value)
}

fn config_value_rule(key: &str, is_url: bool, value: &str) -> Result<(), String> {
    if is_url {
        validate_url_value(value)?;
    }
    crate::config_vars::check_config_value(key, value)
}

/// Masked placeholder shown in place of a sensitive value.
pub(crate) const MASKED_VALUE: &str = "********";

/// SEC-060: a config value is sensitive when it's explicitly flagged
/// sensitive **or** the key's own spelling or DECLARATION says it holds a
/// secret ([`crate::config_vars::is_sensitive_for_storage`]: the
/// `_SECRET`/`_KEY` suffix convention, or a declared
/// [`ConfigVar`](wafer_run::ConfigVar) that is `InputType::Password` or
/// `auto_generate`).
///
/// "Explicitly flagged" means different things on each caller's substrate —
/// the admin Variables table's DB `sensitive` column for ad hoc rows, or the
/// `ConfigVar` in hand for the generic settings form — so callers pass their
/// own flag in as `1`/`0`. The key half of the rule is what every caller
/// shares, and it has to be the WHOLE key rule, not just the suffix: masking
/// on the flag alone leaked a `*_SECRET` value whenever a var/row wasn't
/// explicitly marked, and masking on the flag plus the suffix alone leaked
/// `WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_PASSWORD` — declared
/// `InputType::Password`, spelled with neither suffix — for as long as a
/// legacy row carried `sensitive = 0`, which on Cloudflare means until the
/// next `/_deploy/init` ran [`crate::platform_state::variables::repair_sensitive_flags`].
///
/// Single source of truth for "is this key sensitive": the admin Variables
/// page and its edit modal (`blocks::admin::ops`, re-exported from here), the
/// settings JSON API (`blocks::admin::settings`), the generic
/// ConfigVar-driven settings form (`ui::settings_form`), the edge-cache
/// exclusion (`cache_key::row_is_sensitive`) and the export filter
/// (`blocks::dev::data_snapshot::variable_is_exportable`) all ask this, so no
/// two of them can disagree about what gets redacted — the modal masking a
/// value the table beside it rendered in clear is exactly the drift this
/// closes.
///
/// That list is the surfaces that ASK, not every surface that can publish a
/// stored value. The admin SQL explorer (`POST /b/admin/api/database/query`
/// and its SSR twin) cannot join it: `db::query_raw` returns records keyed by
/// the column name the QUERY chose, so a mask keyed on `(table, column)` is
/// defeated by `SELECT value AS v`. It is held to the same promise by a
/// different mechanism — [`crate::secret_tables`] refuses, before execution,
/// any query naming a table that stores credential material — which is why
/// this enumeration can be read as covering every surface that publishes a
/// stored value, even though the explorer is not on it.
///
/// It is deliberately the same key predicate the WRITE path applies when it
/// decides what the stored flag gets ([`crate::config_vars::is_sensitive_for_storage`],
/// at `platform_state::variables::NewVariable::into_row`). Reader and writer
/// asking one question means the stored flag is a cache of the answer, never
/// the only copy of it — so a row written before the funnel existed, or by a
/// build that did not know the key, is still masked.
pub(crate) fn is_sensitive_key(key: &str, sensitive_flag: i64) -> bool {
    sensitive_flag == 1 || crate::config_vars::is_sensitive_for_storage(key)
}

/// Whether a submitted `value` for `key` is the [`MASKED_VALUE`] a read path
/// produced rather than a value its sender means.
///
/// The counterpart of [`is_sensitive_key`], for the write direction. Every read
/// surface answers a sensitive key with `"********"`, so the read/modify/write
/// loop a JSON client is built around — GET the settings, change one, PATCH
/// them back — hands the mask straight back to the writer for every key it did
/// not touch. Stored, it replaces the secret with eight asterisks; the worst
/// case is a live `..._BOOTSTRAP_ADMIN_TOKEN`, which is what provisions the
/// first admin.
///
/// Gated on sensitivity, not on the string alone: `"********"` is a perfectly
/// ordinary value for a variable nothing masks (placeholder copy, a redaction
/// marker), and refusing it there would be the write path inventing a reserved
/// word. It is only a mask where something masked it — which is exactly the
/// question [`is_sensitive_key`] answers, asked with the same stored flag the
/// reader used.
///
/// All four write surfaces refuse it: the JSON API and the admin Variables
/// modal through `blocks::admin::ops::update_variable`, the generic
/// ConfigVar-driven form through `ui::settings_form::save_settings`, and
/// `CONFIG_SET` itself (`blocks::config`'s `ConfigWrite::write`) — the last of
/// which is what makes this claim true by construction rather than by accident
/// of who calls what, since any block can reach that operation through
/// `wafer_core::clients::config::set`. None of them silently drops it instead —
/// a caller that is told "saved" while its write was discarded can never find
/// out, because the next read hands it the same mask back. "Leave the stored
/// value alone" has its own spelling on each surface (omit `value`; leave the
/// masked field blank), and that spelling is what the refusal names.
///
/// `save_settings` is the one that does not use this predicate, and
/// deliberately: WRAP denies four of its five callers (all but
/// `admin::pages::email`, which runs as the admin block itself) the admin
/// `variables` table, and a shared helper has to work for the four — so it
/// cannot supply the stored flag the third argument stands for. It asks a
/// question that needs no flag instead: would this mask REPLACE the value the
/// field currently holds? That covers more than this predicate does, which is
/// the only shape that lets it promise no half-applied save, and it stops
/// short of refusing a submission that changes nothing. The exactness this
/// predicate provides needs the row, and only the surfaces that can read the
/// row get it.
pub(crate) fn is_masked_submission(key: &str, sensitive_flag: i64, value: &str) -> bool {
    value == MASKED_VALUE && is_sensitive_key(key, sensitive_flag)
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
/// decoding. Repeated keys collapse to the last value (the existing behaviour);
/// a field that can legitimately be posted more than once needs
/// [`form_values`] instead.
pub fn parse_form_body(data: &[u8]) -> HashMap<String, String> {
    url::form_urlencoded::parse(data).into_owned().collect()
}

/// Every value posted under `key`, in wire order.
///
/// The half of a form body [`parse_form_body`]'s last-wins map cannot express.
/// A control that can post its own name more than once — a multi-select, a
/// checkbox group, or a client serialising an array — sends `k=a&k=b`, and
/// reading that through the map silently keeps only `b`.
pub fn form_values(data: &[u8], key: &str) -> Vec<String> {
    url::form_urlencoded::parse(data)
        .filter(|(k, _)| k == key)
        .map(|(_, value)| value.into_owned())
        .collect()
}

/// Parse a request body as either JSON or URL-encoded form into a JSON Value.
///
/// Inspects the first non-whitespace byte: `{` or `[` → JSON, anything else →
/// URL-encoded form (then promoted to a flat object of strings). Lets one
/// handler accept both htmx form posts and programmatic JSON clients without
/// duplicating parse logic.
///
/// A body that announces itself as JSON and then fails to parse is returned as
/// the `serde_json` error, position and reason intact. Swallowing it into
/// `Value::Null` cost the caller that sentence: every malformed JSON body came
/// back to the client as "invalid type: null, expected struct …", which names
/// neither the offending byte nor the fact that the body was not JSON at all.
/// The form branch cannot fail — `form_urlencoded::parse` accepts any bytes.
pub fn parse_body_value(data: &[u8]) -> Result<serde_json::Value, serde_json::Error> {
    let trimmed_start = data
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(0);
    if data.get(trimmed_start) == Some(&b'{') || data.get(trimmed_start) == Some(&b'[') {
        return serde_json::from_slice(data);
    }
    let mut obj = serde_json::Map::new();
    for (k, v) in parse_form_body(data) {
        obj.insert(k, serde_json::Value::String(v));
    }
    Ok(serde_json::Value::Object(obj))
}

/// Encode client-side [`Filter`](wafer_block::db::Filter)s as all-leaf wire
/// [`FilterNode`](wafer_block::wire::database::FilterNode)s for a typed
/// `db::aggregate` request. Mirrors `wafer-core`'s internal
/// `to_wire_filters` conversion (not exported for block code to reuse).
pub(crate) fn to_wire_filters(
    filters: &[wafer_block::db::Filter],
) -> Vec<wafer_block::wire::database::FilterNode> {
    use wafer_block::{db::FilterOp, wire::database as wire};
    filters
        .iter()
        .map(|f| {
            let operator = match f.operator {
                FilterOp::Equal => "eq",
                FilterOp::NotEqual => "neq",
                FilterOp::GreaterThan => "gt",
                FilterOp::GreaterEqual => "gte",
                FilterOp::LessThan => "lt",
                FilterOp::LessEqual => "lte",
                FilterOp::Like => "like",
                FilterOp::In => "in",
                FilterOp::IsNull => "is_null",
                FilterOp::IsNotNull => "is_not_null",
            };
            wire::FilterNode::Leaf(wire::FilterDef {
                field: f.field.clone(),
                operator: operator.to_string(),
                value: f.value.clone(),
                column: None,
            })
        })
        .collect()
}

/// Read one aggregate output column as an exact integer.
///
/// Only a JSON integer is accepted. A float, a numeric string or an absent
/// column is a decode fault, reported with the alias and the value rather
/// than coerced into a plausible figure.
///
/// The caller is responsible for asking the database for an integer. A
/// `COUNT(*)` or a `CaseWhenSum` is one on every backend. A `Sum` is not:
/// PostgreSQL's `sum(bigint)` is `NUMERIC`, which `wafer-block-postgres`
/// decodes through `BigDecimal` into `f64`, and SQLite sums to `REAL` as soon
/// as one summed value is not an integer. So every `Sum` this reads carries
/// [`bigint_cast`], which settles the type in the statement itself.
/// A cast rounds a non-integral value on PostgreSQL and truncates it on
/// SQLite, so it belongs only on columns that hold whole numbers — minor-unit
/// money and quantities, which is all this reads.
///
/// Not folded into [`json_as_i64`]: that one is the coercion for *stored
/// columns*, which accepts the numeric string a TEXT column hands back.
///
/// Gated on `block-products` because that block is its only caller: the
/// commerce analytics is the one place in the crate that reads a `SUM` over a
/// money column. The lean Cloudflare Worker builds without that block and
/// lints dead code as an error, so the gate is the honest statement of who
/// needs this rather than an `allow`.
#[cfg(feature = "block-products")]
pub(crate) fn aggregate_i64(record: &Record, alias: &str) -> Result<i64, wafer_run::WaferError> {
    let fault = |detail: &str| {
        wafer_run::WaferError::new(
            wafer_run::ErrorCode::Internal,
            format!("aggregate column {alias} is not an integer: {detail}"),
        )
    };
    let Some(value) = record.data.get(alias) else {
        return Err(fault("the column is absent from the result row"));
    };
    value.as_i64().ok_or_else(|| fault(&value.to_string()))
}

/// The `cast_as` for a `Sum` read through [`aggregate_i64`]: `BIGINT`, so the
/// sum is an integer on every backend. Spelled from the allowlist
/// `wafer-sql-utils` parses the wire value against, not typed out here.
#[cfg(feature = "block-products")]
pub(crate) fn bigint_cast() -> Option<String> {
    Some(
        wafer_sql_utils::aggregate::CastType::BigInt
            .as_sql()
            .to_string(),
    )
}

/// Run ONE grouped-by-day aggregate over `table` for rows whose `created_at`
/// is at or after `since_iso`, and return the per-day rows (one
/// [`Record`](wafer_block::wire::database::Record) per day that has data,
/// its day under the `created_at` alias). `aggregates` may carry several
/// columns — a plain `Count` alongside a conditional `CaseWhenSum` — so a
/// single statement can back multiple daily series over the same table.
///
/// Shared by the table modules that render a daily chart
/// (`platform_state::request_logs::daily_counts`, the admin dashboard's
/// users series) so the date-bucket shape is built once.
pub(crate) async fn daily_grouped(
    ctx: &dyn wafer_run::context::Context,
    table: &str,
    since_iso: &str,
    extra_filters: Vec<wafer_block::db::Filter>,
    aggregates: Vec<wafer_block::wire::database::AggregateColumnDef>,
) -> Result<Vec<wafer_block::wire::database::Record>, wafer_run::WaferError> {
    use wafer_block::{
        db::{Filter, FilterOp},
        wire::database as wire,
    };
    let mut filters = vec![Filter {
        field: "created_at".into(),
        operator: FilterOp::GreaterEqual,
        value: serde_json::json!(since_iso),
    }];
    filters.extend(extra_filters);

    let req = wire::AggregateRequest {
        collection: table.to_string(),
        select_columns: vec![],
        aggregates,
        filters: to_wire_filters(&filters),
        group_by: vec![wire::GroupByDef::DateBucket {
            field: "created_at".into(),
        }],
        sort: vec![],
        limit: 0,
    };
    wafer_core::clients::database::aggregate(ctx, req).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blocks::auth::config::BOOTSTRAP_ADMIN_PASSWORD_KEY;

    #[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum Colour {
        Red,
        SeaGreen,
    }

    fn coloured(value: serde_json::Value) -> Record {
        Record {
            id: "row-7".to_string(),
            data: [("colour".to_string(), value)].into_iter().collect(),
        }
    }

    #[test]
    fn enum_column_reads_the_stored_spelling() {
        assert_eq!(
            enum_column::<Colour>(&coloured(serde_json::json!("sea_green")), "colour").unwrap(),
            Colour::SeaGreen
        );
    }

    /// A value outside the set is a data-integrity fault, never a default:
    /// the message has to name the row, the column and the value, because
    /// the operator's next move is to go and look at that row.
    #[test]
    fn enum_column_names_the_row_the_column_and_the_value() {
        let err = enum_column::<Colour>(&coloured(serde_json::json!("chartreuse")), "colour")
            .expect_err("refused");
        assert_eq!(err.code, wafer_run::ErrorCode::Internal);
        assert!(err.message.contains("row-7"), "{}", err.message);
        assert!(err.message.contains("colour"), "{}", err.message);
        assert!(err.message.contains("chartreuse"), "{}", err.message);
    }

    /// An absent column and a `NULL` one both read as `""`, which is not a
    /// variant of anything — so both are the empty case, not a bad value.
    #[test]
    fn enum_column_refuses_an_empty_column() {
        for empty in [serde_json::json!(""), serde_json::json!(null)] {
            assert!(enum_column::<Colour>(&coloured(empty), "colour").is_err());
        }
        assert!(enum_column::<Colour>(
            &Record {
                id: "row-7".to_string(),
                data: HashMap::new(),
            },
            "colour"
        )
        .is_err());
    }

    /// An enum that names `""` as a variant reads the empty column as that
    /// variant: the column default is one of its states, not missing data.
    #[test]
    fn enum_column_reads_an_empty_column_as_a_variant_named_empty() {
        #[derive(Debug, PartialEq, serde::Deserialize)]
        enum Tint {
            #[serde(rename = "")]
            Unset,
            #[serde(rename = "red")]
            Red,
        }
        for empty in [serde_json::json!(""), serde_json::json!(null)] {
            assert_eq!(
                enum_column::<Tint>(&coloured(empty), "colour").unwrap(),
                Tint::Unset
            );
        }
        assert_eq!(
            enum_column::<Tint>(&coloured(serde_json::json!("red")), "colour").unwrap(),
            Tint::Red
        );
    }

    /// The fallback is a typed variant, so a caller cannot supply a default
    /// the enum does not define — the bug the `&str` fallback it replaces
    /// made possible.
    #[test]
    fn enum_column_or_takes_the_fallback_only_for_an_empty_column() {
        assert_eq!(
            enum_column_or(&coloured(serde_json::json!("")), "colour", Colour::Red).unwrap(),
            Colour::Red
        );
        assert_eq!(
            enum_column_or(
                &coloured(serde_json::json!("sea_green")),
                "colour",
                Colour::Red
            )
            .unwrap(),
            Colour::SeaGreen
        );
        assert!(enum_column_or(
            &coloured(serde_json::json!("chartreuse")),
            "colour",
            Colour::Red
        )
        .is_err());
    }

    /// A caller writes a URL, so a query string in one has to reach the
    /// callee the way it does over HTTP: off the path and into
    /// `req.query.*`. Leaving it on `req.resource` made the path match no
    /// route at all — a silent 404 the two callers that used it swallowed.
    #[test]
    fn block_request_splits_a_query_string_off_the_path() {
        let msg = block_request(
            "retrieve",
            "GET",
            "/b/messages/api/contexts/c1/entries?kind=message&q=two+words",
            &wafer_run::Message::new("http.request"),
        );

        assert_eq!(msg.path(), "/b/messages/api/contexts/c1/entries");
        assert_eq!(msg.get_meta("http.path"), msg.path());
        assert_eq!(msg.query("kind"), "message");
        assert_eq!(msg.query("q"), "two words");
        assert_eq!(msg.get_meta("http.query.kind"), "message");
        assert_eq!(msg.get_meta("http.raw_query"), "kind=message&q=two+words");
    }

    #[test]
    fn block_request_without_a_query_string_is_unchanged() {
        let msg = block_request(
            "create",
            "POST",
            "/b/messages/api/contexts/c1/entries",
            &wafer_run::Message::new("http.request"),
        );
        assert_eq!(msg.path(), "/b/messages/api/contexts/c1/entries");
        assert_eq!(msg.get_meta("http.raw_query"), "");
        assert!(msg.query("kind").is_empty());
    }

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
    fn form_values_keeps_every_value_a_repeated_key_carries() {
        assert_eq!(form_values(b"k=a&k=b&other=c", "k"), vec!["a", "b"]);
        assert_eq!(form_values(b"k=a+b&k=c%2Fd", "k"), vec!["a b", "c/d"]);
        assert_eq!(form_values(b"k=", "k"), vec![""]);
        assert!(form_values(b"k=a", "absent").is_empty());
        // The last-wins map is what it cannot express.
        assert_eq!(
            parse_form_body(b"k=a&k=b").get("k").map(String::as_str),
            Some("b")
        );
    }

    #[test]
    fn parse_body_value_reads_json_and_form_bodies() {
        assert_eq!(
            parse_body_value(br#"{"a":1}"#).expect("JSON object"),
            serde_json::json!({"a": 1})
        );
        assert_eq!(
            parse_body_value(b"  [1,2]").expect("JSON array after whitespace"),
            serde_json::json!([1, 2])
        );
        assert_eq!(
            parse_body_value(b"a=1&b=two+words").expect("a form body cannot fail"),
            serde_json::json!({"a": "1", "b": "two words"}),
            "form fields are strings — a form cannot say `1` the number"
        );
        assert_eq!(
            parse_body_value(b"").expect("an empty body is an empty form"),
            serde_json::json!({})
        );
    }

    /// A body that opens with `{` claims to be JSON, so a failure to parse it
    /// is a JSON error and must be reported as one. Returning `Value::Null`
    /// instead handed the caller's `from_value` a type mismatch to describe,
    /// and the byte position and reason were gone by then.
    #[test]
    fn a_malformed_json_body_reports_where_it_broke() {
        let err = parse_body_value(b"{oops").expect_err("`{oops` is not JSON");
        let message = err.to_string();
        assert!(
            message.contains("line 1 column 2"),
            "the error must keep its position, got: {message}"
        );
        assert!(
            message.contains("key must be a string"),
            "the error must keep its reason, got: {message}"
        );
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
    fn format_count_groups_thousands() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(1_000), "1,000");
        assert_eq!(format_count(10_000), "10,000");
        assert_eq!(format_count(1_234_567), "1,234,567");
        assert_eq!(format_count(-1_234), "-1,234");
        assert_eq!(format_count(i64::MIN), "-9,223,372,036,854,775,808");
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

    fn record(data: serde_json::Value) -> Record {
        Record {
            id: "r1".to_string(),
            data: json_map(data),
        }
    }

    #[test]
    fn opt_str_field_distinguishes_null_and_absent_from_a_string() {
        let r = record(serde_json::json!({"present": "x", "empty": "", "null": null, "num": 3}));
        assert_eq!(r.opt_str_field("present").as_deref(), Some("x"));
        assert_eq!(r.opt_str_field("empty").as_deref(), Some(""));
        assert_eq!(r.opt_str_field("null"), None);
        assert_eq!(r.opt_str_field("absent"), None);
        assert_eq!(r.opt_str_field("num"), None);
    }

    #[test]
    fn json_fields_decode_text_and_pass_through_decoded_values() {
        // SQLite hands JSON-shaped TEXT back decoded; Postgres/D1 hand back
        // the literal string. Both must read the same.
        let decoded = record(serde_json::json!({
            "tags": ["a", "b", 3],
            "meta": {"k": 1},
            "snap": {"x": [1]}
        }));
        let literal = record(serde_json::json!({
            "tags": "[\"a\",\"b\",3]",
            "meta": "{\"k\":1}",
            "snap": "{\"x\":[1]}"
        }));
        for r in [&decoded, &literal] {
            assert_eq!(r.string_list_field("tags"), vec!["a", "b"]);
            assert_eq!(
                serde_json::Value::Object(r.json_object_field("meta")),
                serde_json::json!({"k": 1})
            );
            assert_eq!(r.json_value_field("snap"), serde_json::json!({"x": [1]}));
        }
    }

    /// The accessor a caller reaches for when it carries a JSON column onward
    /// instead of inspecting it. `str_field` cannot do this job: it answers
    /// `""` for the decoded arm, which is how a payments audit trail was being
    /// blanked on two of the three adapters.
    #[test]
    fn json_text_field_answers_the_encoded_text_on_both_arms() {
        let decoded = record(serde_json::json!({"resp": {"id": "re_1"}, "blank": {}}));
        let literal = record(serde_json::json!({"resp": "{\"id\":\"re_1\"}", "blank": "{}"}));
        for r in [&decoded, &literal] {
            assert_eq!(r.json_text_field("resp"), "{\"id\":\"re_1\"}");
            assert_eq!(r.json_text_field("blank"), "{}");
        }
        // The trap this replaces: on the decoded arm — SQLite and the browser
        // — `str_field` answers `""` for a payload that is plainly there,
        // while the same read is correct against D1 and Postgres.
        assert_eq!(decoded.str_field("resp"), "");
        assert_eq!(literal.str_field("resp"), "{\"id\":\"re_1\"}");
        // Absent and SQL NULL stay empty, so an `is_empty()` guard on the
        // result still means "nothing stored".
        let sparse = record(serde_json::json!({"null": null}));
        assert_eq!(sparse.json_text_field("null"), "");
        assert_eq!(sparse.json_text_field("absent"), "");
    }

    #[test]
    fn json_fields_read_as_empty_when_the_column_does_not_hold_the_declared_kind() {
        let r =
            record(serde_json::json!({"tags": "{\"not\":\"a list\"}", "meta": "[1]", "bad": "{"}));
        assert!(r.string_list_field("tags").is_empty());
        assert!(r.json_object_field("meta").is_empty());
        assert!(r.json_object_field("absent").is_empty());
        assert_eq!(r.json_value_field("bad"), serde_json::Value::Null);
        assert_eq!(r.json_value_field("absent"), serde_json::Value::Null);
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

    /// IPv6 unique-local addresses (`fc00::/7`, in practice almost always
    /// seen as `fd00::/8`) are the IPv6 analogue of the RFC 1918 private
    /// IPv4 ranges the v4 branch already blocks (`10.x`, `172.16-31.x`,
    /// `192.168.x`) — internal-network-only, never a legitimate HTTPS
    /// destination. Previously unchecked for v6 literals.
    #[test]
    fn rejects_ipv6_unique_local() {
        assert!(validate_url_value("https://[fd00::1]/x").is_err());
        assert!(validate_url_value("https://[fc00::1]/x").is_err());
    }

    /// IPv6 link-local (`fe80::/10`) is the v6 analogue of the v4 branch's
    /// `169.254.x` link-local block — which on many cloud providers is also
    /// how the instance-metadata endpoint is reached. Previously unchecked
    /// for v6 literals.
    #[test]
    fn rejects_ipv6_link_local() {
        assert!(validate_url_value("https://[fe80::1]/x").is_err());
        assert!(validate_url_value("https://[fe80::abcd:1234]/x").is_err());
    }

    /// Sanity check: a real, routable IPv6 address must still be allowed —
    /// the new unique-local/link-local checks must not overreject.
    #[test]
    fn allows_real_ipv6_address() {
        assert!(validate_url_value("https://[2606:4700:4700::1111]/x").is_ok());
    }

    /// Ranges the previous hand-rolled predicate missed but the shared
    /// `wafer-net-security` classifier (now delegated to) blocks: CGNAT, the
    /// AWS/GCP metadata IP literals, and the IPv6-embedded-v4 forms
    /// (IPv4-mapped, NAT64, 6to4).
    #[test]
    fn rejects_ranges_closed_by_shared_classifier() {
        // Carrier-grade NAT (100.64.0.0/10) — routable internal cloud infra.
        assert!(validate_url_value("https://100.64.0.1/x").is_err());
        // Cloud metadata service (link-local v4).
        assert!(validate_url_value("https://169.254.169.254/latest/meta-data/").is_err());
        // IPv4-mapped IPv6 private/loopback.
        assert!(validate_url_value("https://[::ffff:10.0.0.1]/x").is_err());
        // NAT64 embedding of the metadata service (64:ff9b::169.254.169.254).
        assert!(validate_url_value("https://[64:ff9b::a9fe:a9fe]/x").is_err());
        // 6to4 embedding of 127.0.0.1.
        assert!(validate_url_value("https://[2002:7f00:1::]/x").is_err());
        // A NAT64 route to a genuinely public v4 stays allowed.
        assert!(validate_url_value("https://[64:ff9b::5db8:d822]/x").is_ok());
    }

    /// A literal cloud-metadata DNS hostname is rejected at config-write time
    /// even though hostnames are otherwise not resolved here.
    #[test]
    fn rejects_cloud_metadata_hostname() {
        assert!(validate_url_value("https://metadata.google.internal/x").is_err());
        assert!(validate_url_value("https://metadata.google.internal./x").is_err());
        // A normal hostname that merely contains the string is still allowed.
        assert!(validate_url_value("https://metadata.google.internal.example.com/x").is_ok());
    }

    #[test]
    fn is_sensitive_key_honors_flag_suffix_and_declaration() {
        // Flag set → sensitive regardless of name.
        assert!(is_sensitive_key("PLAIN", 1));
        // SEC-060: suffix makes it sensitive even when the flag is clear.
        assert!(is_sensitive_key("STRIPE_SECRET", 0));
        assert!(is_sensitive_key("JWT_KEY", 0));
        // The DECLARATION makes it sensitive even with neither flag nor
        // suffix. This key is spelled neither `_SECRET` nor `_KEY`, so before
        // the read path consulted the declaration a row written by an older
        // build sat here unflagged and was served in the clear.
        assert!(is_sensitive_key(BOOTSTRAP_ADMIN_PASSWORD_KEY, 0));
        // None of the three → not sensitive.
        assert!(!is_sensitive_key("SITE_NAME", 0));
    }

    /// An aggregate that comes back as a float is a fault even when the
    /// float is whole. `2500.0` is how an uncast `SUM(bigint)` reads on
    /// PostgreSQL; accepting it would let a sum that is missing its
    /// `BIGINT` cast pass on one backend and keep passing unnoticed.
    #[cfg(feature = "block-products")]
    #[test]
    fn aggregate_i64_refuses_a_whole_float() {
        let err = aggregate_i64(&record(serde_json::json!({"gross": 2500.0})), "gross")
            .expect_err("a float is not an integer");
        assert_eq!(err.code, wafer_run::ErrorCode::Internal);
        assert!(err.message.contains("gross"), "{}", err.message);
        assert!(err.message.contains("2500.0"), "{}", err.message);
    }

    #[cfg(feature = "block-products")]
    #[test]
    fn aggregate_i64_reads_an_integer_and_refuses_everything_else() {
        let row = record(serde_json::json!({
            "gross": 2500,
            "negative": -7,
            "fraction": 2500.5,
            "text": "2500",
            "null": null,
        }));
        assert_eq!(aggregate_i64(&row, "gross").unwrap(), 2500);
        assert_eq!(aggregate_i64(&row, "negative").unwrap(), -7);
        for alias in ["fraction", "text", "null", "absent"] {
            let err = aggregate_i64(&row, alias).expect_err(alias);
            assert!(err.message.contains(alias), "{}", err.message);
        }
    }
}
