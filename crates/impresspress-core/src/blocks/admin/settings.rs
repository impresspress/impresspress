use std::collections::{BTreeMap, HashMap};

use wafer_core::clients::database as db;
use wafer_run::{context::Context, ConfigVar, InputStream, Message, OutputStream};

use super::{
    contracts::{AdminSettingView, AdminSettingsResponse},
    ops::{self, MASKED_VALUE},
};
use crate::{
    blocks::crud,
    http::{err_bad_request, ok_json, require_row},
    platform_state::{
        block_settings::{self, BlockSettingsPatch},
        variables::{self, NewVariable, VariablePatch},
    },
};

/// `GET /b/admin/api/settings/all`.
pub(super) async fn handle_list_full(ctx: &dyn Context) -> OutputStream {
    match variables::list_all(ctx).await {
        Ok(rows) => {
            let vars: Vec<_> = rows
                .iter()
                .map(|row| {
                    let is_sensitive = ops::is_sensitive_key(&row.key, i64::from(row.sensitive));
                    let is_system = row.key.starts_with("WAFER_RUN_SHARED__");
                    // Mask sensitive values even in the "full" listing
                    let value = if is_sensitive {
                        MASKED_VALUE.to_string()
                    } else {
                        row.value.clone()
                    };
                    serde_json::json!({
                        "key": row.key,
                        "name": row.name,
                        "description": row.description,
                        "value": value,
                        "warning": row.warning,
                        "sensitive": is_sensitive,
                        "system": is_system,
                        "updated_at": row.updated_at,
                    })
                })
                .collect();
            ok_json(&vars)
        }
        Err(e) => crud::db_error_internal(e, "Database error"),
    }
}

/// `GET /b/admin/api/settings`.
pub(super) async fn handle_list(ctx: &dyn Context) -> OutputStream {
    match variables::list_all(ctx).await {
        Ok(rows) => {
            // Collected into a `BTreeMap` first, then flattened: the
            // response is a public contract, and a randomized key order made
            // two identical reads differ byte for byte.
            let mut by_key = BTreeMap::new();
            for row in &rows {
                let sensitive = ops::is_sensitive_key(&row.key, i64::from(row.sensitive));
                let value = if sensitive {
                    MASKED_VALUE.to_string()
                } else {
                    row.value.clone()
                };
                by_key.insert(
                    row.key.clone(),
                    AdminSettingView {
                        key: row.key.clone(),
                        value: serde_json::Value::String(value),
                        sensitive,
                    },
                );
            }
            ok_json(&AdminSettingsResponse {
                settings: by_key.into_values().collect(),
            })
        }
        Err(e) => crud::db_error_internal(e, "Database error"),
    }
}

/// `GET /b/admin/api/settings/{key}`. `{key}` is read only as the route
/// table bound it.
pub(super) async fn handle_get(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let key = match crud::path_var(msg, "key", "Missing setting key") {
        Ok(value) => value,
        Err(response) => return response,
    };

    let mut row = match variables::get_by_key(ctx, key)
        .await
        .map_err(|e| crud::db_error(e, "Setting not found", "Database error"))
        .and_then(|row| require_row(row, "Setting not found"))
    {
        Ok(row) => row,
        Err(response) => return response,
    };
    // SEC-060: mask on the row flag OR what the KEY says — the `_SECRET` /
    // `_KEY` suffix, or a declaration that calls the var a password. The
    // single-key getter masked on the flag alone once, so a `*_SECRET` key with
    // the flag unset leaked its value here; it then masked on flag-or-suffix,
    // so an unrepaired `WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_PASSWORD` row
    // leaked instead.
    if ops::is_sensitive_key(key, i64::from(row.sensitive)) {
        row.value = MASKED_VALUE.to_string();
    }
    // The row is echoed in the `{id, data}` record envelope this endpoint has
    // always published; it is declared without a schema until it is typed.
    ok_json(&db::Record {
        id: row.id.clone(),
        data: row.to_data(),
    })
}

/// `PATCH /b/admin/api/settings/{key}`. `{key}` is read only as the route
/// table bound it.
///
/// A real partial update: every field of the body is optional and an absent
/// one leaves its column alone. `value` in particular, because a sensitive key
/// reads back as `MASKED_VALUE` and [`ops::update_variable`] refuses to store
/// that mask — leaving it out is how a caller says "keep the secret I cannot
/// see", and the echoed record then carries no `value` field at all. When a
/// value WAS supplied, the echo masks it the same way [`handle_get`] does.
pub(super) async fn handle_set(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    let key = match crud::path_var(msg, "key", "Missing setting key") {
        Ok(value) => value,
        Err(response) => return response,
    };

    #[derive(serde::Deserialize)]
    struct Req {
        /// Optional: absent leaves the stored value alone.
        ///
        /// It was required, which made this a PATCH that could not patch. A
        /// sensitive key reads back as `MASKED_VALUE` and `ops::update_variable`
        /// refuses to store that mask, so with `value` required there was no
        /// request at all that changed only the `sensitive` flag of a key whose
        /// value the caller cannot see. Absent is how a caller says "not this
        /// field", and it is the remedy the mask refusal names.
        #[serde(default)]
        value: Option<serde_json::Value>,
        /// Optional: absent leaves the stored masking flag alone.
        #[serde(default)]
        sensitive: Option<bool>,
    }
    let raw = match input.collect_to_bytes().await {
        Ok(bytes) => bytes,
        Err(e) => return OutputStream::error(e),
    };
    let body: Req = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };

    // Both fields optional means a body carrying neither now parses, and
    // `update_variable` upserts — so `PATCH {}` against an unstored key would
    // create a blank row, and against a stored one would write an audit entry
    // for a change nobody made. A request with nothing to change is a malformed
    // request, not a no-op, and saying so keeps the field-is-absent spelling
    // meaning exactly one thing.
    if body.value.is_none() && body.sensitive.is_none() {
        return err_bad_request(
            "Nothing to update: send `value`, `sensitive`, or both. Leaving `value` out \
             keeps the stored value.",
        );
    }

    // The `value` column is TEXT; a string value is stored verbatim, anything
    // else as its JSON form (the prior validation already read it via
    // `as_str().unwrap_or("")`, so non-string values were treated as empty).
    let value = body.value.as_ref().map(|value| match value {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    });

    // Guards (sensitive-empty + URL/SSRF), audit-log write, and upsert live in
    // the shared ops layer so the SSR variable surface can't diverge.
    match ops::update_variable(
        ctx,
        msg,
        key,
        ops::VariableUpdate {
            value: value.as_deref(),
            description: None,
            sensitive: body.sensitive,
        },
    )
    .await
    {
        // Echoed in the `{id, data}` record envelope this endpoint has
        // always published; declared without a schema until it is typed.
        //
        // The stored value is never handed back to a request that did not send
        // it: the echo is the row AS STORED, so without this a
        // `PATCH {"sensitive": true}` would answer with the plaintext secret,
        // turning the writer into the reader the masking exists to prevent.
        //
        // Two different situations, and they get two different answers, because
        // masking is the wrong way to spell "this field was not returned".
        //
        // * The request supplied no value: the field is ABSENT from the record.
        //   Substituting the mask here invented a value for rows nothing masks
        //   — and a client replaying what it had just read then stored
        //   `"********"` as a plain setting's value, with
        //   `is_masked_submission` rightly declining to stop it, since for such
        //   a row that string is ordinary. That is this endpoint's own hazard,
        //   re-opened from the other side. Absent is what the request said and
        //   what a JSON object has for "no value".
        // * The request supplied a value for a key something masks: the mask,
        //   as `handle_get` answers. Read off the POST-WRITE flag, which is
        //   right here precisely because the value came in with the request —
        //   the caller already knows it.
        Ok(row) => {
            let mut data = row.to_data();
            if value.is_none() {
                data.remove("value");
            } else if ops::is_sensitive_key(&row.key, i64::from(row.sensitive)) {
                data.insert("value".to_string(), serde_json::json!(MASKED_VALUE));
            }
            ok_json(&db::Record { id: row.id, data })
        }
        Err(out) => out,
    }
}

/// `POST /b/admin/api/settings`.
pub(super) async fn handle_create(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    #[derive(serde::Deserialize)]
    struct Req {
        key: String,
        value: Option<String>,
        name: Option<String>,
        description: Option<String>,
        sensitive: Option<bool>,
    }
    let raw = match input.collect_to_bytes().await {
        Ok(bytes) => bytes,
        Err(e) => return OutputStream::error(e),
    };
    let body: Req = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };
    // Key-empty guard, URL/SSRF validation, audit-log write, and the create
    // live in the shared ops layer so the SSR variable surface can't diverge.
    match ops::create_variable(
        ctx,
        msg,
        &body.key,
        body.value.as_deref().unwrap_or(""),
        body.name.as_deref(),
        body.description.as_deref(),
        // Absent means sensitive. A caller that does not say is protected;
        // one that wants a plain-text variable says `false`. The old `false`
        // default published any key without a `_SECRET`/`_KEY` suffix in
        // plain text unless the operator remembered the box.
        body.sensitive.unwrap_or(true),
    )
    .await
    {
        // Echoed in the `{id, data}` record envelope this endpoint has
        // always published; declared without a schema until it is typed.
        Ok(row) => ok_json(&db::Record {
            id: row.id.clone(),
            data: row.to_data(),
        }),
        Err(out) => out,
    }
}

/// `DELETE /b/admin/api/settings/{key}`. `{key}` is read only as the route
/// table bound it.
pub(super) async fn handle_delete(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let key = match crud::path_var(msg, "key", "Missing setting key") {
        Ok(value) => value,
        Err(response) => return response,
    };

    // The shared-key guard, the delete and the audit row all live in `ops`,
    // shared with the Variables page's row control — so the two surfaces
    // refuse the same keys and leave the same trail. This path wrote no audit
    // row at all before: create and update were audited, delete was not.
    if let Err(response) = ops::delete_variable(ctx, msg, key).await {
        return response;
    }
    ok_json(&serde_json::json!({"deleted": key}))
}

/// `POST /b/admin/api/settings/{key}/reset-to-environment`.
///
/// Releases the row's pin so the next boot seeds the key from the process
/// environment again. The supported way out of a pinned key — see
/// [`ops::reset_variable_to_environment`] for why neither delete nor an empty
/// update is that way.
pub(super) async fn handle_reset_to_environment(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let key = match crud::path_var(msg, "key", "Missing setting key") {
        Ok(value) => value,
        Err(response) => return response,
    };
    if let Err(response) = ops::reset_variable_to_environment(ctx, msg, key).await {
        return response;
    }
    ok_json(&serde_json::json!({"reset_to_environment": key}))
}

/// Full block name of the admin block — the `block_settings` row whose
/// `seed_defaults_hash` column gates this function.
const ADMIN_BLOCK_NAME: &str = "impresspress/admin";

/// Compute a deterministic SHA-256 hex digest over the declared shared
/// config vars. Anything that affects the seed outcome (key, name,
/// description, default, warning, sensitive flag) feeds the hash; sort by
/// key so map ordering can't make two equivalent inputs hash differently.
///
/// `var.optional` does not affect what `seed_defaults` writes — it is consumed
/// by the startup validator — so it is intentionally omitted.
///
/// `var.auto_generate` IS folded in, through the `sensitive` term:
/// `config_vars::is_sensitive_var` unions it (an auto-generated secret is a
/// secret, whatever its `input_type` says), so flipping `auto_generate` on a
/// declared var moves this hash. That is deliberate — it changes the flag a
/// fresh row is created with — and it is why the term is `is_sensitive_var(v)`
/// rather than `input_type == Password`.
fn seed_payload_hash(vars: &[ConfigVar]) -> String {
    use std::fmt::Write as _;
    let mut keys: Vec<&ConfigVar> = vars.iter().collect();
    keys.sort_by(|a, b| a.key.cmp(&b.key));
    let mut buf = String::with_capacity(vars.len() * 128);
    for v in keys {
        // The same rule the seed loop below writes with, so the hash and the
        // write cannot disagree about what a var's flag should be — and so a
        // build that changes a var's sensitivity invalidates the gate and
        // re-seeds instead of leaving the stored flag stale. Read from the var
        // in hand (`is_sensitive_var`), not looked up by key: this hash exists
        // to notice a DECLARATION change, including a var that just became a
        // password.
        let sensitive = i32::from(crate::config_vars::is_sensitive_var(v));
        // Fixed shape per var: `key\x1fname\x1fdescription\x1fdefault\x1fwarning\x1fsensitive\x1e`.
        // ASCII unit-separator (0x1f) + record-separator (0x1e) bracket
        // each field so embedded newlines / colons in description text
        // can't collide field boundaries across different var shapes.
        let _ = write!(
            &mut buf,
            "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1e}",
            v.key, v.name, v.description, v.default, v.warning, sensitive,
        );
    }
    crate::migration_helper::sha256_hex_bytes(buf.as_bytes())
}

pub async fn seed_defaults(ctx: &dyn Context) {
    let vars = crate::config_vars::shared_config_vars();

    // Hash-gate: if the cached `block_settings.seed_defaults_hash` row for
    // the admin block already matches the current declared-vars hash, every
    // shared var was seeded against the same metadata last time — there is
    // no outcome change possible and we can skip the entire seed (zero D1
    // queries). Mirrors `migration_helper::apply_if_blessed`'s gate; reads
    // the same in-memory snapshot the migration helper does, so warm cold
    // starts cost zero round-trips. See 2026-05-14 config-snapshot spec
    // § "Hash-gate seed_defaults like migrations" (PR 3).
    let code_hash = seed_payload_hash(&vars);
    let json = ctx
        .config_get(crate::features::BLOCK_SETTINGS_CONFIG_KEY)
        .unwrap_or("{}");
    let cached_hash =
        crate::features::BlockSettings::state_for(json, ADMIN_BLOCK_NAME).seed_defaults_hash;
    if cached_hash == code_hash && !code_hash.is_empty() {
        return;
    }

    // Single bulk fetch of every existing variable, then in-memory diff
    // per declared shared var, instead of a `get_by_field` per var — two D1
    // queries per shared var per cold isolate otherwise. Without this read
    // there is nothing to diff against: inserting every key blind would
    // collide with each row it could not see. So a failed read ends the run
    // here, unstamped, and the next boot tries again.
    let existing: HashMap<String, _> = match variables::list_all(ctx).await {
        Ok(rows) => rows.into_iter().map(|row| (row.key.clone(), row)).collect(),
        Err(e) => {
            tracing::warn!(
                err = %e,
                "seed_defaults: could not read the variables table; nothing seeded, and \
                 the seed hash is left unstamped so the next boot runs it again"
            );
            return;
        }
    };

    // Keys whose write failed. The hash gate below may only be stamped when
    // this stays empty: the gate skips the whole function on every later
    // boot, so stamping over a failed write would leave that row un-seeded,
    // its metadata stale, or its dead asset URL unrepaired, until a release
    // happens to change a declaration.
    let mut failed: Vec<&str> = Vec::new();

    for var in &vars {
        // What the DECLARATION requires. Not what gets written unconditionally:
        // see the existing-row branch, which may only raise.
        let sensitive = crate::config_vars::is_sensitive_var(var);
        let name = if var.name.is_empty() {
            &var.key
        } else {
            &var.name
        };

        match existing.get(&var.key) {
            Some(row) => {
                // A stored value pointing at our own `/b/static/` route for a
                // file this build does not serve is a stale pointer *this
                // function wrote*: built-in asset URLs carry a content hash
                // and are seeded into the database as defaults, so any release
                // that changes the artwork — new logo, new favicon, or the
                // removed raster wordmark whose route is gone outright —
                // leaves every existing deployment pointing at a 404. Left
                // alone it renders a broken image on every page that shows
                // the brand.
                //
                // Repaired here rather than in an admin migration because
                // migrations are gated (`--run-migrations`, see RELEASE.md)
                // and a broken logo gives an operator no signal to opt in,
                // whereas `seed_defaults` runs on every boot's `Init`. It
                // costs one manifest scan per stored value per boot and is
                // idempotent: once repaired the value names a served file, so
                // it no longer matches. Scoped to the built-in route by
                // `is_stale_builtin_asset_url`, so a white-labelled URL is
                // never touched.
                let stale_builtin_asset = crate::ui::assets::is_stale_builtin_asset_url(&row.value);

                // Only refresh metadata when at least one declared field
                // actually differs. Without this guard every isolate cold-start
                // re-writes every shared config var (~80 vars × cold-starts/day
                // ≈ ~900 useless UPDATEs/day in prod).
                let same_name = row.name == *name;
                let same_desc = row.description == var.description;
                let same_warn = row.warning == var.warning;
                // `sensitive` is deliberately NOT patched here, in either
                // direction. This branch is behind the declared-vars hash gate
                // above, which every settled deployment passes — so anything
                // written here waits for a release that changes a declaration,
                // and a security flag cannot be on that schedule.
                // `platform_state::variables::repair_sensitive_flags` owns the
                // reconciliation instead: same rule, un-gated, on all three
                // targets. This loop keeps only the descriptive metadata, which
                // is exactly what the gate is appropriate for.
                if same_name && same_desc && same_warn && !stale_builtin_asset {
                    continue;
                }
                let mut patch = VariablePatch {
                    name: Some(name.clone()),
                    description: Some(var.description.clone()),
                    warning: Some(var.warning.clone()),
                    ..Default::default()
                };
                if stale_builtin_asset {
                    tracing::warn!(
                        key = %var.key,
                        stale = %row.value,
                        repaired_to = %var.default,
                        "repaired a persisted URL for a built-in asset this build \
                         no longer serves; reset to the declared default"
                    );
                    patch.value = Some(var.default.clone());
                }
                if let Err(e) = variables::upsert_by_key(ctx, &var.key, patch).await {
                    tracing::warn!(key = %var.key, err = %e, "seed_defaults: metadata refresh failed");
                    failed.push(&var.key);
                }
            }
            None => {
                // Seed from process env when set (lets `.env` bootstrap a
                // fresh deployment), otherwise fall back to the declared
                // default. Empty env values are treated as unset so that
                // `FOO=` doesn't accidentally clear a meaningful default, and
                // so is one that fails the key's declared value rule (named
                // at ERROR, as `seed_and_load` does for the same export).
                let seed_value = std::env::var(&var.key)
                    .ok()
                    .filter(|v| !v.is_empty())
                    .filter(
                        |v| match crate::config_vars::check_config_value(&var.key, v) {
                            Ok(()) => true,
                            Err(e) => {
                                variables::log_refused_env_value(&var.key, v, &e);
                                false
                            }
                        },
                    )
                    .unwrap_or_else(|| var.default.clone());
                if !seed_value.is_empty() {
                    let inserted = variables::insert(
                        ctx,
                        NewVariable {
                            key: var.key.clone(),
                            value: seed_value,
                            name: name.clone(),
                            description: var.description.clone(),
                            warning: var.warning.clone(),
                            sensitive,
                            updated_by: String::new(),
                            block: variables::block_for_key(&var.key),
                        },
                    )
                    .await;
                    if let Err(e) = inserted {
                        tracing::warn!(key = %var.key, err = %e, "seed_defaults: insert failed");
                        failed.push(&var.key);
                    }
                }
            }
        }
    }

    if !failed.is_empty() {
        tracing::warn!(
            failed = ?failed,
            "seed_defaults: some writes failed; the seed hash is left unstamped so the next \
             boot runs the seed again"
        );
        return;
    }

    // Every read and write above succeeded: stamp the new hash on the admin
    // block_settings row so the next cold start short-circuits before issuing
    // `list_all`. The `block_settings` row may not exist yet (admin
    // migrations create it on the same `Init` pass), which is why this is
    // `upsert_fields` rather than an update. A failed stamp is only logged:
    // it errs toward re-running, costing the next boot the same bulk read
    // this one paid.
    //
    // An outage heals because a failed read or write never reaches this
    // stamp: the read returns early above, a failed write returns at the
    // `failed` check, and the gate stays open until a boot on which every
    // read and write lands. The same holds for a write that fails
    // PERMANENTLY — a row the database refuses on every attempt: the gate
    // never closes, so every cold start pays the bulk `list_all` again,
    // retries that write, and logs the "some writes failed" warning above,
    // until the row is repaired.
    let patch = BlockSettingsPatch {
        seed_defaults_hash: Some(code_hash),
        ..Default::default()
    };
    if let Err(e) = block_settings::upsert_fields(ctx, ADMIN_BLOCK_NAME, patch).await {
        tracing::warn!(
            err = %e,
            "seed_defaults: failed to stamp seed_defaults_hash; next cold start will re-run the bulk list_all"
        );
    }
}

#[cfg(test)]
mod tests {
    use wafer_run::InputType;

    use super::*;
    use crate::{
        config_vars::{FAVICON_URL_KEY, LOGO_ICON_URL_KEY},
        test_support::TestContext,
    };

    /// Seed one `variables` row with an explicit `sensitive` flag.
    async fn seed_var(ctx: &dyn Context, key: &str, value: &str, sensitive: bool) {
        variables::insert(
            ctx,
            NewVariable {
                key: key.to_string(),
                value: value.to_string(),
                name: key.to_string(),
                description: String::new(),
                warning: String::new(),
                sensitive,
                updated_by: String::new(),
                block: variables::block_for_key(key),
            },
        )
        .await
        .expect("seed variable");
    }

    /// `GET /b/admin/api/settings` never publishes a secret value.
    ///
    /// The endpoint's OpenAPI description promises exactly this, and the
    /// promise rests on two independent halves of `is_sensitive_key`: the
    /// row's `sensitive` flag, and what the KEY says — the `_SECRET` / `_KEY`
    /// suffix convention, or a declaration that calls the var a password. A
    /// key needs only one of them. Nothing tested this before the endpoint was
    /// documented, which is the worst order to do it in.
    #[tokio::test]
    async fn list_masks_every_sensitive_value() {
        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        crate::blocks::admin::migrations::apply(&ctx)
            .await
            .expect("apply admin migrations");

        // Not sensitive: no flag, no suffix.
        seed_var(&ctx, "SITE_NAME", "Acme", false).await;
        // Sensitive by suffix alone (SEC-060): the flag is clear.
        seed_var(&ctx, "STRIPE_SECRET", "sk_live_realsecret", false).await;
        seed_var(&ctx, "MAILGUN_API_KEY", "key-realsecret", false).await;
        // Sensitive by flag alone: `InputType::Password` vars carry neither
        // suffix, and `seed_defaults` is what sets their flag.
        seed_var(&ctx, "BOOTSTRAP_ADMIN_PASSWORD", "hunter2", true).await;

        let body = crate::test_support::output_json(handle_list(&ctx).await).await;
        let by_key: std::collections::HashMap<&str, &serde_json::Value> = body["settings"]
            .as_array()
            .expect("settings is an array")
            .iter()
            .map(|entry| (entry["key"].as_str().expect("key is a string"), entry))
            .collect();

        assert_eq!(
            by_key["SITE_NAME"]["value"],
            serde_json::json!("Acme"),
            "a non-sensitive value must be published unchanged"
        );
        assert_eq!(
            by_key["SITE_NAME"]["sensitive"],
            serde_json::json!(false),
            "a non-sensitive variable must say so"
        );
        for masked in [
            "STRIPE_SECRET",
            "MAILGUN_API_KEY",
            "BOOTSTRAP_ADMIN_PASSWORD",
        ] {
            assert_eq!(
                by_key[masked]["value"],
                serde_json::json!(MASKED_VALUE),
                "{masked} must be masked"
            );
            assert_eq!(
                by_key[masked]["sensitive"],
                serde_json::json!(true),
                "{masked} must be flagged sensitive, or a reader cannot tell \
                 the mask from a literal value"
            );
        }

        let raw = body.to_string();
        for secret in ["sk_live_realsecret", "key-realsecret", "hunter2"] {
            assert!(
                !raw.contains(secret),
                "GET /b/admin/api/settings leaked `{secret}`: {raw}"
            );
        }
    }

    /// `seed_payload_hash` is independent of input order (sorts by `key`).
    #[test]
    fn payload_hash_independent_of_input_order() {
        let a = ConfigVar::new("AAA", "first", "1");
        let b = ConfigVar::new("BBB", "second", "2");
        let h1 = seed_payload_hash(&[a.clone(), b.clone()]);
        let h2 = seed_payload_hash(&[b, a]);
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64);
    }

    /// Hash changes whenever any seed-relevant field changes.
    #[test]
    fn payload_hash_sensitive_to_each_field() {
        let base = vec![ConfigVar::new("KEY", "desc", "def")];
        let h_base = seed_payload_hash(&base);

        let mut name_changed = base.clone();
        name_changed[0].name = "label".into();
        assert_ne!(h_base, seed_payload_hash(&name_changed));

        let mut desc_changed = base.clone();
        desc_changed[0].description = "different".into();
        assert_ne!(h_base, seed_payload_hash(&desc_changed));

        let mut default_changed = base.clone();
        default_changed[0].default = "other".into();
        assert_ne!(h_base, seed_payload_hash(&default_changed));

        let mut warning_changed = base.clone();
        warning_changed[0].warning = "careful".into();
        assert_ne!(h_base, seed_payload_hash(&warning_changed));

        let mut sensitive_changed = base;
        sensitive_changed[0].input_type = InputType::Password;
        assert_ne!(h_base, seed_payload_hash(&sensitive_changed));
    }

    /// End-to-end: after `seed_defaults` runs once, the admin
    /// `block_settings` row carries the current hash. Wiring that hash into
    /// the next cold-start's config snapshot short-circuits `seed_defaults`
    /// before it can touch the `variables` table — even if every row was
    /// deleted between starts.
    #[tokio::test]
    async fn second_call_with_matching_snapshot_hash_short_circuits() {
        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);

        // 1. Run admin migrations so the block_settings + variables tables
        //    exist (with the new seed_defaults_hash column).
        crate::blocks::admin::migrations::apply(&ctx)
            .await
            .expect("apply admin migrations");

        // 2. First seed run — populates variables + stamps the hash row.
        seed_defaults(&ctx).await;
        let var_count_after_first = variables::list_all(&ctx)
            .await
            .expect("list variables")
            .len();
        assert!(
            var_count_after_first > 0,
            "first seed_defaults should populate at least one variable"
        );

        // 3. Read the stamped hash from the block_settings row directly.
        let admin_rows: Vec<_> = block_settings::list_all(&ctx)
            .await
            .expect("list block_settings")
            .into_iter()
            .filter(|row| row.block_name == ADMIN_BLOCK_NAME)
            .collect();
        assert_eq!(
            admin_rows.len(),
            1,
            "admin block_settings row should be present after first seed_defaults"
        );
        let stamped_hash = admin_rows[0].seed_defaults_hash.clone();
        let code_hash = seed_payload_hash(&crate::config_vars::shared_config_vars());
        assert_eq!(
            stamped_hash, code_hash,
            "stamped seed_defaults_hash should match current declared vars",
        );

        // 4. Simulate a fresh cold start: build a new TestContext (fresh
        //    in-memory DB — no variables, no block_settings row), but
        //    pre-populate the config snapshot with the stamped hash. This
        //    mirrors what the production loader does on the next boot.
        let mut next_ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        crate::blocks::admin::migrations::apply(&next_ctx)
            .await
            .expect("apply admin migrations on next ctx");
        let snapshot = serde_json::json!({
            ADMIN_BLOCK_NAME: { "enabled": true, "seed_defaults_hash": stamped_hash }
        })
        .to_string();
        next_ctx.set_config(crate::features::BLOCK_SETTINGS_CONFIG_KEY, &snapshot);

        // 5. seed_defaults should short-circuit before any list_all on
        //    variables — leaving the (empty) variables table untouched.
        seed_defaults(&next_ctx).await;
        let var_count_after_second = variables::list_all(&next_ctx)
            .await
            .expect("list variables on next ctx")
            .len();
        assert_eq!(
            var_count_after_second, 0,
            "seed_defaults should short-circuit when snapshot hash matches; \
             expected 0 rows in fresh variables table, got {var_count_after_second}"
        );
    }

    /// When the snapshot's cached hash differs from the current code hash
    /// (e.g. a new shared var was declared), `seed_defaults` runs again
    /// and re-stamps the row.
    #[tokio::test]
    async fn mismatched_snapshot_hash_re_runs_seed() {
        let mut ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        crate::blocks::admin::migrations::apply(&ctx)
            .await
            .expect("apply admin migrations");

        // Pre-populate the snapshot with a deliberately-wrong hash.
        let snapshot = serde_json::json!({
            ADMIN_BLOCK_NAME: {
                "enabled": true,
                "seed_defaults_hash": "deadbeef".to_string(),
            }
        })
        .to_string();
        ctx.set_config(crate::features::BLOCK_SETTINGS_CONFIG_KEY, &snapshot);

        seed_defaults(&ctx).await;
        let count = variables::list_all(&ctx)
            .await
            .expect("list variables")
            .len();
        assert!(
            count > 0,
            "mismatched snapshot hash should still run the seed; got 0 rows"
        );
    }

    /// The `seed_defaults_hash` the admin `block_settings` row carries, or
    /// empty when there is no row.
    async fn stored_seed_hash(ctx: &dyn Context) -> String {
        block_settings::list_all(ctx)
            .await
            .expect("list block_settings")
            .into_iter()
            .find(|row| row.block_name == ADMIN_BLOCK_NAME)
            .map(|row| row.seed_defaults_hash)
            .unwrap_or_default()
    }

    /// The next boot as the production loader builds it: the config snapshot
    /// carries whatever hash the database holds.
    async fn snapshot_stored_hash(ctx: &mut TestContext) {
        let snapshot = serde_json::json!({
            ADMIN_BLOCK_NAME: { "enabled": true, "seed_defaults_hash": stored_seed_hash(ctx).await }
        })
        .to_string();
        ctx.set_config(crate::features::BLOCK_SETTINGS_CONFIG_KEY, &snapshot);
    }

    /// One failed write mid-seed leaves the hash gate unstamped, so the next
    /// boot runs the seed again and finishes it.
    ///
    /// The gate skips the whole function once the stamped hash matches the
    /// declarations. Stamping it over a dropped write — which the seed did,
    /// discarding every write result — left that row's metadata stale (or a
    /// dead asset URL unrepaired) on every later boot, until a release
    /// changed a declaration. The fixture stores one declared var under a
    /// stale name, so its metadata refresh is the one `database.update` on
    /// the table in the run; the injector fails exactly that write, while the
    /// inserts for every other var go through.
    ///
    /// Names `variables::TABLE` only to aim the fault injector;
    /// `tests/repo_door.rs` allowlists it as one.
    #[tokio::test]
    async fn a_failed_seed_write_leaves_the_gate_open_for_the_next_boot() {
        use crate::test_support::FailingDbOpContext;

        let mut ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        crate::blocks::admin::migrations::apply(&ctx)
            .await
            .expect("apply admin migrations");
        let declared = crate::config_vars::shared_config_vars();
        let stale = &declared[1];
        assert!(!stale.name.is_empty() && stale.name != stale.key);
        // Stored under its key as its name: the declared name differs.
        seed_var(&ctx, &stale.key, &stale.default, false).await;
        let last = declared
            .iter()
            .rev()
            .find(|v| !v.default.is_empty())
            .expect("a declared var with a default");

        let failing =
            FailingDbOpContext::new(ctx.clone(), vec![("database.update", variables::TABLE)]);
        seed_defaults(&failing).await;

        assert!(
            variables::get_by_key(&ctx, &last.key)
                .await
                .expect("read")
                .is_some(),
            "precondition: the seed kept going past the failed write"
        );
        let code_hash = seed_payload_hash(&declared);
        assert_ne!(
            stored_seed_hash(&ctx).await,
            code_hash,
            "a seed with a failed write must not stamp the gate"
        );

        snapshot_stored_hash(&mut ctx).await;
        seed_defaults(&ctx).await;
        assert_eq!(
            variables::get_by_key(&ctx, &stale.key)
                .await
                .expect("read")
                .expect("row")
                .name,
            stale.name,
            "the next boot re-runs the seed and refreshes the metadata"
        );
        assert_eq!(
            stored_seed_hash(&ctx).await,
            code_hash,
            "a seed whose every write landed stamps the gate"
        );
    }

    /// A failed read of the variables table seeds nothing and stamps
    /// nothing. There is nothing to diff against, and treating it as an
    /// empty table stamped the gate over a seed that never happened.
    ///
    /// Names `variables::TABLE` only to aim the fault injector;
    /// `tests/repo_door.rs` allowlists it as one.
    #[tokio::test]
    async fn a_failed_variables_read_leaves_the_gate_open() {
        use crate::test_support::FailingDbOpContext;

        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        crate::blocks::admin::migrations::apply(&ctx)
            .await
            .expect("apply admin migrations");

        let failing =
            FailingDbOpContext::new(ctx.clone(), vec![("database.list", variables::TABLE)]);
        seed_defaults(&failing).await;

        assert_ne!(
            stored_seed_hash(&ctx).await,
            seed_payload_hash(&crate::config_vars::shared_config_vars()),
            "a seed that could not read the table must not stamp the gate"
        );
    }

    /// SEC-060 regression: the single-key getter must mask a `*_SECRET` value
    /// even when its `sensitive` flag is 0 (the prior code masked on the flag
    /// alone, leaking the secret here).
    #[tokio::test]
    async fn handle_get_masks_secret_suffix_without_flag() {
        use crate::test_support::{admin_msg, output_json};

        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        crate::blocks::admin::migrations::apply(&ctx)
            .await
            .expect("apply admin migrations");

        // Insert a *_SECRET row with the sensitive flag explicitly unset.
        seed_var(&ctx, "STRIPE_SECRET", "sk_live_supersecret", false).await;

        let msg = crate::blocks::admin::test_support::routed(admin_msg(
            "retrieve",
            "/b/admin/api/settings/STRIPE_SECRET",
        ));
        let body = output_json(handle_get(&ctx, &msg).await).await;
        // `Record` serializes as `{ id, data: { value, ... } }`.
        assert_eq!(
            body.get("data")
                .and_then(|d| d.get("value"))
                .and_then(|v| v.as_str()),
            Some(MASKED_VALUE),
            "a *_SECRET value must be masked even with the sensitive flag unset"
        );
    }

    /// A `Password`-typed declared var supplied through the process
    /// environment must come back MASKED from the settings read path.
    ///
    /// The boot seeder derived the row's `sensitive` flag from the
    /// `_SECRET`/`_KEY` suffix alone, so
    /// `WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_PASSWORD` — declared
    /// `InputType::Password`, ending in neither suffix — landed with
    /// `sensitive = 0`. `is_sensitive_key` was then a union of the stored flag
    /// and the suffix alone, and this key satisfies neither, so `GET
    /// /b/admin/api/settings/{key}` returned the bootstrap password in clear.
    /// `seed_defaults` never repairs it: the declared-vars hash gate
    /// short-circuits once stamped. Both halves are closed now and this test
    /// pins the WRITE one — the reader's own declaration half is pinned by
    /// `a_legacy_unflagged_declared_password_row_is_masked_by_every_read_path`,
    /// which stages the row the writer can no longer produce.
    ///
    /// Drives the real write path (`seed_and_load`) into the real read paths,
    /// not the stored integer, because the integer is only interesting for
    /// what the reader does with it.
    #[tokio::test]
    async fn an_env_supplied_password_var_is_masked_by_the_settings_read_path() {
        use crate::test_support::{admin_msg, output_json};

        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        crate::blocks::admin::migrations::apply(&ctx)
            .await
            .expect("apply admin migrations");

        let key = crate::blocks::auth::config::BOOTSTRAP_ADMIN_PASSWORD_KEY;
        ctx.seed_env_vars(&[(key, "hunter2")]).await;

        let msg = crate::blocks::admin::test_support::routed(admin_msg(
            "retrieve",
            &format!("/b/admin/api/settings/{key}"),
        ));
        let body = output_json(handle_get(&ctx, &msg).await).await;
        assert_eq!(
            body.get("data")
                .and_then(|d| d.get("value"))
                .and_then(|v| v.as_str()),
            Some(MASKED_VALUE),
            "an env-supplied bootstrap password must not be readable through the settings API"
        );

        // The listings publish it too, and both must agree.
        for listing in [
            output_json(handle_list(&ctx).await).await,
            output_json(handle_list_full(&ctx).await).await,
        ] {
            let raw = listing.to_string();
            assert!(
                !raw.contains("hunter2"),
                "a settings listing leaked the bootstrap password: {raw}"
            );
        }
    }

    /// A row an OLDER build stored UNFLAGGED for a declaration-only-sensitive
    /// key must still be masked by every settings read path.
    ///
    /// `WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_PASSWORD` is declared
    /// `InputType::Password` and spelled with neither `_SECRET` nor `_KEY`, so
    /// the stored flag was the only thing the readers consulted — and on a
    /// deployment that has not yet run `repair_sensitive_flags` (on Cloudflare,
    /// one with no `/_deploy/init` since the upgrade) that flag is still `0`.
    /// The edit modal already masks such a row off the DECLARATION
    /// (`handle_edit_variable_form`'s `show_sensitive`); these three endpoints
    /// published the password verbatim in the same window. The modal's source
    /// and the read paths' have to be the same source.
    ///
    /// Stages the row in the variables TABLE, not the boot snapshot, and drives
    /// the three real handlers.
    #[tokio::test]
    async fn a_legacy_unflagged_declared_password_row_is_masked_by_every_read_path() {
        use crate::test_support::{admin_msg, output_json};

        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        crate::blocks::admin::migrations::apply(&ctx)
            .await
            .expect("apply admin migrations");

        let key = crate::blocks::auth::config::BOOTSTRAP_ADMIN_PASSWORD_KEY;
        assert!(
            !crate::config_vars::has_sensitive_suffix(key),
            "the point of this test is a key the suffix rule cannot catch"
        );
        assert!(
            crate::config_vars::is_sensitive_for_storage(key),
            "and one the declaration does call sensitive"
        );

        // `variables::insert` would raise the flag on the way in — this is the
        // row an older build left behind, so it goes in unflagged.
        variables::seed_row_with_flag(&ctx, key, "hunter2", 0).await;

        let msg = crate::blocks::admin::test_support::routed(admin_msg(
            "retrieve",
            &format!("/b/admin/api/settings/{key}"),
        ));
        let body = output_json(handle_get(&ctx, &msg).await).await;
        assert_eq!(
            body.get("data")
                .and_then(|d| d.get("value"))
                .and_then(|v| v.as_str()),
            Some(MASKED_VALUE),
            "an unrepaired bootstrap-password row must not be readable through the settings API"
        );

        for listing in [
            output_json(handle_list(&ctx).await).await,
            output_json(handle_list_full(&ctx).await).await,
        ] {
            let raw = listing.to_string();
            assert!(
                !raw.contains("hunter2"),
                "a settings listing leaked an unrepaired bootstrap password: {raw}"
            );
        }
    }

    /// A client that reads a sensitive setting and writes what it read back
    /// must not be able to replace the secret with the mask.
    ///
    /// `GET /b/admin/api/settings/{key}` answers `"********"` for a sensitive
    /// key, and `PATCH` stored the request value verbatim — so the read/modify/
    /// write loop every JSON client is built around (GET the settings, change
    /// one, PATCH them back) overwrote every OTHER secret with eight asterisks.
    /// The worst case is a LIVE `..._BOOTSTRAP_ADMIN_TOKEN`, which is what
    /// provisions the first admin: destroying it strands the deployment with no
    /// admin path, and on Cloudflare there is no process environment to re-seed
    /// it from.
    ///
    /// Drives the two real handlers, GET into PATCH, with the row staged in the
    /// `variables` table — a hand-built mask string would prove only that the
    /// constant is refused, not that the round trip produces it.
    #[tokio::test]
    async fn patching_back_a_masked_value_cannot_overwrite_the_secret() {
        use crate::test_support::{admin_msg, output_http_status, output_json};

        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        crate::blocks::admin::migrations::apply(&ctx)
            .await
            .expect("apply admin migrations");

        let key = crate::blocks::auth::config::BOOTSTRAP_ADMIN_TOKEN_KEY;
        seed_var(&ctx, key, "live-bootstrap-token", true).await;

        // What the client reads.
        let get = crate::blocks::admin::test_support::routed(admin_msg(
            "retrieve",
            &format!("/b/admin/api/settings/{key}"),
        ));
        let read_back = output_json(handle_get(&ctx, &get).await).await["data"]["value"]
            .as_str()
            .expect("the getter publishes a value")
            .to_string();
        assert_eq!(
            read_back, MASKED_VALUE,
            "the read path masks it, which is what makes the write path reachable"
        );

        // ...and what it writes straight back.
        let put = crate::blocks::admin::test_support::routed(admin_msg(
            "update",
            &format!("/b/admin/api/settings/{key}"),
        ));
        let body = serde_json::to_vec(&serde_json::json!({ "value": read_back }))
            .expect("serialize request body");
        let status =
            output_http_status(handle_set(&ctx, &put, InputStream::from_bytes(body)).await).await;

        assert_eq!(
            variables::get_by_key(&ctx, key)
                .await
                .expect("read the row back")
                .expect("the row is still there")
                .value,
            "live-bootstrap-token",
            "a masked round trip must not overwrite the stored secret",
        );
        assert_eq!(
            status, 400,
            "and the client must be told, not given a 200 for a write that did not happen",
        );
    }

    /// The admin API refuses a session lifetime past its bound, on create and
    /// on update, and leaves what is stored alone.
    ///
    /// `100000000` days put the refresh expiry past the last date chrono can
    /// represent, where the addition panicked and aborted the native server on
    /// every login. Drives the two real handlers.
    #[tokio::test]
    async fn an_out_of_range_session_lifetime_is_refused_by_the_settings_api() {
        use crate::test_support::{admin_msg, output_http_status};

        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        crate::blocks::admin::migrations::apply(&ctx)
            .await
            .expect("apply admin migrations");
        let key = crate::blocks::auth::config::SESSION_LIFETIME_DAYS_KEY;

        let post = crate::blocks::admin::test_support::routed(admin_msg(
            "create",
            "/b/admin/api/settings",
        ));
        let body = serde_json::to_vec(&serde_json::json!({
            "key": key,
            "value": "100000000",
            "sensitive": false,
        }))
        .expect("serialize request body");
        let status =
            output_http_status(handle_create(&ctx, &post, InputStream::from_bytes(body)).await)
                .await;
        assert_eq!(status, 400, "create must refuse an out-of-range lifetime");
        assert!(
            variables::get_by_key(&ctx, key)
                .await
                .expect("read back")
                .is_none(),
            "a refused create must not leave a row behind"
        );

        seed_var(&ctx, key, "7", false).await;
        let put = crate::blocks::admin::test_support::routed(admin_msg(
            "update",
            &format!("/b/admin/api/settings/{key}"),
        ));
        for refused in ["100000000", "0", "3651"] {
            let body = serde_json::to_vec(&serde_json::json!({ "value": refused }))
                .expect("serialize request body");
            let status =
                output_http_status(handle_set(&ctx, &put, InputStream::from_bytes(body)).await)
                    .await;
            assert_eq!(status, 400, "update must refuse {refused:?}");
        }
        assert_eq!(
            variables::get_by_key(&ctx, key)
                .await
                .expect("read back")
                .expect("the row is still there")
                .value,
            "7",
            "a refused update must leave the stored lifetime alone"
        );

        let body = serde_json::to_vec(&serde_json::json!({ "value": "30" }))
            .expect("serialize request body");
        let status =
            output_http_status(handle_set(&ctx, &put, InputStream::from_bytes(body)).await).await;
        assert_eq!(status, 200, "an in-range lifetime is accepted");
    }

    /// The remedy the refusal names has to exist: a `PATCH` that leaves `value`
    /// out changes only the fields it carries.
    ///
    /// Without it the refusal would be a dead end for a sensitive key — the
    /// only value a client can read is the mask, and the mask is now refused,
    /// so there would be no way to change the `sensitive` flag (or, later, any
    /// other column) without also knowing the secret.
    #[tokio::test]
    async fn patching_without_a_value_leaves_the_stored_value_alone() {
        use crate::test_support::{admin_msg, output_http_status};

        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        crate::blocks::admin::migrations::apply(&ctx)
            .await
            .expect("apply admin migrations");

        // An ad hoc row: sensitive by the operator's flag alone, so the flag is
        // a thing that can legitimately be turned off.
        seed_var(&ctx, "MY_SERVICE_HANDLE", "acme-prod", true).await;

        let put = crate::blocks::admin::test_support::routed(admin_msg(
            "update",
            "/b/admin/api/settings/MY_SERVICE_HANDLE",
        ));
        let body = serde_json::to_vec(&serde_json::json!({ "sensitive": false }))
            .expect("serialize request body");
        let status =
            output_http_status(handle_set(&ctx, &put, InputStream::from_bytes(body)).await).await;
        assert_eq!(status, 200, "a value-less PATCH is a valid partial update");

        let row = variables::get_by_key(&ctx, "MY_SERVICE_HANDLE")
            .await
            .expect("read the row back")
            .expect("the row is still there");
        assert_eq!(row.value, "acme-prod", "the value column is untouched");
        assert!(!row.sensitive, "and the field that was sent did change");
    }

    /// The echoed row never carries a value the request did not supply.
    ///
    /// The echo used to be masked on the POST-WRITE flag alone, and this exact
    /// request lowers it: an ad hoc row is sensitive by its column only — the
    /// key says nothing — so `PATCH {"sensitive": false}` cleared the flag,
    /// `is_sensitive_key` then answered false, and the response body carried
    /// the plaintext secret back in a request that supplied no value. Making
    /// `value` optional is what created that read; before it, an unflag was
    /// impossible without also overwriting the value it would expose.
    #[tokio::test]
    async fn a_value_less_patch_never_echoes_the_stored_value() {
        use crate::test_support::{admin_msg, output_json};

        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        crate::blocks::admin::migrations::apply(&ctx)
            .await
            .expect("apply admin migrations");
        seed_var(&ctx, "MY_SERVICE_HANDLE", "acme-prod", true).await;

        let put = crate::blocks::admin::test_support::routed(admin_msg(
            "update",
            "/b/admin/api/settings/MY_SERVICE_HANDLE",
        ));
        let body = serde_json::to_vec(&serde_json::json!({ "sensitive": false }))
            .expect("serialize request body");
        let echoed = output_json(handle_set(&ctx, &put, InputStream::from_bytes(body)).await).await;

        assert_ne!(
            echoed["data"]["value"],
            serde_json::json!("acme-prod"),
            "a request that supplied no value must not be answered with one: {echoed}",
        );
        assert!(
            echoed["data"].get("value").is_none(),
            "and the way it is not answered is ABSENCE, not a substituted mask — see \
             `a_value_less_patch_on_a_plain_row_does_not_invent_a_mask` for what \
             substituting one did to rows nothing masks: {echoed}",
        );
    }

    /// A value-less PATCH on a row NOTHING masks must not answer with a mask.
    ///
    /// Substituting `MASKED_VALUE` was the wrong way to spell "this field was
    /// not returned", and it re-opened this PR's own hazard from the other
    /// side: the echo for a plain row said `"********"`, and a client that
    /// replayed what it had just read stored that string as the value — with
    /// `is_masked_submission` correctly declining to stop it, because for a row
    /// nothing masks `"********"` is an ordinary value. The field is simply
    /// absent from the record now, which is what "no value" means in a JSON
    /// object and what the request itself said.
    #[tokio::test]
    async fn a_value_less_patch_on_a_plain_row_does_not_invent_a_mask() {
        use crate::test_support::{admin_msg, output_json};

        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        crate::blocks::admin::migrations::apply(&ctx)
            .await
            .expect("apply admin migrations");
        seed_var(&ctx, "SITE_MOTTO", "move fast", false).await;

        let msg = crate::blocks::admin::test_support::routed(admin_msg(
            "update",
            "/b/admin/api/settings/SITE_MOTTO",
        ));
        let body = serde_json::to_vec(&serde_json::json!({ "sensitive": false }))
            .expect("serialize request body");
        let echoed = output_json(handle_set(&ctx, &msg, InputStream::from_bytes(body)).await).await;
        assert!(
            echoed["data"].get("value").is_none(),
            "a field the request did not supply must be absent, not masked: {echoed}"
        );

        // The read/modify/write client, replaying exactly the fields it was
        // handed. With `value` absent there is nothing to replay.
        let mut replay = serde_json::Map::new();
        if let Some(value) = echoed["data"].get("value") {
            replay.insert("value".to_string(), value.clone());
        }
        replay.insert("sensitive".to_string(), serde_json::json!(false));
        let msg = crate::blocks::admin::test_support::routed(admin_msg(
            "update",
            "/b/admin/api/settings/SITE_MOTTO",
        ));
        let body =
            serde_json::to_vec(&serde_json::Value::Object(replay)).expect("serialize request body");
        let _ = crate::test_support::output_http_status(
            handle_set(&ctx, &msg, InputStream::from_bytes(body)).await,
        )
        .await;

        assert_eq!(
            variables::get_by_key(&ctx, "SITE_MOTTO")
                .await
                .expect("read back")
                .expect("row")
                .value,
            "move fast",
            "replaying the echo must not destroy the value it stood for",
        );
    }

    /// A `PATCH` that supplies no value must not CREATE the row it would then
    /// have to leave empty.
    ///
    /// `update_variable` upserts, and the "Nothing to update" guard only closes
    /// the both-fields-absent case — so `{"sensitive": true}` on a key with no
    /// stored row skipped the `if let Some(value)` block entirely, which is
    /// where the sensitive-empty guard lives, and created a row with `value:
    /// ""`. Before `value` became optional the equivalent `{"value": ""}` was
    /// refused by that guard, so optionality removed a check that had been
    /// running only because the field was always present.
    ///
    /// The blank row is then permanent AND load-bearing: `seed_and_load` seeds
    /// through `insert_if_absent`, which skips any key that already has a row,
    /// and `delete_variable` refuses a declared `WAFER_RUN_SHARED__*` row. One
    /// such request against the bootstrap token disables the environment path
    /// for it forever — the lockout class this whole change exists to prevent.
    #[tokio::test]
    async fn a_value_less_patch_does_not_create_a_blank_row() {
        use crate::test_support::{admin_msg, output_http_status};

        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        crate::blocks::admin::migrations::apply(&ctx)
            .await
            .expect("apply admin migrations");

        let key = crate::blocks::auth::config::BOOTSTRAP_ADMIN_TOKEN_KEY;
        let put = crate::blocks::admin::test_support::routed(admin_msg(
            "update",
            &format!("/b/admin/api/settings/{key}"),
        ));
        let body = serde_json::to_vec(&serde_json::json!({ "sensitive": true }))
            .expect("serialize request body");
        let status =
            output_http_status(handle_set(&ctx, &put, InputStream::from_bytes(body)).await).await;

        assert!(
            variables::get_by_key(&ctx, key)
                .await
                .expect("read back")
                .is_none(),
            "a PATCH with no value must not create a row the boot seeder can never fill",
        );
        assert_eq!(status, 400);
    }

    /// `POST` must not create a blank row for a key something masks either.
    ///
    /// The same row by the other route, and this one predates the change:
    /// `ops::create_variable` has never had an empty-value guard, so
    /// `POST {"key": "...BOOTSTRAP_ADMIN_TOKEN", "value": ""}` produced exactly
    /// the permanent blank row above. Closed here because the guard belongs to
    /// the write, not to one surface's request shape.
    #[tokio::test]
    async fn creating_a_masked_key_with_an_empty_value_is_refused() {
        use crate::test_support::{admin_msg, output_http_status};

        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        crate::blocks::admin::migrations::apply(&ctx)
            .await
            .expect("apply admin migrations");

        let key = crate::blocks::auth::config::BOOTSTRAP_ADMIN_TOKEN_KEY;
        let post = crate::blocks::admin::test_support::routed(admin_msg(
            "create",
            "/b/admin/api/settings",
        ));
        let body = serde_json::to_vec(&serde_json::json!({ "key": key, "value": "" }))
            .expect("serialize request body");
        let status =
            output_http_status(handle_create(&ctx, &post, InputStream::from_bytes(body)).await)
                .await;

        assert!(
            variables::get_by_key(&ctx, key)
                .await
                .expect("read back")
                .is_none(),
            "a create with an empty value must not store a masked key as blank",
        );
        assert_eq!(status, 400);

        // A plain variable may still be created empty — the guard is about what
        // is masked, not about emptiness.
        let post = crate::blocks::admin::test_support::routed(admin_msg(
            "create",
            "/b/admin/api/settings",
        ));
        let body = serde_json::to_vec(&serde_json::json!({
            "key": "SITE_NOTES", "value": "", "sensitive": false
        }))
        .expect("serialize request body");
        assert_eq!(
            output_http_status(handle_create(&ctx, &post, InputStream::from_bytes(body)).await)
                .await,
            200,
        );
    }

    /// ...and it may be created empty while ASKING to be masked.
    ///
    /// This is the fixture that pins the guard to the KEY rather than to the
    /// `sensitive` argument. The Add Variable modal renders its Sensitive box
    /// `checked` by default (`pages/variables.rs`, and
    /// `create_modal_posts_the_flag_explicitly_and_is_checked_by_default`
    /// asserts it), so every empty variable an operator creates through the UI
    /// arrives here asking to be masked. Gating the refusal on the argument
    /// would 400 all of them — and the whole suite would still pass without
    /// this case, since the allowed case above says `"sensitive": false`.
    ///
    /// Safe to allow because what makes a blank row a TRAP is the boot seeder
    /// owning the key and `delete_variable` protecting it, and both follow from
    /// the key's declaration or spelling. This row is neither: it is deletable,
    /// and nothing will ever try to seed it.
    #[tokio::test]
    async fn creating_an_empty_ad_hoc_variable_marked_sensitive_is_allowed() {
        use crate::test_support::{admin_msg, output_http_status};

        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        crate::blocks::admin::migrations::apply(&ctx)
            .await
            .expect("apply admin migrations");

        let post = crate::blocks::admin::test_support::routed(admin_msg(
            "create",
            "/b/admin/api/settings",
        ));
        let body = serde_json::to_vec(&serde_json::json!({
            "key": "SITE_NOTES", "value": "", "sensitive": true
        }))
        .expect("serialize request body");
        assert_eq!(
            output_http_status(handle_create(&ctx, &post, InputStream::from_bytes(body)).await)
                .await,
            200,
            "the Add Variable modal's default flow must not be refused",
        );
        assert!(variables::get_by_key(&ctx, "SITE_NOTES")
            .await
            .expect("read back")
            .is_some());
    }

    /// Making both fields optional must not make an empty body a way to
    /// conjure a blank row: `update_variable` upserts, so `PATCH {}` on an
    /// unstored key would create one.
    #[tokio::test]
    async fn patching_with_no_fields_at_all_is_refused() {
        use crate::test_support::{admin_msg, output_http_status};

        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        crate::blocks::admin::migrations::apply(&ctx)
            .await
            .expect("apply admin migrations");

        let put = crate::blocks::admin::test_support::routed(admin_msg(
            "update",
            "/b/admin/api/settings/NOT_STORED_YET",
        ));
        let status = output_http_status(
            handle_set(&ctx, &put, InputStream::from_bytes(b"{}".to_vec())).await,
        )
        .await;
        assert_eq!(status, 400);
        assert!(
            variables::get_by_key(&ctx, "NOT_STORED_YET")
                .await
                .expect("read back")
                .is_none(),
            "a field-less PATCH must not create a row",
        );
    }

    /// The refusal is scoped to keys whose value the reader masks. A variable
    /// that is not sensitive may hold the literal string — it is only a mask
    /// where something masked it.
    #[tokio::test]
    async fn a_non_sensitive_variable_may_hold_the_mask_string() {
        use crate::test_support::{admin_msg, output_http_status};

        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        crate::blocks::admin::migrations::apply(&ctx)
            .await
            .expect("apply admin migrations");
        seed_var(&ctx, "PASSWORD_PLACEHOLDER_TEXT", "type here", false).await;

        let put = crate::blocks::admin::test_support::routed(admin_msg(
            "update",
            "/b/admin/api/settings/PASSWORD_PLACEHOLDER_TEXT",
        ));
        let body = serde_json::to_vec(&serde_json::json!({ "value": MASKED_VALUE }))
            .expect("serialize request body");
        assert_eq!(
            output_http_status(handle_set(&ctx, &put, InputStream::from_bytes(body)).await).await,
            200,
        );
        assert_eq!(
            variables::get_by_key(&ctx, "PASSWORD_PLACEHOLDER_TEXT")
                .await
                .expect("read the row back")
                .expect("the row is still there")
                .value,
            MASKED_VALUE,
        );
    }

    /// Read one variable row's `value` column.
    async fn stored_value(ctx: &dyn Context, key: &str) -> Option<String> {
        variables::get_by_key(ctx, key)
            .await
            .expect("get variable")
            .map(|row| row.value)
    }

    /// Releases before the pixel-art mark seeded `LOGO_URL` with the built-in
    /// raster wordmark's content-hashed URL. That asset and its route are
    /// gone, so a deployment still carrying the seeded value renders a broken
    /// image on every auth card, sidebar and account card. `seed_defaults`
    /// must clear it back to blank so the app-name fallback takes over.
    #[tokio::test]
    async fn seed_defaults_clears_the_removed_builtin_wordmark_url() {
        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        crate::blocks::admin::migrations::apply(&ctx)
            .await
            .expect("apply admin migrations");

        // Exactly what an older release's `seed_defaults` wrote: the route
        // prefix plus that release's content hash.
        seed_var(
            &ctx,
            crate::config_vars::LOGO_URL_KEY,
            "/b/static/impresspress-logo-long-1f4c8ab2.png",
            false,
        )
        .await;

        seed_defaults(&ctx).await;

        assert_eq!(
            stored_value(&ctx, crate::config_vars::LOGO_URL_KEY)
                .await
                .as_deref(),
            Some(""),
            "a persisted pointer at the removed built-in wordmark must be \
             cleared so the app-name fallback renders"
        );
    }

    /// The repair reaches a *real* upgrade, not just a blank slate.
    ///
    /// The seed short-circuits when the stamped `seed_defaults_hash` matches
    /// the current declared vars, so the repair only ever runs if that gate
    /// opens. It does: this release changed `LOGO_URL`'s declared default
    /// *and* its description, both of which feed `seed_payload_hash`, so any
    /// older release's stamped hash necessarily differs. This pins that —
    /// stamp a prior release's hash, then assert the stale value is still
    /// repaired.
    #[tokio::test]
    async fn stale_wordmark_is_repaired_through_a_prior_releases_stamped_hash() {
        let mut ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        crate::blocks::admin::migrations::apply(&ctx)
            .await
            .expect("apply admin migrations");

        // A prior release's declared LOGO_URL: the old description, and the
        // built-in wordmark URL as the default. Its hash is what that release
        // would have stamped.
        let mut prior = crate::config_vars::shared_config_vars();
        let logo = prior
            .iter_mut()
            .find(|v| v.key == crate::config_vars::LOGO_URL_KEY)
            .expect("LOGO_URL must be a declared shared var");
        logo.description = "Logo shown in header and emails".into();
        logo.default = "/b/static/impresspress-logo-long-1f4c8ab2.png".into();
        let prior_hash = seed_payload_hash(&prior);

        ctx.set_config(
            crate::features::BLOCK_SETTINGS_CONFIG_KEY,
            &serde_json::json!({
                ADMIN_BLOCK_NAME: { "enabled": true, "seed_defaults_hash": prior_hash }
            })
            .to_string(),
        );
        seed_var(
            &ctx,
            crate::config_vars::LOGO_URL_KEY,
            "/b/static/impresspress-logo-long-1f4c8ab2.png",
            false,
        )
        .await;

        seed_defaults(&ctx).await;

        assert_eq!(
            stored_value(&ctx, crate::config_vars::LOGO_URL_KEY)
                .await
                .as_deref(),
            Some(""),
            "the hash gate must open on upgrade so the repair runs"
        );
    }

    /// The repair above is scoped to the built-in wordmark's own route. An
    /// operator's white-label logo is their data and must survive untouched.
    #[tokio::test]
    async fn seed_defaults_keeps_an_operator_configured_logo_url() {
        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        crate::blocks::admin::migrations::apply(&ctx)
            .await
            .expect("apply admin migrations");

        seed_var(
            &ctx,
            crate::config_vars::LOGO_URL_KEY,
            "https://acme.example/wordmark.png",
            false,
        )
        .await;

        seed_defaults(&ctx).await;

        assert_eq!(
            stored_value(&ctx, crate::config_vars::LOGO_URL_KEY)
                .await
                .as_deref(),
            Some("https://acme.example/wordmark.png"),
            "a white-labelled logo URL must not be cleared"
        );
    }

    /// The repair is not specific to the removed wordmark. Any release that
    /// changes an asset's bytes changes its content hash, and the previous
    /// hash is already seeded into every existing deployment's database —
    /// so the sidebar mark and the browser tab icon go dead on upgrade
    /// exactly like the wordmark did. Unlike the wordmark, these assets still
    /// exist, so the repair must point them at the *current* default rather
    /// than blank.
    #[tokio::test]
    async fn seed_defaults_repairs_stale_builtin_logo_and_favicon_urls() {
        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        crate::blocks::admin::migrations::apply(&ctx)
            .await
            .expect("apply admin migrations");

        // What a release built before the artwork changed would have seeded:
        // the right route, a hash this build no longer serves.
        seed_var(
            &ctx,
            LOGO_ICON_URL_KEY,
            "/b/static/impresspress-logo-5e884a3a.png",
            false,
        )
        .await;
        seed_var(
            &ctx,
            FAVICON_URL_KEY,
            "/b/static/favicon-2845a6ac.ico",
            false,
        )
        .await;

        seed_defaults(&ctx).await;

        assert_eq!(
            stored_value(&ctx, LOGO_ICON_URL_KEY).await.as_deref(),
            Some(crate::ui::assets::logo_icon_url().as_str()),
            "a stale built-in logo URL must be repaired to the current asset"
        );
        assert_eq!(
            stored_value(&ctx, FAVICON_URL_KEY).await.as_deref(),
            Some(crate::ui::assets::favicon_url().as_str()),
            "a stale built-in favicon URL must be repaired to the current asset"
        );
    }

    /// A row already naming the current asset is left alone — the repair is
    /// idempotent, and must not churn a write on every boot.
    #[tokio::test]
    async fn seed_defaults_leaves_a_current_builtin_logo_url_untouched() {
        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        crate::blocks::admin::migrations::apply(&ctx)
            .await
            .expect("apply admin migrations");

        let current = crate::ui::assets::logo_icon_url();
        seed_var(&ctx, LOGO_ICON_URL_KEY, &current, false).await;

        seed_defaults(&ctx).await;

        assert_eq!(
            stored_value(&ctx, LOGO_ICON_URL_KEY).await.as_deref(),
            Some(current.as_str()),
        );
    }
}

#[cfg(test)]
mod create_tests {
    use wafer_run::InputStream;

    use super::*;
    use crate::test_support::{admin_msg, collect_or_panic, TestContext};

    async fn admin_ctx() -> TestContext {
        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        crate::blocks::admin::migrations::apply(&ctx)
            .await
            .expect("apply admin migrations");
        ctx
    }

    async fn sensitive_flag(ctx: &dyn Context, key: &str) -> bool {
        variables::get_by_key(ctx, key)
            .await
            .expect("get variable")
            .unwrap_or_else(|| panic!("{key} was not created"))
            .sensitive
    }

    async fn create(ctx: &dyn Context, body: serde_json::Value) {
        let out = handle_create(
            ctx,
            &admin_msg("create", "/b/admin/api/settings"),
            InputStream::from_bytes(serde_json::to_vec(&body).unwrap()),
        )
        .await;
        collect_or_panic(out).await;
    }

    /// `POST /b/admin/api/settings` with a key that is already stored answers
    /// **409**, not the 500 the 2026-09-10 live-server audit found. The
    /// classification lives in `ops::create_variable`; this pins that the JSON
    /// surface publishes it rather than reshaping it on the way out.
    #[tokio::test]
    async fn creating_an_existing_key_answers_conflict() {
        let ctx = admin_ctx().await;
        create(
            &ctx,
            serde_json::json!({"key": "SITE_MOTTO", "value": "one"}),
        )
        .await;

        let out = handle_create(
            &ctx,
            &admin_msg("create", "/b/admin/api/settings"),
            InputStream::from_bytes(
                serde_json::to_vec(&serde_json::json!({"key": "SITE_MOTTO", "value": "two"}))
                    .unwrap(),
            ),
        )
        .await;
        assert_eq!(crate::test_support::output_http_status(out).await, 409);
    }

    /// An ad hoc variable created without saying whether it is sensitive is
    /// stored as sensitive. Masking an innocuous value costs the operator one
    /// click to undo; publishing a secret in plain text — which is what the
    /// old `false` default did for any key without a `_SECRET`/`_KEY`
    /// suffix — cannot be undone.
    #[tokio::test]
    async fn create_defaults_to_sensitive_when_the_flag_is_omitted() {
        let ctx = admin_ctx().await;
        create(
            &ctx,
            serde_json::json!({"key": "SITE_MOTTO", "value": "move fast"}),
        )
        .await;
        assert!(sensitive_flag(&ctx, "SITE_MOTTO").await);
    }

    #[tokio::test]
    async fn create_honours_an_explicit_not_sensitive() {
        let ctx = admin_ctx().await;
        create(
            &ctx,
            serde_json::json!({"key": "SITE_MOTTO", "value": "move fast", "sensitive": false}),
        )
        .await;
        assert!(!sensitive_flag(&ctx, "SITE_MOTTO").await);
    }
}

#[cfg(test)]
mod wrap_denial_tests {
    use super::*;
    use crate::test_support::{admin_msg, output_http_status, TestContext};

    /// A block deployed without the grant its handler needs answers **403**,
    /// not `500 Internal server error (ref: …)`.
    ///
    /// The three-arm `Ok(Some) / Ok(None) / Err` shape these handlers used
    /// tested only for the missing row; a WRAP refusal fell through the `Err`
    /// arm into `err_internal`, so a missing `ResourceGrant` in production was
    /// indistinguishable from a corrupt row. `crud::db_error` is the arm that
    /// was missing.
    async fn denied_ctx() -> TestContext {
        TestContext::with_admin().await.running_as("test/ungranted")
    }

    #[tokio::test]
    async fn a_denied_settings_read_is_403() {
        let ctx = denied_ctx().await;
        let mut msg = admin_msg("retrieve", "/b/admin/api/settings/SOME_KEY");
        msg.set_meta("req.param.key", "SOME_KEY");
        assert_eq!(output_http_status(handle_get(&ctx, &msg).await).await, 403);
    }

    #[tokio::test]
    async fn a_denied_settings_delete_is_403() {
        let ctx = denied_ctx().await;
        let mut msg = admin_msg("delete", "/b/admin/api/settings/SOME_KEY");
        msg.set_meta("req.param.key", "SOME_KEY");
        assert_eq!(
            output_http_status(handle_delete(&ctx, &msg).await).await,
            403
        );
    }
}

/// CFG-01 reproduction. Kept in its own module so the fixture it needs — a
/// config service seeded from the table the way boot seeds it — cannot leak
/// into the tests above, which deliberately seed their own values.
#[cfg(test)]
mod config_store_reproduction {
    use super::*;
    use crate::test_support::{unique_config_value, TestContext};

    /// A setting changed through the documented admin API must reach the
    /// config readers blocks actually use.
    ///
    /// `PATCH /b/admin/api/settings/{key}` persists through
    /// `ops::update_variable` → `variables::upsert_by_key`, which writes the
    /// `variables` table and stops there. Every block instead reads through
    /// `wafer_core::clients::config::get_default`, served by an
    /// `EnvConfigService` that `impresspress_server::build_native_runtime` seeds from that table exactly
    /// once at boot. Nothing rejoins the two surfaces, so an admin who
    /// changes the site's primary colour through the documented endpoint
    /// keeps seeing the old one until the process restarts.
    ///
    /// Confirmed live on 2026-09-10: restarting a native server against the
    /// same database made the page render the new colour.
    #[tokio::test]
    async fn patch_settings_reaches_config_readers_without_a_restart() {
        const KEY: &str = crate::config_vars::PRIMARY_COLOR_KEY;

        let mut ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        crate::blocks::admin::migrations::apply(&ctx)
            .await
            .expect("apply admin migrations");
        // Boot once, the way the native server does, before any admin write.
        ctx.boot_config_service().await;

        let saved = unique_config_value();
        let mut msg = crate::test_support::admin_msg("update", "/b/admin/api/settings");
        msg.set_meta("req.param.key", KEY);
        let body = serde_json::to_vec(&serde_json::json!({ "value": saved }))
            .expect("serialize request body");
        let status = crate::test_support::output_status(
            handle_set(&ctx, &msg, InputStream::from_bytes(body)).await,
        )
        .await;
        assert_eq!(
            status, 200,
            "the documented admin endpoint accepted the change"
        );

        // The durable half works: the row is there.
        let row = variables::get_by_key(&ctx, KEY)
            .await
            .expect("read the variable back")
            .expect("the admin write created a row");
        assert_eq!(row.value, saved);

        // The half that decides what a visitor sees. `ui/mod.rs:73` reads
        // this key through the async client.
        let seen = wafer_core::clients::config::get_default(&ctx, KEY, "unset")
            .await
            .expect("config read");
        assert_eq!(
            seen, saved,
            "a saved admin setting must be visible to async config readers without a restart"
        );

        // Read surface 2 — the synchronous `ctx.config_get` snapshot, which
        // `blocks/auth_ui/pages/mod.rs:62` uses for this exact key — is
        // deliberately NOT asserted here. The config-store decision does not
        // rejoin that snapshot; its step 3 moves those eleven branding reads
        // onto the async client instead. Asserting `config_get` would pin a
        // surface the plan abandons, and would fail forever however correct
        // the fix. The requirement it stood for — the login page showing the
        // saved colour — belongs to that migration, and is tracked with it.
    }
}
