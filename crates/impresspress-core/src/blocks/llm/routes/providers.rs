//! Provider CRUD (admin-only).
//!
//! These endpoints back the LLM admin UI's provider management. All writes
//! reload the in-memory `ProviderLlmService` from the DB so chat requests
//! pick up the new configuration without restarting the process.
//!
//! ## A deployment that cannot manage providers says so
//!
//! Whether provider management works is a property of the *handle the block
//! was constructed with*, not of the build: a runtime holding a
//! [`NoopProviderAdmin`] has no router to configure. The four mutating
//! handlers therefore ask
//! [`ProviderAdmin::manages_providers`] before they touch the database and
//! answer 501, so a refusal never leaves a provider row behind that nothing
//! will load. The routes stay declared either way — see
//! [`NoopProviderAdmin`]'s own documentation for why the published surface
//! must not depend on which handle a deployment holds.
//!
//! [`NoopProviderAdmin`]: crate::blocks::llm::provider_admin::NoopProviderAdmin

use wafer_core::{
    clients::{config, database as db},
    interfaces::llm::service::LlmError,
};
use wafer_run::{context::Context, ErrorCode, InputStream, Message, OutputStream, WaferError};

use crate::{
    blocks::{
        crud,
        llm::{
            contracts::{
                CreateProviderRequest, DiscoveredModelsResponse, ProviderDeleteResponse,
                ProviderListResponse, ProviderView, UpdateProviderRequest,
            },
            provider_admin::ProviderAdmin,
            providers::config::ProviderConfig,
            schema::{config_to_row, models_row, row_to_config, TABLE as PROVIDERS_TABLE},
            LlmBlock,
        },
    },
    db_read::{self, Bound},
    http::{err_bad_request, err_internal, ok_json},
};

/// Refuse, before any database work, when this runtime's provider router
/// cannot be configured.
///
/// Returns the 501 as an `Err` so a handler can `?`-shaped early-return it.
/// The message names the capability rather than the handle type: an operator
/// reading it needs to know that this deployment does not do provider
/// management, not which Rust struct is standing in.
fn require_provider_management(block: &LlmBlock) -> Result<(), OutputStream> {
    if block.provider_admin.manages_providers() {
        return Ok(());
    }
    Err(OutputStream::error(WaferError::new(
        ErrorCode::Unimplemented,
        "provider management is not supported on this deployment: it has no \
         configurable provider router",
    )))
}

/// Render an [`LlmError`] from the provider-admin surface as the response its
/// cause deserves.
///
/// Mirrors `wafer_core::interfaces::llm::handler::llm_error_to_block_error`,
/// which maps the same enum for the *service block's* wire and is private to
/// that module (it is a candidate to make public upstream). The classified
/// arms agree deliberately; the difference is what happens to the two
/// internal ones, which go through [`err_internal`] so the cause is logged
/// with a correlation id and the client gets the sanitized message instead of
/// a provider's raw transport text.
///
/// `context` is the fixed log label for those arms — no interpolated error
/// text, per [`err_internal`]'s contract.
///
/// The classified arms render `e` through `Display`. `discover_models` used
/// to hand `format!("{e:?}")` to `err_internal`, which put a Rust enum
/// spelling into a log line and answered 500 for every one of them —
/// including `NotSupported`, which is a 501, and `Unauthorized`, which is the
/// admin's own provider credential being wrong.
fn llm_error_response(context: &str, e: LlmError) -> OutputStream {
    let code = match &e {
        LlmError::NotSupported => ErrorCode::Unimplemented,
        LlmError::InvalidRequest(_) => ErrorCode::InvalidArgument,
        LlmError::ModelNotFound(_) => ErrorCode::NotFound,
        LlmError::RateLimited => ErrorCode::Unavailable,
        LlmError::Unauthorized => ErrorCode::Unauthenticated,
        LlmError::Cancelled => ErrorCode::Cancelled,
        // `BackendError` and `Network` carry a provider's own transport text,
        // which is deployment topology and must not reach the client. The
        // wildcard is required — `LlmError` is `#[non_exhaustive]` upstream —
        // and lands on the sanitizing arm deliberately: an error shape this
        // repo has not classified yet is not one to echo.
        _ => return err_internal(context, e),
    };
    OutputStream::error(WaferError::new(code, e.to_string()))
}

/// Reload all enabled providers from the DB and push the snapshot into the
/// in-memory provider router via [`ProviderAdmin::configure`].
///
/// This is the single choke point where stored rows become live
/// `ProviderConfig`s: rows are decoded via [`row_to_config`] (which never
/// yields an `api_key`) and each config's `key_var` is resolved into
/// `api_key` here, via the config client, before `configure()`. Secret
/// rotation therefore takes effect on the next reload (boot or any provider
/// CRUD write), not per chat request.
///
/// Shared by the provider CRUD handlers and `LlmBlock::lifecycle(Init)`
/// (which is why it takes the provider-admin handle rather than the whole
/// block).
///
/// Errors are returned to the caller, which answers them through
/// `crud::db_error_internal`: the row read keeps its database code, so a WRAP
/// denial is a 403 and a quota a 429, and a router that refused the snapshot
/// is an `Internal` failure — a 500. We do not silently swallow — a failure
/// here means the in-memory service is stale and the admin needs to know.
///
/// A handle with no router to configure ([`ProviderAdmin::manages_providers`]
/// false) fails here too. The mutating handlers never see that, because they
/// refuse at [`require_provider_management`] before writing; `lifecycle(Init)`
/// does not call this at all on such a runtime. It stays an error rather than
/// a silent success so a caller that grew past those two cannot inherit the
/// green-200-over-an-inert-router bug a second time.
pub(in crate::blocks::llm) async fn reload_provider_service(
    ctx: &dyn Context,
    provider_admin: &dyn ProviderAdmin,
) -> Result<(), WaferError> {
    let records = db_read::list_bounded(
        ctx,
        PROVIDERS_TABLE,
        vec![],
        Bound::Curated("LLM providers are configured by an admin"),
    )
    .await?;
    let mut configs: Vec<ProviderConfig> = Vec::with_capacity(records.len());
    for rec in &records {
        match row_to_config(rec) {
            Ok(mut cfg) if cfg.enabled => match resolve_provider_key(ctx, &mut cfg).await {
                Ok(()) => configs.push(cfg),
                // Like a malformed row, one provider whose key cannot be
                // read must not take the others down with it — and it must
                // not run without the key it names either, so it is left
                // out. Create, and an update whose body names `key_var`,
                // refuse to store a row whose key cannot be read; an update
                // that leaves `key_var` alone stores the row whatever its key
                // reads now, so the admin can still disable or repoint it.
                // One arrives here, then, when a read that succeeded at the
                // last check fails now — the grant was withdrawn since, or
                // the read failed transiently (the next reload retries it) —
                // or when the row reached the table without going through
                // create or update.
                Err(e) => tracing::error!(
                    "skipping provider row {}: its key_var could not be read: {e}",
                    rec.id
                ),
            },
            Ok(_) => {} // disabled — skip
            Err(e) => {
                // A malformed row should not poison the whole reload —
                // drop just that one.
                tracing::warn!("skipping malformed provider row {}: {e}", rec.id);
            }
        }
    }
    provider_admin.configure(configs).map_err(|e| {
        WaferError::new(
            ErrorCode::Internal,
            format!("provider configure failed: {e}"),
        )
    })
}

/// Resolve a provider's `key_var` into its plaintext `api_key` via the
/// config client. `key_var` takes precedence over any inline `api_key`;
/// with no `key_var` the config is left untouched.
///
/// A variable that is unset or empty is logged and leaves `api_key` as-is —
/// the provider then runs unauthenticated, and the per-protocol encoder
/// decides whether that's an error (`MissingApiKey` → 401) on the next chat
/// call. Local OpenAI-compatible servers legitimately run without a key.
///
/// A read that fails is returned: a refused read says nothing about whether
/// a key exists, so the caller must not run the provider without one.
/// [`reload_provider_service`] leaves that provider out; create, and an update
/// naming `key_var`, refuse the write up front through
/// [`check_key_var_readable`].
async fn resolve_provider_key(
    ctx: &dyn Context,
    cfg: &mut ProviderConfig,
) -> Result<(), WaferError> {
    let Some(var) = cfg.key_var.as_deref() else {
        return Ok(());
    };
    match config::get_optional(ctx, var).await? {
        Some(value) if !value.is_empty() => cfg.api_key = Some(value),
        Some(_) => tracing::warn!(
            "provider '{}': key_var `{var}` is set but empty — provider will run unauthenticated",
            cfg.name
        ),
        None => tracing::warn!(
            "provider '{}': key_var `{var}` is not set — provider will run unauthenticated",
            cfg.name
        ),
    }
    Ok(())
}

/// Refuse to store a provider whose `key_var` this block cannot read.
///
/// Checked before the row is written — on every create, and on an update
/// whose body names `key_var` — so an admin who names a variable the block
/// holds no grant for (another block's `*_SECRET_KEY`, say) is told so on the
/// save and nothing is stored. The value itself is discarded; the
/// reload resolves it.
async fn check_key_var_readable(ctx: &dyn Context, cfg: &ProviderConfig) -> Result<(), WaferError> {
    match cfg.key_var.as_deref() {
        Some(var) => config::get_optional(ctx, var).await.map(|_| ()),
        None => Ok(()),
    }
}

/// `GET /b/llm/api/providers` — list all rows. Admin-only.
pub(in crate::blocks::llm) async fn list_providers(
    _block: &LlmBlock,
    ctx: &dyn Context,
    _msg: &Message,
) -> OutputStream {
    let records = match db_read::list_bounded(
        ctx,
        PROVIDERS_TABLE,
        vec![],
        Bound::Curated("LLM providers are configured by an admin"),
    )
    .await
    {
        Ok(r) => r,
        Err(e) => return crud::db_error_internal(e, "Database error"),
    };
    let providers: Vec<ProviderView> = records
        .iter()
        .filter_map(|rec| {
            row_to_config(rec)
                .ok()
                .map(|cfg| ProviderView::from_config(&rec.id, &cfg))
        })
        .collect();
    ok_json(&ProviderListResponse { providers })
}

/// Parse the create-provider body as JSON or as the admin page's
/// URL-encoded form.
///
/// A leading `{` is JSON and deserializes into the contract directly, with no
/// coercions — a string where the schema says array or bool is a 400, as the
/// published schema promises. Anything else is a form body: the add-provider
/// form is a plain htmx `hx-post`, so it sends
/// `application/x-www-form-urlencoded` and every field arrives as a string.
///
/// The form values that are not strings in the contract are coerced here, and
/// only here: `models` is one comma-separated text input, `enabled` is a
/// checkbox, which posts nothing at all when the admin unticks it, and
/// `max_tokens_field` is a select whose "follow the protocol" option has no
/// token to post — a `<select>` always sends *something*, and `""` is not one
/// of the two wire spellings, so an empty one is dropped and the contract
/// sees the field omitted. `models` is
/// read through [`crate::util::form_values`] rather than the last-wins map, so
/// a client that spells the list as a repeated key (`models=a&models=b`, which
/// is how urlencoded serialisation writes an array) is understood to mean both
/// rather than silently reduced to the last one. The rest
/// of the map is handed to serde untouched, so `deny_unknown_fields` still
/// refuses an `api_key` by name on the form path too — which is the whole
/// point of that attribute (see [`CreateProviderRequest`]).
fn parse_create_provider_body(raw: &[u8]) -> Result<CreateProviderRequest, String> {
    if raw.iter().find(|b| !b.is_ascii_whitespace()) == Some(&b'{') {
        return serde_json::from_slice(raw).map_err(|e| format!("Invalid body: {e}"));
    }
    let form = crate::util::parse_form_body(raw);
    let mut fields = serde_json::Map::new();
    for (key, value) in &form {
        if key == "models" || key == "enabled" {
            continue;
        }
        if key == "max_tokens_field" && value.is_empty() {
            continue;
        }
        fields.insert(key.clone(), serde_json::Value::String(value.clone()));
    }
    let posted_models = crate::util::form_values(raw, "models");
    if !posted_models.is_empty() {
        let models: Vec<serde_json::Value> = posted_models
            .iter()
            .flat_map(|value| value.split(','))
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(|m| serde_json::Value::String(m.to_string()))
            .collect();
        fields.insert("models".to_string(), serde_json::Value::Array(models));
    }
    fields.insert(
        "enabled".to_string(),
        serde_json::Value::Bool(crate::config_vars::form_bool(&form, "enabled")),
    );
    serde_json::from_value(serde_json::Value::Object(fields))
        .map_err(|e| format!("Invalid body: {e}"))
}

/// Refuse a `max_tokens_field` on a protocol that has no use for one.
///
/// The Anthropic Messages API carries the budget in a single `max_tokens`
/// field, so `providers::anthropic`'s encoder never reads the override.
/// Storing one there would be a setting an admin can see in the row and on
/// `GET /b/llm/api/providers` and that changes nothing on the wire, which is
/// worth a 400 rather than a shrug. Both call sites check the configuration
/// they are about to store — on the patch route that is the *patched* one — so
/// the message names the way out an admin actually has: clearing the override
/// in the very request that switches the protocol is accepted.
fn check_max_tokens_field(cfg: &ProviderConfig) -> Result<(), String> {
    match cfg.max_tokens_field {
        Some(field) if !cfg.protocol.accepts_max_tokens_field() => Err(format!(
            "`max_tokens_field` ({}) does not apply to the `{}` protocol: its \
             request body has a single budget field — omit the field, or send \
             `\"max_tokens_field\": null` in this same request to clear an \
             override the row already holds",
            field.as_str(),
            cfg.protocol.as_str()
        )),
        _ => Ok(()),
    }
}

/// `POST /b/llm/api/providers` — create. The typed body requires `name`,
/// `protocol` (one of the `ProviderProtocol` tokens) and `endpoint`;
/// `key_var`, `models`, `enabled` are optional. Admin-only.
///
/// Accepts the admin page's form body as well as JSON — see
/// [`parse_create_provider_body`].
pub(in crate::blocks::llm) async fn create_provider(
    block: &LlmBlock,
    ctx: &dyn Context,
    _msg: &Message,
    input: InputStream,
) -> OutputStream {
    // Before the body is even read: a runtime that cannot configure a
    // provider router must not store a provider row.
    if let Err(refusal) = require_provider_management(block) {
        return refusal;
    }

    let raw = match input.collect_to_bytes().await {
        Ok(bytes) => bytes,
        Err(e) => return OutputStream::error(e),
    };
    let body: CreateProviderRequest = match parse_create_provider_body(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&e),
    };

    // Presence is enforced by the type; emptiness still has to be, because
    // `""` is a valid JSON string and neither a usable name nor a URL.
    if let Err(e) = crate::blocks::llm::schema::validate_provider_name(&body.name) {
        return err_bad_request(&e);
    }
    if body.endpoint.is_empty() {
        return err_bad_request("`endpoint` is required");
    }
    // SSRF: an admin must not be able to point a provider at internal infra.
    // Same gate the config `_URL` write surfaces use; the outbound client
    // re-checks at call time (resolve-before-connect), this fails fast on save.
    if let Err(e) = crate::util::validate_url_value(&body.endpoint) {
        return err_bad_request(&format!("invalid `endpoint`: {e}"));
    }

    let mut cfg = ProviderConfig::new(body.name, body.protocol, body.endpoint);
    if let Some(k) = body.key_var.filter(|s| !s.is_empty()) {
        cfg.key_var = Some(k);
    }
    cfg.max_tokens_field = body.max_tokens_field;
    if let Some(m) = body.models {
        cfg.models = m;
    }
    if let Some(e) = body.enabled {
        cfg.enabled = e;
    }
    if let Err(e) = check_max_tokens_field(&cfg) {
        return err_bad_request(&e);
    }

    if let Err(e) = check_key_var_readable(ctx, &cfg).await {
        return crud::db_error_internal(e, "Failed to read the provider's key_var");
    }

    let mut data = config_to_row(&cfg);
    crate::util::stamp_created(&mut data);

    // `providers.name` is UNIQUE, and the name is the one unique value this
    // write sets: a duplicate is that name being taken.
    let record = match db::create(ctx, PROVIDERS_TABLE, data).await {
        Ok(r) => r,
        Err(e) => {
            return crud::taken_key_or_db_error(
                e,
                crud::TakenKey::new("provider", "name", &cfg.name),
                "Database error",
            )
        }
    };

    if let Err(e) = reload_provider_service(ctx, block.provider_admin.as_ref()).await {
        return crud::db_error_internal(e, "reload_provider_service failed");
    }

    ok_json(&ProviderView::from_config(&record.id, &cfg))
}

/// `PATCH /b/llm/api/providers/:id` — partial update. Admin-only.
pub(in crate::blocks::llm) async fn update_provider(
    block: &LlmBlock,
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    if let Err(refusal) = require_provider_management(block) {
        return refusal;
    }

    let id = match crud::path_id(msg, "Provider") {
        Ok(value) => value.to_string(),
        Err(response) => return response,
    };

    let raw = match input.collect_to_bytes().await {
        Ok(bytes) => bytes,
        Err(e) => return OutputStream::error(e),
    };
    let body: UpdateProviderRequest = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };
    // Refused before any row is read, like the protocol.
    if let Some(name) = body.name.as_deref().filter(|s| !s.is_empty()) {
        if let Err(e) = crate::blocks::llm::schema::validate_provider_name(name) {
            return err_bad_request(&e);
        }
    }

    // Load existing record so we can apply the patch on top of stored values.
    let existing = match db::get(ctx, PROVIDERS_TABLE, &id).await {
        Ok(r) => r,
        Err(e) => return crud::db_error(e, "Provider not found", "Database error"),
    };
    let mut cfg = match row_to_config(&existing) {
        Ok(c) => c,
        Err(e) => return err_internal("Stored provider row invalid", e),
    };

    if let Some(n) = body.name.filter(|s| !s.is_empty()) {
        cfg.name = n;
    }
    if let Some(p) = body.protocol {
        cfg.protocol = p;
    }
    if let Some(e) = body.endpoint.filter(|s| !s.is_empty()) {
        // SSRF: re-validate on edit (see create_provider) so an update can't
        // smuggle in an internal endpoint that create rejected.
        if let Err(err) = crate::util::validate_url_value(&e) {
            return err_bad_request(&format!("invalid `endpoint`: {err}"));
        }
        cfg.endpoint = e;
    }
    // Both nullable fields: present at all — `null` included — replaces the
    // stored value; a body that omits the key leaves it alone. An empty
    // `key_var` string clears it too, which is what it means to the create
    // form; this route takes JSON only, and the admin page has no edit form.
    // See `UpdateProviderRequest`.
    let names_key_var = body.key_var.is_some();
    if let Some(k) = body.key_var {
        cfg.key_var = k.filter(|s| !s.is_empty());
    }
    if let Some(f) = body.max_tokens_field {
        cfg.max_tokens_field = f;
    }
    if let Some(m) = body.models {
        cfg.models = m;
    }
    if let Some(e) = body.enabled {
        cfg.enabled = e;
    }
    // Checked on the patched config, not on the body: switching a provider to
    // `anthropic` while an override is stored produces the same unusable
    // pairing as sending both in one body.
    if let Err(e) = check_max_tokens_field(&cfg) {
        return err_bad_request(&e);
    }

    // Only a body that names `key_var` is checked: a patch that leaves the
    // stored variable alone must still reach a row whose key has become
    // unreadable since it was saved, so the admin can disable, rename or
    // repoint it rather than meet a 403 on every edit. The reload leaves such
    // a row out whatever this patch sets.
    if names_key_var {
        if let Err(e) = check_key_var_readable(ctx, &cfg).await {
            return crud::db_error_internal(e, "Failed to read the provider's key_var");
        }
    }

    let mut data = config_to_row(&cfg);
    crate::util::stamp_updated(&mut data);

    // The row is rewritten whole, so `providers.name` is always among the
    // values set; a duplicate is a rename onto a name another provider holds.
    let record = match db::update(ctx, PROVIDERS_TABLE, &id, data).await {
        Ok(r) => r,
        Err(e) => {
            return crud::taken_key_or(e, crud::TakenKey::new("provider", "name", &cfg.name), |e| {
                crud::db_error(e, "Provider not found", "Database error")
            })
        }
    };

    if let Err(e) = reload_provider_service(ctx, block.provider_admin.as_ref()).await {
        return crud::db_error_internal(e, "reload_provider_service failed");
    }

    ok_json(&ProviderView::from_config(&record.id, &cfg))
}

/// `DELETE /b/llm/api/providers/:id` — remove. Admin-only.
///
/// The providers table's Delete button swaps the answer over its own row
/// (`hx-target="closest tr"`, `outerHTML`), so an `HX-Request` gets an empty
/// HTML body — the row goes — while an API caller gets the JSON receipt. A
/// JSON body swapped over the row replaced it with `{"deleted":true}` as text.
pub(in crate::blocks::llm) async fn delete_provider(
    block: &LlmBlock,
    ctx: &dyn Context,
    msg: &Message,
) -> OutputStream {
    if let Err(refusal) = require_provider_management(block) {
        return refusal;
    }

    let id = match crud::path_id(msg, "Provider") {
        Ok(value) => value.to_string(),
        Err(response) => return response,
    };
    if let Err(e) = db::delete(ctx, PROVIDERS_TABLE, &id).await {
        return crud::db_error(e, "Provider not found", "Database error");
    }

    if let Err(e) = reload_provider_service(ctx, block.provider_admin.as_ref()).await {
        return crud::db_error_internal(e, "reload_provider_service failed");
    }

    if crate::ui::is_htmx(msg) {
        return crate::ui::html_response(maud::html! {});
    }
    ok_json(&ProviderDeleteResponse { deleted: true })
}

/// `POST /b/llm/api/providers/:id/discover-models` — call the provider's
/// `/v1/models` endpoint, persist the discovered list back to the row, and
/// return the new model list. Admin-only.
///
/// The write is a one-column write (`models`, via [`models_row`]) and
/// not a re-encode of the row this handler read: the provider's HTTP endpoint
/// is awaited in between, and an admin editing the same provider across that
/// window would otherwise have their change written back at its stale value.
pub(in crate::blocks::llm) async fn discover_models(
    block: &LlmBlock,
    ctx: &dyn Context,
    msg: &Message,
) -> OutputStream {
    // Discovery writes the discovered list back to the row, so it is a
    // mutating handler and refuses on the same terms as the other three.
    if let Err(refusal) = require_provider_management(block) {
        return refusal;
    }

    let id = match crud::path_id(msg, "Provider") {
        Ok(value) => value.to_string(),
        Err(response) => return response,
    };

    // Resolve the provider name from the row — discover_models is keyed by
    // provider name (== ProviderConfig::name), not by row id.
    let existing = match db::get(ctx, PROVIDERS_TABLE, &id).await {
        Ok(r) => r,
        Err(e) => return crud::db_error(e, "Provider not found", "Database error"),
    };
    let mut cfg = match row_to_config(&existing) {
        Ok(c) => c,
        Err(e) => return err_internal("Stored provider row invalid", e),
    };

    // Make sure the in-memory service knows about this provider — discover
    // looks up by name, and the service may be empty if the process just
    // started or the row is disabled (and so was excluded from the last
    // configure call).
    if let Err(e) = reload_provider_service(ctx, block.provider_admin.as_ref()).await {
        return crud::db_error_internal(e, "reload_provider_service failed");
    }

    let models = match block.provider_admin.discover_models(&cfg.name).await {
        Ok(m) => m,
        Err(e) => return llm_error_response("discover_models failed", e),
    };
    cfg.models = models.into_iter().map(|m| m.model_id).collect();

    let mut data = models_row(&cfg.models);
    crate::util::stamp_updated(&mut data);
    if let Err(e) = db::update(ctx, PROVIDERS_TABLE, &id, data).await {
        return crud::db_error(e, "Provider not found", "Database error");
    }

    if let Err(e) = reload_provider_service(ctx, block.provider_admin.as_ref()).await {
        return crud::db_error_internal(e, "reload_provider_service failed");
    }

    ok_json(&DiscoveredModelsResponse { models: cfg.models })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use wafer_run::{streams::output::TerminalNotResponse, ErrorCode};

    use super::*;
    // Read only by `reload_provider_service_resolves_key_var_into_api_key`,
    // which needs the concrete `ProviderLlmService` and so carries the same
    // gate.
    #[cfg(feature = "llm")]
    use crate::blocks::llm::EXAMPLE_KEY_VAR;
    use crate::{
        blocks::llm::{
            providers::config::ProviderProtocol,
            routes::test_support::{
                admin_msg, routed, stub_block, PanicCtx, RecordingProviderAdmin,
            },
        },
        test_support::{output_json, TestContext},
    };

    #[tokio::test]
    async fn create_provider_returns_bad_request_on_invalid_json() {
        let block = stub_block();
        let ctx = PanicCtx;
        let msg = admin_msg("create", "/b/llm/api/providers");
        let input = InputStream::from_bytes(b"not json".to_vec());

        let out = create_provider(&block, &ctx, &msg, input).await;
        match out.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::InvalidArgument);
                assert!(
                    e.message.contains("Invalid body"),
                    "expected Invalid body, got: {}",
                    e.message
                );
            }
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_provider_requires_name() {
        let block = stub_block();
        let ctx = PanicCtx;
        let msg = admin_msg("create", "/b/llm/api/providers");
        let input =
            InputStream::from_bytes(br#"{"protocol":"open_ai","endpoint":"https://x"}"#.to_vec());

        let out = create_provider(&block, &ctx, &msg, input).await;
        match out.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::InvalidArgument);
                assert!(e.message.contains("name"), "got: {}", e.message);
            }
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
    }

    /// `protocol` is typed as the `ProviderProtocol` enum on the way in, so an
    /// alias is refused by deserialization and the refusal names the accepted
    /// values — which is what a caller who sent `openai` needs to see.
    #[tokio::test]
    async fn create_provider_rejects_unknown_protocol() {
        let block = stub_block();
        let ctx = PanicCtx;
        let msg = admin_msg("create", "/b/llm/api/providers");
        let input = InputStream::from_bytes(
            br#"{"name":"x","protocol":"openai","endpoint":"https://x"}"#.to_vec(),
        );

        let out = create_provider(&block, &ctx, &msg, input).await;
        match out.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::InvalidArgument);
                assert!(
                    e.message.contains("open_ai_compatible"),
                    "the refusal must name the accepted values, got: {}",
                    e.message
                );
            }
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
    }

    /// Same enum on the patch body: `""` is outside it and is refused before
    /// any row is read. The untyped body used to treat an empty `protocol` as
    /// "not provided" and silently apply the rest of the patch.
    #[tokio::test]
    async fn update_provider_rejects_an_empty_protocol() {
        let block = stub_block();
        let ctx = PanicCtx;
        let msg = routed(admin_msg("update", "/b/llm/api/providers/row-1"));
        let input = InputStream::from_bytes(br#"{"protocol":""}"#.to_vec());

        let out = update_provider(&block, &ctx, &msg, input).await;
        match out.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::InvalidArgument);
                assert!(
                    e.message.contains("open_ai_compatible"),
                    "the refusal must name the accepted values, got: {}",
                    e.message
                );
            }
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
    }

    /// A provider's name is its `backend_id` on the llm router, which the llm
    /// handler refuses on every call when it holds a `/` (the model resource
    /// `{backend}/{model}` would be ambiguous). So the name is refused where
    /// the admin can still choose another, on create and on rename, before
    /// the store is touched (`PanicCtx`).
    #[tokio::test]
    async fn a_provider_name_with_a_slash_is_refused() {
        let block = stub_block();
        let create = create_provider(
            &block,
            &PanicCtx,
            &admin_msg("create", "/b/llm/api/providers"),
            InputStream::from_bytes(
                br#"{"name":"openai/x","protocol":"open_ai","endpoint":"https://x.example"}"#
                    .to_vec(),
            ),
        )
        .await;
        let rename = update_provider(
            &block,
            &PanicCtx,
            &routed(admin_msg("update", "/b/llm/api/providers/row-1")),
            InputStream::from_bytes(br#"{"name":"openai/x"}"#.to_vec()),
        )
        .await;
        for (what, out) in [("create", create), ("rename", rename)] {
            match out.collect_buffered().await {
                Err(TerminalNotResponse::Error(e)) => {
                    assert_eq!(e.code, ErrorCode::InvalidArgument, "{what}");
                    assert!(e.message.contains('/'), "{what}: {}", e.message);
                }
                other => panic!("{what}: expected InvalidArgument, got {other:?}"),
            }
        }
    }

    /// SSRF: an admin must not be able to point a provider endpoint at
    /// internal infrastructure. The rejection happens before any DB write, so
    /// `PanicCtx` (which panics if the store is touched) staying quiet also
    /// proves the check fails fast, ahead of `db::create`.
    #[tokio::test]
    async fn create_provider_rejects_internal_endpoint() {
        for endpoint in [
            "https://169.254.169.254/latest/meta-data/", // cloud metadata (link-local)
            "http://10.0.0.1/v1",                        // RFC1918 private
            "https://100.64.0.1/v1",                     // CGNAT
            "https://metadata.google.internal/v1",       // metadata hostname
        ] {
            let block = stub_block();
            let ctx = PanicCtx;
            let msg = admin_msg("create", "/b/llm/api/providers");
            let body = format!(r#"{{"name":"x","protocol":"open_ai","endpoint":"{endpoint}"}}"#);
            let input = InputStream::from_bytes(body.into_bytes());

            let out = create_provider(&block, &ctx, &msg, input).await;
            match out.collect_buffered().await {
                Err(TerminalNotResponse::Error(e)) => {
                    assert_eq!(e.code, ErrorCode::InvalidArgument, "endpoint {endpoint}");
                    assert!(
                        e.message.contains("endpoint"),
                        "endpoint {endpoint}: got: {}",
                        e.message
                    );
                }
                other => panic!("expected InvalidArgument for {endpoint}, got {other:?}"),
            }
        }
    }

    /// The API key is referenced by variable name (`key_var`), never sent
    /// inline. A body carrying `api_key` must be refused by name, not
    /// silently dropped after the secret transited the request — the admin
    /// would otherwise get a 200 and a provider that runs unauthenticated.
    #[tokio::test]
    async fn create_provider_refuses_an_inline_api_key() {
        let block = stub_block();
        let ctx = PanicCtx;
        let msg = admin_msg("create", "/b/llm/api/providers");
        let input = InputStream::from_bytes(
            br#"{"name":"x","protocol":"open_ai","endpoint":"https://api.openai.com/v1","api_key":"sk-inline"}"#
                .to_vec(),
        );

        let out = create_provider(&block, &ctx, &msg, input).await;
        match out.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::InvalidArgument);
                assert!(
                    e.message.contains("api_key"),
                    "the refusal must name the unknown field, got: {}",
                    e.message
                );
            }
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
    }

    /// Same on the patch body.
    #[tokio::test]
    async fn update_provider_refuses_an_inline_api_key() {
        let block = stub_block();
        let ctx = PanicCtx;
        let msg = routed(admin_msg("update", "/b/llm/api/providers/row-1"));
        let input = InputStream::from_bytes(br#"{"api_key":"sk-inline"}"#.to_vec());

        let out = update_provider(&block, &ctx, &msg, input).await;
        match out.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::InvalidArgument);
                assert!(
                    e.message.contains("api_key"),
                    "the refusal must name the unknown field, got: {}",
                    e.message
                );
            }
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn update_provider_requires_id() {
        let block = stub_block();
        let ctx = PanicCtx;
        // Path has no id segment after the prefix.
        let msg = admin_msg("update", "/b/llm/api/providers/");
        let input = InputStream::from_bytes(b"{}".to_vec());

        let out = update_provider(&block, &ctx, &msg, input).await;
        match out.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::InvalidArgument);
                assert!(e.message.contains("provider ID"), "got: {}", e.message);
            }
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn delete_provider_requires_id() {
        let block = stub_block();
        let ctx = PanicCtx;
        let msg = admin_msg("delete", "/b/llm/api/providers/");

        let out = delete_provider(&block, &ctx, &msg).await;
        match out.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::InvalidArgument);
            }
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
    }

    /// Provider handlers read the id the table bound, nothing else.
    #[test]
    fn provider_id_is_bound_by_the_table() {
        let m = routed(admin_msg("update", "/b/llm/api/providers/abc123"));
        assert_eq!(m.var("id"), "abc123");

        let m2 = routed(admin_msg(
            "create",
            "/b/llm/api/providers/abc123/discover-models",
        ));
        assert_eq!(m2.var("id"), "abc123");

        // A path with no id segment matches no row and binds nothing; the
        // handler then answers InvalidArgument (see `update_provider_requires_id`).
        let mut m3 = admin_msg("delete", "/b/llm/api/providers/");
        assert!(crate::endpoint_match::dispatch(&mut m3, crate::blocks::llm::ROUTES).is_none());
        assert_eq!(m3.var("id"), "");
    }

    // -----------------------------------------------------------------
    // Wire shape — what the published schema is derived from
    // -----------------------------------------------------------------

    const KEY_VAR: &str = "IMPRESSPRESS__LLM__TEST_OPENAI_KEY";

    /// Shaped like a real key so a substring search over a response body is
    /// a meaningful leak check rather than a match on a common word.
    const SECRET: &str = "sk-live-0123456789abcdefABCDEF";

    /// A block over the recording provider-admin handle, on a context where
    /// `KEY_VAR` resolves to `SECRET`. The handle is returned separately so
    /// a test can read back what the reload resolved into it.
    async fn keyed_fixture() -> (TestContext, Arc<RecordingProviderAdmin>, LlmBlock) {
        let mut ctx = TestContext::with_llm().await;
        ctx.set_config(KEY_VAR, SECRET);
        let admin = Arc::new(RecordingProviderAdmin::default());
        let block = LlmBlock::new(admin.clone());
        (ctx, admin, block)
    }

    fn json_input(value: serde_json::Value) -> InputStream {
        InputStream::from_bytes(serde_json::to_vec(&value).expect("serialize body"))
    }

    fn create_body() -> InputStream {
        json_input(serde_json::json!({
            "name": "openai-main",
            "protocol": "open_ai",
            "endpoint": "https://api.openai.com/v1",
            "key_var": KEY_VAR,
            "models": ["gpt-4o"],
        }))
    }

    /// The field set a provider row publishes, on every endpoint that returns
    /// one. This is the assertion the `/openapi.json` schema rests on.
    fn assert_provider_view(label: &str, row: &serde_json::Value) {
        let mut got: Vec<&str> = row
            .as_object()
            .unwrap_or_else(|| panic!("{label}: expected an object, got {row}"))
            .keys()
            .map(String::as_str)
            .collect();
        got.sort_unstable();
        assert_eq!(
            got,
            [
                "enabled",
                "endpoint",
                "id",
                "key_var",
                "max_tokens_field",
                "models",
                "name",
                "protocol"
            ],
            "{label}: the wire field set must equal ProviderView's, or the published \
             schema describes something the handler does not emit"
        );
    }

    /// A variable the fixture refuses to let the llm block read, as WRAP
    /// refuses another block's secret.
    const UNREADABLE_VAR: &str = "IMPRESSPRESS__LLM__TEST_UNREADABLE_KEY";

    fn refused() -> WaferError {
        WaferError::new(
            ErrorCode::PermissionDenied,
            "WRAP: impresspress/llm holds no grant on the key variable",
        )
    }

    /// Every provider row stored, as `(name, key_var)`.
    async fn stored_key_vars(ctx: &TestContext) -> Vec<(String, Option<String>)> {
        db_read::list_bounded(ctx, PROVIDERS_TABLE, vec![], Bound::Curated("test"))
            .await
            .expect("list providers")
            .iter()
            .map(|rec| {
                let cfg = row_to_config(rec).expect("stored row decodes");
                (cfg.name, cfg.key_var)
            })
            .collect()
    }

    /// A create naming a `key_var` the block cannot read is the classified
    /// denial, and nothing is stored or configured.
    #[tokio::test]
    async fn a_create_with_an_unreadable_key_var_stores_nothing() {
        let (mut ctx, admin, block) = keyed_fixture().await;
        ctx.refuse_config_reads_of(UNREADABLE_VAR, refused());

        let out = create_provider(
            &block,
            &ctx,
            &admin_msg("create", "/b/llm/api/providers"),
            json_input(serde_json::json!({
                "name": "openai-main",
                "protocol": "open_ai",
                "endpoint": "https://api.openai.com/v1",
                "key_var": UNREADABLE_VAR,
            })),
        )
        .await;
        assert_eq!(
            crate::test_support::output_http_json(out).await,
            serde_json::json!({ "error": "PermissionDenied", "message": "Access denied" }),
        );
        assert_eq!(stored_key_vars(&ctx).await, vec![]);
        assert!(admin.providers_snapshot().is_empty());
    }

    /// An update pointing a provider at an unreadable `key_var` is refused
    /// and the stored row keeps the variable it had.
    #[tokio::test]
    async fn an_update_to_an_unreadable_key_var_leaves_the_row_alone() {
        let (mut ctx, _admin, block) = keyed_fixture().await;
        let created = output_json(
            create_provider(
                &block,
                &ctx,
                &admin_msg("create", "/b/llm/api/providers"),
                create_body(),
            )
            .await,
        )
        .await;
        let id = created["id"].as_str().expect("created id").to_string();
        ctx.refuse_config_reads_of(UNREADABLE_VAR, refused());

        let out = update_provider(
            &block,
            &ctx,
            &routed(admin_msg("update", &format!("/b/llm/api/providers/{id}"))),
            json_input(serde_json::json!({ "key_var": UNREADABLE_VAR })),
        )
        .await;
        assert_eq!(
            crate::test_support::output_http_json(out).await,
            serde_json::json!({ "error": "PermissionDenied", "message": "Access denied" }),
        );
        assert_eq!(
            stored_key_vars(&ctx).await,
            vec![("openai-main".to_string(), Some(KEY_VAR.to_string()))]
        );
    }

    /// A stored provider whose key has become unreadable can still be
    /// disabled: a patch that does not name `key_var` is not refused over the
    /// variable it leaves alone.
    #[tokio::test]
    async fn a_provider_with_a_stale_key_grant_can_still_be_disabled() {
        let (mut ctx, _admin, block) = keyed_fixture().await;
        let mut cfg = ProviderConfig::new(
            "stale".to_string(),
            ProviderProtocol::OpenAi,
            "https://api.openai.com/v1".to_string(),
        );
        cfg.key_var = Some(UNREADABLE_VAR.to_string());
        let id = db::create(&ctx, PROVIDERS_TABLE, config_to_row(&cfg))
            .await
            .expect("seed provider row")
            .id;
        ctx.refuse_config_reads_of(UNREADABLE_VAR, refused());

        let out = update_provider(
            &block,
            &ctx,
            &routed(admin_msg("update", &format!("/b/llm/api/providers/{id}"))),
            json_input(serde_json::json!({ "enabled": false })),
        )
        .await;
        let body = output_json(out).await;
        assert_eq!(body["enabled"], serde_json::json!(false), "{body}");

        let stored = db::get(&ctx, PROVIDERS_TABLE, &id)
            .await
            .expect("row still there");
        let stored = row_to_config(&stored).expect("stored row decodes");
        assert!(!stored.enabled, "the disable must be stored");
        assert_eq!(stored.key_var.as_deref(), Some(UNREADABLE_VAR));
    }

    /// A stored provider whose key can no longer be read is left out of the
    /// reload — never run keyless — and the other providers still load.
    #[tokio::test]
    async fn a_reload_skips_a_provider_whose_key_cannot_be_read() {
        let (mut ctx, admin, _block) = keyed_fixture().await;
        for (name, var) in [("healthy", KEY_VAR), ("poisoned", UNREADABLE_VAR)] {
            let mut cfg = ProviderConfig::new(
                name.to_string(),
                ProviderProtocol::OpenAi,
                "https://api.openai.com/v1".to_string(),
            );
            cfg.key_var = Some(var.to_string());
            db::create(&ctx, PROVIDERS_TABLE, config_to_row(&cfg))
                .await
                .expect("seed provider row");
        }
        ctx.refuse_config_reads_of(UNREADABLE_VAR, refused());

        reload_provider_service(&ctx, admin.as_ref())
            .await
            .expect("one unreadable key does not fail the reload");
        let loaded: Vec<(String, Option<String>)> = admin
            .providers_snapshot()
            .into_iter()
            .map(|p| (p.name, p.api_key))
            .collect();
        assert_eq!(
            loaded,
            vec![("healthy".to_string(), Some(SECRET.to_string()))]
        );
    }

    /// The resolved key sits on the very `ProviderConfig`s the handlers hold
    /// (`block.provider_admin`), one field away from every response. No
    /// endpoint may carry it, or an `api_key` field of any kind.
    #[tokio::test]
    async fn provider_endpoints_never_emit_the_resolved_api_key() {
        let (ctx, admin, block) = keyed_fixture().await;

        let created = output_json(
            create_provider(
                &block,
                &ctx,
                &admin_msg("create", "/b/llm/api/providers"),
                create_body(),
            )
            .await,
        )
        .await;
        let id = created["id"].as_str().expect("created id").to_string();

        // Control: after the create's reload the secret is on the handle the
        // handlers read. Without this the assertions below could pass because
        // nothing was ever within reach.
        assert_eq!(
            admin
                .providers_snapshot()
                .first()
                .and_then(|p| p.api_key.as_deref()),
            Some(SECRET),
            "fixture must resolve the key into the handlers' provider handle"
        );

        let listed = output_json(
            list_providers(&block, &ctx, &admin_msg("retrieve", "/b/llm/api/providers")).await,
        )
        .await;
        let updated = output_json(
            update_provider(
                &block,
                &ctx,
                &routed(admin_msg("update", &format!("/b/llm/api/providers/{id}"))),
                json_input(serde_json::json!({ "models": ["gpt-4o-mini"] })),
            )
            .await,
        )
        .await;
        let discovered = output_json(
            discover_models(
                &block,
                &ctx,
                &routed(admin_msg(
                    "create",
                    &format!("/b/llm/api/providers/{id}/discover-models"),
                )),
            )
            .await,
        )
        .await;

        for (label, body) in [
            ("create", &created),
            ("list", &listed),
            ("update", &updated),
            ("discover-models", &discovered),
        ] {
            let raw = body.to_string();
            assert!(
                !raw.contains(SECRET),
                "{label} leaked the resolved key: {raw}"
            );
            assert!(
                !raw.to_lowercase().contains("api_key"),
                "{label} published an api_key field: {raw}"
            );
        }
    }

    // -----------------------------------------------------------------
    // The per-provider output-token budget field
    // -----------------------------------------------------------------

    /// Refusals from these handlers, as `(code, message)`.
    async fn provider_refusal(out: OutputStream) -> (ErrorCode, String) {
        match out.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => (e.code, e.message),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    /// The stored rows, re-read through `GET /b/llm/api/providers` — the only
    /// way to tell a write that persisted from one the handler merely echoed
    /// back out of the config it had in hand.
    async fn stored_rows(ctx: &TestContext, block: &LlmBlock) -> Vec<serde_json::Value> {
        let listed = output_json(
            list_providers(block, ctx, &admin_msg("retrieve", "/b/llm/api/providers")).await,
        )
        .await;
        listed["providers"]
            .as_array()
            .expect("providers array")
            .clone()
    }

    async fn create_azure_style(
        ctx: &TestContext,
        block: &LlmBlock,
    ) -> (String, serde_json::Value) {
        let created = output_json(
            create_provider(
                block,
                ctx,
                &admin_msg("create", "/b/llm/api/providers"),
                json_input(serde_json::json!({
                    "name": "azure-reasoning",
                    "protocol": "open_ai_compatible",
                    "endpoint": "https://example.openai.azure.com/openai/v1",
                    "max_tokens_field": "max_completion_tokens",
                })),
            )
            .await,
        )
        .await;
        let id = created["id"].as_str().expect("created id").to_string();
        (id, created)
    }

    /// The configuration this whole field exists for: an Azure OpenAI
    /// reasoning deployment, which speaks `open_ai_compatible` but accepts
    /// only `max_completion_tokens`. It survives to the stored row, not just
    /// the create response.
    #[tokio::test]
    async fn a_declared_max_tokens_field_is_stored_and_published() {
        let (ctx, _admin, block) = keyed_fixture().await;
        let (_id, created) = create_azure_style(&ctx, &block).await;

        assert_eq!(created["max_tokens_field"], "max_completion_tokens");
        let rows = stored_rows(&ctx, &block).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0]["max_tokens_field"], "max_completion_tokens",
            "the row must carry it, not only the create echo"
        );
    }

    /// Omitted means "follow the protocol", and that is what the row says —
    /// `null`, never a spelling chosen on the admin's behalf.
    #[tokio::test]
    async fn an_omitted_max_tokens_field_stays_null() {
        let (ctx, _admin, block) = keyed_fixture().await;
        let created = output_json(
            create_provider(
                &block,
                &ctx,
                &admin_msg("create", "/b/llm/api/providers"),
                create_body(),
            )
            .await,
        )
        .await;

        assert_eq!(created["max_tokens_field"], serde_json::Value::Null);
        assert_eq!(
            stored_rows(&ctx, &block).await[0]["max_tokens_field"],
            serde_json::Value::Null
        );
    }

    /// A present `null` on the patch clears the override; an absent key
    /// leaves it alone. Both re-read from the store, because `update_provider`
    /// answers out of the config it just built either way.
    #[tokio::test]
    async fn a_null_max_tokens_field_clears_it_and_an_absent_one_does_not() {
        let (ctx, _admin, block) = keyed_fixture().await;
        let (id, _) = create_azure_style(&ctx, &block).await;

        let untouched = output_json(
            update_provider(
                &block,
                &ctx,
                &routed(admin_msg("update", &format!("/b/llm/api/providers/{id}"))),
                json_input(serde_json::json!({ "enabled": false })),
            )
            .await,
        )
        .await;
        assert_eq!(
            untouched["max_tokens_field"], "max_completion_tokens",
            "a patch that does not mention the field must not change it"
        );
        assert_eq!(
            stored_rows(&ctx, &block).await[0]["max_tokens_field"],
            "max_completion_tokens"
        );

        let cleared = output_json(
            update_provider(
                &block,
                &ctx,
                &routed(admin_msg("update", &format!("/b/llm/api/providers/{id}"))),
                json_input(serde_json::json!({ "max_tokens_field": null })),
            )
            .await,
        )
        .await;
        assert_eq!(cleared["max_tokens_field"], serde_json::Value::Null);
        assert_eq!(
            stored_rows(&ctx, &block).await[0]["max_tokens_field"],
            serde_json::Value::Null,
            "the cleared override must be gone from the row, not only the reply"
        );
    }

    /// `UpdateProviderRequest` documents an empty `key_var` as clearing the
    /// variable. The row is what has to say so: `config_to_row` feeds
    /// `db::update`, which sets only the columns it is handed, so a `key_var`
    /// the encoder omitted left the old variable name in place under a reply
    /// that said `null`.
    #[tokio::test]
    async fn an_emptied_key_var_is_cleared_in_the_stored_row() {
        let (ctx, _admin, block) = keyed_fixture().await;
        let created = output_json(
            create_provider(
                &block,
                &ctx,
                &admin_msg("create", "/b/llm/api/providers"),
                create_body(),
            )
            .await,
        )
        .await;
        let id = created["id"].as_str().expect("created id").to_string();
        assert_eq!(created["key_var"], KEY_VAR);

        let updated = output_json(
            update_provider(
                &block,
                &ctx,
                &routed(admin_msg("update", &format!("/b/llm/api/providers/{id}"))),
                json_input(serde_json::json!({ "key_var": "" })),
            )
            .await,
        )
        .await;
        assert_eq!(updated["key_var"], serde_json::Value::Null);
        assert_eq!(
            stored_rows(&ctx, &block).await[0]["key_var"],
            serde_json::Value::Null,
            "the reply said the variable was cleared; the row has to agree"
        );
    }

    /// `key_var` and `max_tokens_field` are the two fields a provider can hold
    /// as `null`, and they clear the same way: a present `null`. Sending one
    /// used to be a 200 that changed nothing, which is the worst answer of the
    /// three available.
    #[tokio::test]
    async fn a_null_key_var_clears_it_and_an_absent_one_does_not() {
        let (ctx, _admin, block) = keyed_fixture().await;
        let created = output_json(
            create_provider(
                &block,
                &ctx,
                &admin_msg("create", "/b/llm/api/providers"),
                create_body(),
            )
            .await,
        )
        .await;
        let id = created["id"].as_str().expect("created id").to_string();
        assert_eq!(created["key_var"], KEY_VAR);

        let untouched = output_json(
            update_provider(
                &block,
                &ctx,
                &routed(admin_msg("update", &format!("/b/llm/api/providers/{id}"))),
                json_input(serde_json::json!({ "enabled": false })),
            )
            .await,
        )
        .await;
        assert_eq!(
            untouched["key_var"], KEY_VAR,
            "a patch that does not mention the field must not change it"
        );

        let cleared = output_json(
            update_provider(
                &block,
                &ctx,
                &routed(admin_msg("update", &format!("/b/llm/api/providers/{id}"))),
                json_input(serde_json::json!({ "key_var": null })),
            )
            .await,
        )
        .await;
        assert_eq!(cleared["key_var"], serde_json::Value::Null);
        assert_eq!(
            stored_rows(&ctx, &block).await[0]["key_var"],
            serde_json::Value::Null,
            "a present `null` clears the variable in the row, not only in the reply"
        );
    }

    /// A `ProviderAdmin` that edits the provider row from *inside*
    /// `discover_models` — the admin who changes `key_var` between the read at
    /// the top of the handler and the write at the bottom. `discover_models`
    /// awaits the provider's HTTP endpoint at exactly this point, so the
    /// window is real rather than contrived.
    struct EditsTheRowDuringDiscovery {
        ctx: std::sync::Mutex<Option<Arc<dyn Context>>>,
        id: std::sync::Mutex<String>,
    }

    /// The variable name the concurrent edit sets, distinct from `KEY_VAR` so
    /// the row can only hold it if the edit survived.
    const CONCURRENT_KEY_VAR: &str = "IMPRESSPRESS__LLM__ROTATED_KEY";

    #[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
    #[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
    impl ProviderAdmin for EditsTheRowDuringDiscovery {
        fn manages_providers(&self) -> bool {
            true
        }

        fn configure(&self, _providers: Vec<ProviderConfig>) -> Result<(), LlmError> {
            Ok(())
        }

        fn providers_snapshot(&self) -> Vec<ProviderConfig> {
            Vec::new()
        }

        async fn discover_models(
            &self,
            provider_name: &str,
        ) -> Result<Vec<wafer_core::interfaces::llm::service::ModelInfo>, LlmError> {
            let ctx = self
                .ctx
                .lock()
                .expect("ctx lock")
                .clone()
                .expect("ctx handed to the fixture");
            let id = self.id.lock().expect("id lock").clone();
            let mut data = std::collections::HashMap::new();
            data.insert(
                "key_var".to_string(),
                serde_json::Value::String(CONCURRENT_KEY_VAR.to_string()),
            );
            db::update(ctx.as_ref(), PROVIDERS_TABLE, &id, data)
                .await
                .expect("concurrent key_var edit");
            Ok(vec![wafer_core::interfaces::llm::service::ModelInfo::new(
                provider_name,
                "gpt-4o",
                "GPT-4o",
            )])
        }
    }

    /// Discovery learns a model list and nothing else, so it writes one
    /// column. Re-encoding the whole config would write every other column
    /// back at the value it held before the provider call, silently reverting
    /// an edit that landed in that window.
    #[tokio::test]
    async fn discovery_does_not_write_back_columns_it_did_not_learn() {
        let mut ctx = TestContext::with_llm().await;
        ctx.set_config(KEY_VAR, SECRET);
        let admin = Arc::new(EditsTheRowDuringDiscovery {
            ctx: std::sync::Mutex::new(None),
            id: std::sync::Mutex::new(String::new()),
        });
        let block = LlmBlock::new(admin.clone());

        let created = output_json(
            create_provider(
                &block,
                &ctx,
                &admin_msg("create", "/b/llm/api/providers"),
                create_body(),
            )
            .await,
        )
        .await;
        let id = created["id"].as_str().expect("created id").to_string();
        assert_eq!(created["key_var"], KEY_VAR);

        *admin.ctx.lock().expect("ctx lock") = Some(ctx.clone_arc());
        *admin.id.lock().expect("id lock") = id.clone();

        let discovered = output_json(
            discover_models(
                &block,
                &ctx,
                &routed(admin_msg(
                    "create",
                    &format!("/b/llm/api/providers/{id}/discover-models"),
                )),
            )
            .await,
        )
        .await;
        assert_eq!(discovered, serde_json::json!({ "models": ["gpt-4o"] }));

        let row = &stored_rows(&ctx, &block).await[0];
        assert_eq!(
            row["models"],
            serde_json::json!(["gpt-4o"]),
            "the discovered list is the one column discovery does own"
        );
        assert_eq!(
            row["key_var"], CONCURRENT_KEY_VAR,
            "an edit that landed while the provider was being queried must \
             survive discovery's write"
        );
    }

    /// Anthropic's Messages API has one budget field, so an override there is
    /// a setting that cannot do anything. It is refused by name rather than
    /// stored and ignored.
    #[tokio::test]
    async fn an_anthropic_provider_may_not_declare_a_max_tokens_field() {
        let (ctx, _admin, block) = keyed_fixture().await;
        let out = create_provider(
            &block,
            &ctx,
            &admin_msg("create", "/b/llm/api/providers"),
            json_input(serde_json::json!({
                "name": "anthropic-main",
                "protocol": "anthropic",
                "endpoint": "https://api.anthropic.com/v1",
                "max_tokens_field": "max_completion_tokens",
            })),
        )
        .await;

        let (code, message) = provider_refusal(out).await;
        assert_eq!(code, ErrorCode::InvalidArgument);
        assert!(
            message.contains("max_tokens_field") && message.contains("anthropic"),
            "the refusal must name the field and the protocol, got: {message}"
        );
        assert!(
            stored_rows(&ctx, &block).await.is_empty(),
            "a refused create must leave no row behind"
        );
    }

    /// The same pairing reached the other way round — switching an existing
    /// provider to `anthropic` while its override is still stored — is refused
    /// on the patched configuration, and the stored row is untouched.
    #[tokio::test]
    async fn switching_a_provider_with_an_override_to_anthropic_is_refused() {
        let (ctx, _admin, block) = keyed_fixture().await;
        let (id, _) = create_azure_style(&ctx, &block).await;

        let out = update_provider(
            &block,
            &ctx,
            &routed(admin_msg("update", &format!("/b/llm/api/providers/{id}"))),
            json_input(serde_json::json!({ "protocol": "anthropic" })),
        )
        .await;

        let (code, message) = provider_refusal(out).await;
        assert_eq!(code, ErrorCode::InvalidArgument);
        assert!(message.contains("max_tokens_field"), "got: {message}");
        let rows = stored_rows(&ctx, &block).await;
        assert_eq!(rows[0]["protocol"], "open_ai_compatible");
        assert_eq!(rows[0]["max_tokens_field"], "max_completion_tokens");
    }

    /// Clearing the override and switching protocol in one body is accepted —
    /// the check reads the patched configuration, so the two changes are
    /// judged together rather than as the body's field order happens to fall.
    #[tokio::test]
    async fn clearing_the_override_in_the_same_patch_that_switches_to_anthropic_is_accepted() {
        let (ctx, _admin, block) = keyed_fixture().await;
        let (id, _) = create_azure_style(&ctx, &block).await;

        let updated = output_json(
            update_provider(
                &block,
                &ctx,
                &routed(admin_msg("update", &format!("/b/llm/api/providers/{id}"))),
                json_input(serde_json::json!({
                    "protocol": "anthropic",
                    "max_tokens_field": null,
                    "endpoint": "https://api.anthropic.com/v1",
                })),
            )
            .await,
        )
        .await;

        assert_eq!(updated["protocol"], "anthropic");
        assert_eq!(updated["max_tokens_field"], serde_json::Value::Null);
    }

    /// Every provider endpoint publishes the one row projection, and the
    /// acknowledgement shapes are exactly what their schemas say.
    #[tokio::test]
    async fn provider_endpoints_publish_exactly_the_view_fields() {
        let (ctx, _admin, block) = keyed_fixture().await;

        let created = output_json(
            create_provider(
                &block,
                &ctx,
                &admin_msg("create", "/b/llm/api/providers"),
                create_body(),
            )
            .await,
        )
        .await;
        assert_provider_view("create", &created);
        assert_eq!(created["name"], "openai-main");
        assert_eq!(created["protocol"], "open_ai");
        assert_eq!(created["endpoint"], "https://api.openai.com/v1");
        assert_eq!(created["key_var"], KEY_VAR);
        assert_eq!(created["models"], serde_json::json!(["gpt-4o"]));
        assert_eq!(created["enabled"], true);
        let id = created["id"].as_str().expect("created id").to_string();

        let listed = output_json(
            list_providers(&block, &ctx, &admin_msg("retrieve", "/b/llm/api/providers")).await,
        )
        .await;
        let rows = listed["providers"].as_array().expect("providers array");
        assert_eq!(rows.len(), 1);
        assert_provider_view("list", &rows[0]);
        assert_eq!(
            rows[0], created,
            "list must publish the row the create returned"
        );

        let updated = output_json(
            update_provider(
                &block,
                &ctx,
                &routed(admin_msg("update", &format!("/b/llm/api/providers/{id}"))),
                json_input(serde_json::json!({
                    "models": ["gpt-4o-mini"],
                    "enabled": false,
                })),
            )
            .await,
        )
        .await;
        assert_provider_view("update", &updated);
        assert_eq!(updated["id"], id);
        assert_eq!(updated["models"], serde_json::json!(["gpt-4o-mini"]));
        assert_eq!(updated["enabled"], false);
        assert_eq!(
            updated["key_var"], KEY_VAR,
            "fields absent from the patch are retained"
        );

        let discovered = output_json(
            discover_models(
                &block,
                &ctx,
                &routed(admin_msg(
                    "create",
                    &format!("/b/llm/api/providers/{id}/discover-models"),
                )),
            )
            .await,
        )
        .await;
        assert_eq!(
            discovered,
            serde_json::json!({ "models": ["gpt-4o", "gpt-4o-mini"] })
        );

        let deleted = output_json(
            delete_provider(
                &block,
                &ctx,
                &routed(admin_msg("delete", &format!("/b/llm/api/providers/{id}"))),
            )
            .await,
        )
        .await;
        assert_eq!(deleted, serde_json::json!({ "deleted": true }));
    }

    // -----------------------------------------------------------------
    // reload_provider_service — key_var resolution
    // -----------------------------------------------------------------

    /// End-to-end reload over a real in-memory DB + config block:
    /// a row whose `key_var` resolves gets its `api_key` populated, a row
    /// without `key_var` stays unauthenticated, and an unresolvable
    /// `key_var` degrades to no key (warn) instead of failing the reload.
    ///
    /// Needs `feature = "llm"` for the concrete `ProviderLlmService` whose
    /// snapshot the assertions read; a build without it has no router to
    /// reload into, which `the_no_op_handle_refuses_configure_and_says_so`
    /// covers instead.
    #[cfg(feature = "llm")]
    #[tokio::test]
    async fn reload_provider_service_resolves_key_var_into_api_key() {
        use wafer_core::{
            interfaces::config::service::ConfigService,
            service_blocks::config::{ConfigBlock, EnvConfigService},
        };
        /// A key variable nothing sets.
        const MISSING_KEY_VAR: &str = "IMPRESSPRESS__LLM__TEST_MISSING_KEY";

        let mut ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::llm::LlmBlock::BLOCK_NAME);
        {
            use crate::blocks::llm::migrations;
            let sqlite: Vec<&str> = migrations::SQLITE_MIGRATIONS
                .iter()
                .map(|(_, sql)| *sql)
                .collect();
            crate::migration_helper::apply_migrations(
                &ctx,
                "impresspress/llm",
                &sqlite,
                migrations::POSTGRES_MIGRATIONS,
            )
            .await
            .expect("apply llm migrations");
        }

        let config_svc = Arc::new(EnvConfigService::new());
        config_svc.set(EXAMPLE_KEY_VAR, "sk-resolved");
        ctx.register_block("wafer-run/config", Arc::new(ConfigBlock::new(config_svc)));

        for cfg in [
            ProviderConfig::new(
                "with-key-var",
                ProviderProtocol::OpenAi,
                "https://api.openai.com/v1",
            )
            .with_key_var(EXAMPLE_KEY_VAR),
            ProviderConfig::new(
                "no-key-var",
                ProviderProtocol::OpenAiCompatible,
                "http://localhost:11434/v1",
            ),
            ProviderConfig::new(
                "unresolvable-key-var",
                ProviderProtocol::OpenAi,
                "https://api.openai.com/v1",
            )
            .with_key_var(MISSING_KEY_VAR),
        ] {
            let mut data = config_to_row(&cfg);
            crate::util::stamp_created(&mut data);
            db::create(&ctx, PROVIDERS_TABLE, data)
                .await
                .expect("create provider row");
        }

        let svc = crate::blocks::llm::providers::ProviderLlmService::try_new()
            .expect("build provider service");
        reload_provider_service(&ctx, &svc)
            .await
            .expect("reload succeeds");

        let by_name = |name: &str| {
            svc.providers_snapshot()
                .into_iter()
                .find(|c| c.name == name)
                .unwrap_or_else(|| panic!("provider '{name}' missing from snapshot"))
        };
        assert_eq!(
            by_name("with-key-var").api_key.as_deref(),
            Some("sk-resolved"),
            "key_var must resolve into api_key at reload"
        );
        assert_eq!(by_name("no-key-var").api_key, None);
        assert_eq!(
            by_name("unresolvable-key-var").api_key,
            None,
            "unresolvable key_var degrades to no key, not a reload failure"
        );
    }
}

// ---------------------------------------------------------------------------
// Tests: provider management over a router that cannot be configured.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod inert_router_tests {
    use std::sync::Arc;

    use wafer_run::{streams::output::TerminalNotResponse, ErrorCode};

    use super::*;
    use crate::{
        blocks::llm::{
            provider_admin::NoopProviderAdmin,
            providers::config::ProviderProtocol,
            routes::test_support::{admin_msg, routed},
            LlmBlock,
        },
        test_support::{output_json, TestContext},
    };

    fn json_input(value: serde_json::Value) -> InputStream {
        InputStream::from_bytes(serde_json::to_vec(&value).expect("serialize body"))
    }

    fn create_body() -> InputStream {
        json_input(serde_json::json!({
            "name": "openai-main",
            "protocol": "open_ai",
            "endpoint": "https://api.openai.com/v1",
        }))
    }

    /// A block holding the no-op provider admin — every wasm32 build, and
    /// `test_support::real_block_infos()`.
    fn inert_block() -> LlmBlock {
        LlmBlock::new(Arc::new(NoopProviderAdmin))
    }

    /// The rows the providers table currently holds.
    async fn provider_rows(ctx: &dyn Context) -> Vec<wafer_core::clients::database::Record> {
        db_read::list_every(ctx, PROVIDERS_TABLE, vec![])
            .await
            .expect("list providers")
    }

    /// Creating a provider on a runtime whose router cannot be configured
    /// answered a green 200 and left a row behind that nothing would ever
    /// load: `NoopProviderAdmin::configure` was infallible and inert, so the
    /// reload after the write reported success. It now refuses — and refuses
    /// *before* the write, because a 501 that still persisted the row would
    /// be the same lie in a different status code.
    #[tokio::test]
    async fn create_on_an_inert_router_refuses_and_writes_no_row() {
        let ctx = TestContext::with_llm().await;
        let block = inert_block();

        let out = create_provider(
            &block,
            &ctx,
            &admin_msg("create", "/b/llm/api/providers"),
            create_body(),
        )
        .await;

        match out.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::Unimplemented, "got {e:?}")
            }
            other => panic!("expected 501, got {other:?}"),
        }
        assert!(
            provider_rows(&ctx).await.is_empty(),
            "a refused create must not leave an orphan provider row"
        );
    }

    /// Same for update and delete: both used to mutate the row and then
    /// report success over a router that never saw the change.
    #[tokio::test]
    async fn update_and_delete_on_an_inert_router_refuse_and_change_nothing() {
        let ctx = TestContext::with_llm().await;
        // Seeded through `config_to_row`, the same encoder the create
        // handler writes with, so the row the refused update and delete are
        // asked to change is one the block itself could have stored.
        let seeded = db::create(
            &ctx,
            PROVIDERS_TABLE,
            config_to_row(&ProviderConfig::new(
                "openai-main",
                ProviderProtocol::OpenAi,
                "https://api.openai.com/v1",
            )),
        )
        .await
        .expect("seed provider row");
        let block = inert_block();

        let updated = update_provider(
            &block,
            &ctx,
            &routed(admin_msg(
                "update",
                &format!("/b/llm/api/providers/{}", seeded.id),
            )),
            json_input(serde_json::json!({ "name": "renamed" })),
        )
        .await;
        match updated.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::Unimplemented, "update: got {e:?}")
            }
            other => panic!("update: expected 501, got {other:?}"),
        }

        let deleted = delete_provider(
            &block,
            &ctx,
            &routed(admin_msg(
                "delete",
                &format!("/b/llm/api/providers/{}", seeded.id),
            )),
        )
        .await;
        match deleted.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::Unimplemented, "delete: got {e:?}")
            }
            other => panic!("delete: expected 501, got {other:?}"),
        }

        let rows = provider_rows(&ctx).await;
        assert_eq!(rows.len(), 1, "the refused delete must not remove the row");
        assert_eq!(
            rows[0].data.get("name").and_then(|v| v.as_str()),
            Some("openai-main"),
            "the refused update must not rename the row"
        );
    }

    /// `discover-models` writes the discovered list back to the row, so it
    /// is provider management and refuses on the same terms as the other
    /// three — before it looks the row up, which is why a runtime that
    /// cannot discover says so instead of answering 404 for a row it never
    /// asked about.
    ///
    /// What a *capable* router's own `LlmError` renders as is pinned
    /// separately, in `discovery_error_shape_tests`.
    #[tokio::test]
    async fn discover_models_on_an_inert_router_reports_not_supported() {
        let ctx = TestContext::with_llm().await;
        let block = inert_block();

        let out = discover_models(
            &block,
            &ctx,
            &routed(admin_msg(
                "create",
                "/b/llm/api/providers/row-1/discover-models",
            )),
        )
        .await;

        match out.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::Unimplemented, "got {e:?}");
                assert!(
                    !e.message.contains("NotSupported"),
                    "the message is a Display render, not a Debug one: {}",
                    e.message
                );
            }
            other => panic!("expected 501, got {other:?}"),
        }
    }

    /// Reading the stored rows is not provider *management* — the rows are
    /// real configuration and an admin must still be able to see them.
    #[tokio::test]
    async fn listing_providers_still_works_on_an_inert_router() {
        let ctx = TestContext::with_llm().await;
        let block = inert_block();

        let body = output_json(
            list_providers(&block, &ctx, &admin_msg("retrieve", "/b/llm/api/providers")).await,
        )
        .await;

        assert_eq!(body, serde_json::json!({ "providers": [] }));
    }

    /// The two answers must not drift: a handle that says it cannot manage
    /// providers must refuse `configure`. The gate the handlers above use and
    /// the reload's own failure would otherwise be two independent opinions
    /// about what this runtime supports.
    #[test]
    fn the_no_op_handle_refuses_configure_and_says_so() {
        let noop = NoopProviderAdmin;
        assert!(!noop.manages_providers());
        assert!(noop.configure(Vec::new()).is_err());
    }

    /// And the real provider router says it can, and accepts.
    #[cfg(feature = "llm")]
    #[test]
    fn the_real_router_accepts_configure_and_says_so() {
        let svc = crate::blocks::llm::providers::ProviderLlmService::try_new()
            .expect("build provider service");
        assert!(svc.manages_providers());
        assert!(svc.configure(Vec::new()).is_ok());
    }
}

// ---------------------------------------------------------------------------
// Tests: what a provider's own failure looks like on the wire.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod discovery_error_shape_tests {
    use std::sync::Arc;

    use wafer_run::{streams::output::TerminalNotResponse, ErrorCode};

    use super::*;
    use crate::{
        blocks::llm::{
            providers::config::ProviderProtocol,
            routes::test_support::{admin_msg, routed},
            LlmBlock,
        },
        test_support::TestContext,
    };

    /// A router that manages providers and whose discovery fails with a
    /// scripted [`LlmError`]. `RecordingProviderAdmin` always succeeds, so
    /// nothing in the tree could reach `discover_models`'s error arm before.
    struct FailingDiscovery(fn() -> LlmError);

    #[async_trait::async_trait]
    impl ProviderAdmin for FailingDiscovery {
        fn manages_providers(&self) -> bool {
            true
        }
        fn configure(&self, _providers: Vec<ProviderConfig>) -> Result<(), LlmError> {
            Ok(())
        }
        fn providers_snapshot(&self) -> Vec<ProviderConfig> {
            Vec::new()
        }
        async fn discover_models(
            &self,
            _provider_name: &str,
        ) -> Result<Vec<wafer_core::interfaces::llm::service::ModelInfo>, LlmError> {
            Err((self.0)())
        }
    }

    /// Seed one provider row and run discovery against a router whose
    /// discovery fails with `error`.
    async fn discover_against(error: fn() -> LlmError) -> WaferError {
        let ctx = TestContext::with_llm().await;
        let seeded = db::create(
            &ctx,
            PROVIDERS_TABLE,
            config_to_row(&ProviderConfig::new(
                "openai-main",
                ProviderProtocol::OpenAi,
                "https://api.openai.com/v1",
            )),
        )
        .await
        .expect("seed provider row");
        let block = LlmBlock::new(Arc::new(FailingDiscovery(error)));

        let out = discover_models(
            &block,
            &ctx,
            &routed(admin_msg(
                "create",
                &format!("/b/llm/api/providers/{}/discover-models", seeded.id),
            )),
        )
        .await;

        match out.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => e,
            other => panic!("expected an error terminal, got {other:?}"),
        }
    }

    /// The admin typed a provider name the router does not know: that is the
    /// admin's own input, a 400, and the message says which name.
    ///
    /// Every one of these used to be `err_internal("discover_models failed",
    /// format!("{e:?}"))` — a 500 whose body was the sanitized
    /// `"Internal server error (ref: …)"` and whose *log line* carried a Rust
    /// enum spelling (`InvalidRequest("unknown provider: openai-main")`)
    /// rather than the error's own sentence.
    #[tokio::test]
    async fn an_unknown_provider_is_the_callers_mistake_not_an_internal_error() {
        let e = discover_against(|| LlmError::InvalidRequest("unknown provider: x".into())).await;

        assert_eq!(e.code, ErrorCode::InvalidArgument, "got {e:?}");
        assert_eq!(e.message, "invalid request: unknown provider: x");
    }

    /// A protocol with no discovery endpoint (Anthropic) is 501, and it does
    /// not read as a bug in this deployment.
    #[tokio::test]
    async fn a_protocol_without_discovery_is_not_supported() {
        let e = discover_against(|| LlmError::NotSupported).await;

        assert_eq!(e.code, ErrorCode::Unimplemented, "got {e:?}");
        assert_eq!(e.message, "not supported by this backend");
    }

    /// The provider rejected the configured credential. That is 401 and the
    /// admin can act on it; collapsing it into a 500 told them nothing.
    #[tokio::test]
    async fn a_rejected_credential_is_reported_as_such() {
        let e = discover_against(|| LlmError::Unauthorized).await;

        assert_eq!(e.code, ErrorCode::Unauthenticated, "got {e:?}");
        assert_eq!(e.message, "unauthorized");
    }

    /// A transport failure keeps the sanitized 500: the text is the
    /// provider's own and names deployment topology, so it is logged with a
    /// correlation id and never echoed.
    #[tokio::test]
    async fn a_transport_failure_stays_a_sanitized_internal_error() {
        let e =
            discover_against(|| LlmError::Network("dns failure for internal.corp".into())).await;

        assert_eq!(e.code, ErrorCode::Internal, "got {e:?}");
        assert!(
            e.message.starts_with("Internal server error (ref: "),
            "the provider's own text must not reach the client: {}",
            e.message
        );
        assert!(!e.message.contains("internal.corp"), "{}", e.message);
    }
}

// ---------------------------------------------------------------------------
// Tests: the add-provider form's own bytes.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod form_body_tests {
    //! The add-provider form on `/b/llm/providers` is a plain htmx `hx-post`,
    //! so every submit arrives as `application/x-www-form-urlencoded` with
    //! every field a string. These tests post those bytes — percent-encoded,
    //! `+` for space — rather than a Rust struct serialized to JSON, because
    //! the difference between the two is the whole bug.

    use std::sync::Arc;

    use wafer_run::{streams::output::TerminalNotResponse, ErrorCode};

    use super::*;
    use crate::{
        blocks::llm::{routes::test_support::admin_msg, LlmBlock, EXAMPLE_KEY_VAR},
        test_support::{output_json, TestContext},
    };

    /// The block whose provider-admin handle manages providers, over a
    /// context with the llm migrations applied.
    async fn fixture() -> (TestContext, LlmBlock) {
        let ctx = TestContext::with_llm().await;
        let block = LlmBlock::new(Arc::new(
            crate::blocks::llm::routes::test_support::RecordingProviderAdmin::default(),
        ));
        (ctx, block)
    }

    /// The exact body htmx builds from the rendered form when the admin fills
    /// in every field, leaves the "Enabled" box ticked and leaves the budget
    /// field on "Follow the protocol". `enabled=true` is the checkbox's own
    /// `value` attribute; `max_tokens_field=` is what a select whose chosen
    /// option has an empty value posts — a select always sends something; the
    /// endpoint is percent-encoded as a browser encodes it.
    const TICKED_FORM: &str = "name=openai-main&protocol=open_ai\
&endpoint=https%3A%2F%2Fapi.openai.com%2Fv1\
&key_var=IMPRESSPRESS__LLM__OPENAI_KEY\
&max_tokens_field=\
&models=gpt-4o%2C+gpt-4o-mini\
&enabled=true";

    async fn create_from_form(body: &str) -> OutputStream {
        let (ctx, block) = fixture().await;
        create_provider(
            &block,
            &ctx,
            &admin_msg("create", "/b/llm/api/providers"),
            InputStream::from_bytes(body.as_bytes().to_vec()),
        )
        .await
    }

    async fn refusal(out: OutputStream) -> (ErrorCode, String) {
        match out.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => (e.code, e.message),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_form_submit_creates_the_provider() {
        let created = output_json(create_from_form(TICKED_FORM).await).await;

        assert_eq!(created["name"], "openai-main");
        assert_eq!(created["protocol"], "open_ai");
        assert_eq!(
            created["endpoint"], "https://api.openai.com/v1",
            "the endpoint must be form-decoded"
        );
        assert_eq!(created["key_var"], EXAMPLE_KEY_VAR);
        assert_eq!(
            created["models"],
            serde_json::json!(["gpt-4o", "gpt-4o-mini"]),
            "the one comma-separated text input becomes the contract's array"
        );
        assert_eq!(created["enabled"], true);
    }

    /// An unticked checkbox posts nothing at all, which is the only way the
    /// form can say "disabled" — reading an absent `enabled` as the
    /// contract's `true` default would ignore the admin.
    #[tokio::test]
    async fn an_unticked_enabled_box_disables_the_provider() {
        let body = TICKED_FORM
            .strip_suffix("&enabled=true")
            .expect("the ticked body ends with the checkbox");
        let created = output_json(create_from_form(body).await).await;
        assert_eq!(created["enabled"], false);
    }

    /// The models input is optional: the form hint tells the admin to leave
    /// it empty and use "Discover models" instead, and an empty text input
    /// still posts its (empty) value.
    #[tokio::test]
    async fn an_empty_models_input_is_no_models() {
        let body = TICKED_FORM.replace("models=gpt-4o%2C+gpt-4o-mini", "models=");
        let created = output_json(create_from_form(&body).await).await;
        assert_eq!(created["models"], serde_json::json!([]));
    }

    /// A list spelled as a repeated key is the other way a form can carry one,
    /// and it is what urlencoded serialisation writes for an array. The
    /// last-wins map behind `parse_form_body` would have kept `gpt-4o-mini`
    /// alone — the same silent single-model failure that ruled out keeping the
    /// browser-side hook.
    #[tokio::test]
    async fn a_repeated_models_key_keeps_every_model() {
        let body = TICKED_FORM.replace(
            "models=gpt-4o%2C+gpt-4o-mini",
            "models=gpt-4o&models=gpt-4o-mini",
        );
        let created = output_json(create_from_form(&body).await).await;
        assert_eq!(
            created["models"],
            serde_json::json!(["gpt-4o", "gpt-4o-mini"])
        );
    }

    /// The budget-field select's "Follow the protocol" option has no token to
    /// post, so it posts the empty string — which is not one of the two wire
    /// spellings. Handing it to serde as-is would 400 every ordinary submit,
    /// so the parser drops it and the contract sees the field omitted.
    #[tokio::test]
    async fn an_empty_budget_field_select_leaves_the_provider_on_its_protocol() {
        let created = output_json(create_from_form(TICKED_FORM).await).await;
        assert_eq!(created["max_tokens_field"], serde_json::Value::Null);
    }

    /// And a chosen option arrives as the override — the Azure reasoning
    /// deployment, configured the way the form configures it.
    #[tokio::test]
    async fn a_chosen_budget_field_reaches_the_stored_provider() {
        let body = TICKED_FORM
            .replace("protocol=open_ai&", "protocol=open_ai_compatible&")
            .replace(
                "max_tokens_field=",
                "max_tokens_field=max_completion_tokens",
            );
        let created = output_json(create_from_form(&body).await).await;
        assert_eq!(created["protocol"], "open_ai_compatible");
        assert_eq!(created["max_tokens_field"], "max_completion_tokens");
    }

    /// A token the enum does not have is refused rather than stored, the same
    /// way a wrong `protocol` is — the select's options are the only two.
    #[tokio::test]
    async fn a_form_body_with_an_unknown_budget_field_is_refused() {
        let body = TICKED_FORM.replace("max_tokens_field=", "max_tokens_field=maxTokens");
        let (code, message) = refusal(create_from_form(&body).await).await;
        assert_eq!(code, ErrorCode::InvalidArgument);
        assert!(
            message.contains("maxTokens")
                && message.contains("max_completion_tokens")
                && message.contains("max_tokens"),
            "the refusal must name the rejected token and both accepted ones, \
             got: {message}"
        );
    }

    /// `deny_unknown_fields` is on `CreateProviderRequest` so an inline
    /// `api_key` is refused by name rather than silently dropped. The form
    /// path builds the same typed body, so it inherits that refusal — a form
    /// path that assembled the struct field by field would not.
    #[tokio::test]
    async fn a_form_body_cannot_smuggle_an_inline_api_key() {
        let body = format!("{TICKED_FORM}&api_key=sk-live-should-be-refused");
        let (code, message) = refusal(create_from_form(&body).await).await;
        assert_eq!(code, ErrorCode::InvalidArgument);
        assert!(
            message.contains("api_key"),
            "the refusal must name the field, got: {message}"
        );
    }

    /// The enum still types `protocol` on the form path, so a wrong token is
    /// refused with the list of accepted ones rather than stored.
    #[tokio::test]
    async fn a_form_body_with_an_unknown_protocol_is_refused() {
        let body = TICKED_FORM.replace("protocol=open_ai", "protocol=openai");
        let (code, message) = refusal(create_from_form(&body).await).await;
        assert_eq!(code, ErrorCode::InvalidArgument);
        assert!(
            message.contains("open_ai_compatible"),
            "the refusal must name the accepted values, got: {message}"
        );
    }

    /// SSRF validation is on the value, not on the encoding: the same gate
    /// the JSON path passes through runs for a form-encoded endpoint.
    #[tokio::test]
    async fn a_form_body_cannot_point_at_internal_infrastructure() {
        let body = TICKED_FORM.replace(
            "endpoint=https%3A%2F%2Fapi.openai.com%2Fv1",
            "endpoint=https%3A%2F%2F169.254.169.254%2Flatest",
        );
        let (code, message) = refusal(create_from_form(&body).await).await;
        assert_eq!(code, ErrorCode::InvalidArgument);
        assert!(
            message.contains("endpoint"),
            "the refusal must name the field, got: {message}"
        );
    }
}
