//! The data snapshot — `seed/data.json` (design §10.1, amendment 9).
//!
//! An export is not just files and blocks: a shop has rows — products,
//! offers, the owner's own account. [`export`] reads an explicit table
//! allowlist into a [`DataSnapshot`]; [`import`] applies it back through the
//! typed database client. **No SQL text is generated or executed anywhere in
//! this module** — the import is one `db::batch` of typed writes, exactly as
//! amendment 9 requires and as
//! `CLAUDE.md`'s "no raw SQL in block code" rule already demands of every
//! other block.
//!
//! # The allowlist is closed, not additive
//!
//! [`TABLE_ALLOWLIST`] and [`TABLE_EXCLUDED`] between them must name every
//! collection the products, admin and auth blocks declare — a new table that
//! lands in neither is a decision nobody made, and
//! `every_declared_table_of_the_three_blocks_has_an_export_decision`
//! (`tests/dev_data_snapshot.rs`) fails the build the moment that happens.
//! [`import`] enforces the same closure from the other direction: a snapshot
//! naming a table outside [`TABLE_ALLOWLIST`] is refused rather than applied,
//! so a hand-edited bundle (or one produced by a build with a wider
//! allowlist) cannot smuggle a write into a table this build never decided
//! to trust.
//!
//! # What never leaves
//!
//! `impresspress__admin__variables` is filtered row-by-row through
//! [`variable_is_exportable`] as it is read, so a sensitive value or an
//! `IMPRESSPRESS_`-prefixed infrastructure key never enters the snapshot in
//! the first place — there is no later redaction step for a bug to skip.
//! Every session, token, audit-log and payment/provider-state table is kept
//! off [`TABLE_ALLOWLIST`] entirely (see the comments there): those describe
//! *this running instance*, not the shop, and are excluded by name rather
//! than by any runtime check.
//!
//! The one deliberate exception is the owner's own login: `users` and
//! `wafer_run__auth__local_credentials` (their password hash) travel
//! together, `Replace`d as a pair, because the export exists so the owner can
//! re-host their own shop and log back into it — design §10.1 calls this out
//! explicitly, and the exported bundle's README discloses it.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use wafer_block::wire::database::{BatchWrite, OnConflict, UpsertRequest};
use wafer_core::clients::database as db;
use wafer_run::{context::Context, ErrorCode, WaferError};

// A standalone, unbraced import so the text `products::TABLE` — the pattern
// `blocks/products/tests/repo_door_test.rs`'s `only_the_allowlist_names_the_products_table_via_the_const`
// scans for — appears here plainly rather than hidden inside the larger
// grouped import below; this file is listed in that test's
// `TABLE_IDENT_ALLOWED` with the same justification given there.
use crate::blocks::products::TABLE as PRODUCTS_COLLECTION;
use crate::{
    blocks::{
        admin::{AUDIT_LOGS_TABLE, PERMISSIONS_TABLE, ROLES_TABLE, STORAGE_ACCESS_LOGS_TABLE},
        auth::repo::{
            api_keys, bootstrap_tokens, jwt_blocklist, local_credentials, maintenance, oauth_pkce,
            orgs, pats, provider_links, rate_limits, sessions, tokens, users,
        },
        products::{
            list_live_products, product_snapshot_upsert, CHECKOUT_PRESETS_TABLE, DISPUTES_TABLE,
            ENTITLEMENTS_TABLE, GROUPS_TABLE, GROUP_TEMPLATES_TABLE, LINE_ITEMS_TABLE,
            OFFERS_TABLE, OFFER_COMPONENTS_TABLE, PAYMENT_LINKS_TABLE, PRODUCT_TEMPLATES_TABLE,
            PRODUCT_VERSIONS_TABLE, PROVIDER_OPERATIONS_TABLE, PURCHASES_TABLE, REFUNDS_TABLE,
            SELLER_ACCOUNTS_TABLE, STRIPE_EVENTS_TABLE, SUBSCRIPTIONS_TABLE,
            SUBSCRIPTION_ITEMS_TABLE, TYPES_TABLE, VARIABLES_TABLE as PRODUCTS_VARIABLES_TABLE,
        },
    },
    // audit-allow: names the platform tables for the export allowlist/exclusion bookkeeping below — the two it reads (`variables`, `user_roles`) are granted by `dev::wrap_grants()`, which maps every `TABLE_ALLOWLIST` entry to `read_write(BLOCK_NAME, table)` and which the runtime honours from its flat grant list, and the audit attributes grants to the declaring file's block and cannot see it
    platform_state::{block_settings, request_logs, user_roles, variables, wrap_grants},
};

/// Schema version this build's [`DataSnapshot`] reads and writes.
///
/// Separate from [`super::seed::SCHEMA_VERSION`] and
/// [`super::generation::SCHEMA_VERSION`] for the reason those two are
/// separate from each other: this is the shape of one interchange format
/// (rows per allowlisted table), free to move at its own pace.
pub const SCHEMA_VERSION: u32 = 1;

/// The on-disk shape of `seed/data.json`: every allowlisted table's rows,
/// keyed by table name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataSnapshot {
    /// Must equal [`SCHEMA_VERSION`] for [`import`] to accept it.
    pub schema_version: u32,
    /// `BTreeMap`, not `HashMap`: table order is incidental to what an
    /// export means, so sorting it is free reproducibility — two exports of
    /// the same data serialize to the same bytes, the same reason
    /// [`super::zip::ZipWriter`]'s archives are byte-identical across runs.
    pub tables: BTreeMap<String, Vec<serde_json::Map<String, Value>>>,
}

/// The conflict target of a [`Mode::Upsert`] table whose identity is its `id`.
///
/// Most exported tables are like this: the row's id is the only thing that
/// identifies it, and two instances never mint the same one.
pub const BY_ID: &[&str] = &["id"];

/// How [`import`] applies one allowlisted table's rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Upsert each row, with the named columns as the conflict target.
    /// Safe to run repeatedly — a second import of the same snapshot updates
    /// the same rows rather than duplicating them — and never removes a row
    /// the destination already has that the snapshot doesn't mention.
    ///
    /// **The columns are the table's real identity, not always `id`.** For
    /// most tables they are [`BY_ID`], and this is the same thing an upsert
    /// keyed on the primary key has always done. For the handful whose rows
    /// the DESTINATION mints for itself — `roles` and `permissions` come from
    /// admin's own migration, `variables` from its boot seeder — the id is
    /// per-install and the identity is a natural key the schema marks
    /// `UNIQUE` (`roles.name`, `permissions.name`, `variables.key`). Keyed on
    /// `id` those rows do not conflict on the id at all: they are INSERTs
    /// that then violate the unique index on the natural key, and the whole
    /// import fails on it. Design §10.2's
    /// promise that a bundle imports into a fresh instance is exactly the
    /// case where the destination has already seeded its own copies of these
    /// rows, so this is not a corner.
    ///
    /// On a conflict the destination keeps its OWN `id` (and its own value of
    /// the conflict columns, which are equal by definition) and takes every
    /// other column from the snapshot — see [`upsert_op`].
    Upsert(&'static [&'static str]),
    /// Delete every row in the destination table, then insert the exported
    /// rows, in the import's one transaction. Reserved for the tables whose *set* must match the
    /// snapshot exactly: a fresh instance's own bootstrap admin (and its
    /// role assignment, and its local credentials) must be gone once someone
    /// else's account is imported, not merged alongside it.
    Replace,
}

/// Every table this build exports, and how [`import`] applies its rows.
///
/// See the module docs for why this list — together with [`TABLE_EXCLUDED`]
/// — is a closed set, and for the one exception ([`local_credentials`])
/// among the tables usually thought of as "secrets".
pub const TABLE_ALLOWLIST: &[(&str, Mode)] = &[
    // --- products: catalog structure. No money moved, nothing tied to a
    // specific buyer, subscription or provider account — the shop's shape,
    // not its history. ---
    // Read and written through `products::list_live_products`/
    // `product_snapshot_upsert`, never through the generic
    // `db::list_all`/upsert path below — this table alone carries a
    // soft-delete filter its own repo module's door tests enforce (see
    // `export`/`upsert_op`).
    (PRODUCTS_COLLECTION, Mode::Upsert(BY_ID)),
    (GROUPS_TABLE, Mode::Upsert(BY_ID)),
    (TYPES_TABLE, Mode::Upsert(BY_ID)),
    (GROUP_TEMPLATES_TABLE, Mode::Upsert(BY_ID)),
    (PRODUCT_TEMPLATES_TABLE, Mode::Upsert(BY_ID)),
    (PRODUCT_VERSIONS_TABLE, Mode::Upsert(BY_ID)),
    (OFFERS_TABLE, Mode::Upsert(BY_ID)),
    // The four below hang off a row above them, and [`export`] filters each
    // against the ids its owner actually exported — so the order here is
    // load-bearing, not alphabetical: an owner listed after its dependents
    // would be filtering against a set nothing had filled yet. `variables`
    // sits here rather than beside the other product-shaped tables for
    // exactly that reason. [`OWNED_TABLES`], and
    // `every_owned_table_is_listed_after_its_owner`, are what keep it true.
    (PRODUCTS_VARIABLES_TABLE, Mode::Upsert(BY_ID)),
    (OFFER_COMPONENTS_TABLE, Mode::Upsert(BY_ID)),
    (CHECKOUT_PRESETS_TABLE, Mode::Upsert(BY_ID)),
    // --- admin: IAM catalog plus config. `variables::TABLE` is filtered row
    // by row at export time (`variable_is_exportable`) rather than excluded
    // wholesale — most admin variables are ordinary site config (`APP_NAME`,
    // feature flags), exactly what a re-hosted copy needs to keep working.
    // These three are keyed on their NATURAL key, not on `id`: the
    // destination seeds its own `roles`/`permissions` rows from admin's
    // migration and its own `variables` rows from the boot seeder, each with
    // a freshly minted id, and each table marks the natural key `UNIQUE`. An
    // upsert keyed on `id` inserts a second `admin` role / a second
    // `APP_NAME` variable and dies on that index. See [`Mode::Upsert`].
    (ROLES_TABLE, Mode::Upsert(&["name"])),
    (PERMISSIONS_TABLE, Mode::Upsert(&["name"])),
    (variables::TABLE, Mode::Upsert(&["key"])),
    // --- identity: the owner's own account, `Replace`d as a set so a fresh
    // instance's bootstrap admin is gone once someone else's is imported —
    // every `owner_id`/`created_by` an imported product carries still
    // resolves, and the instance never ends up with two admins. See the
    // module docs for why `local_credentials` travels with `users`. ---
    (users::TABLE, Mode::Replace),
    (local_credentials::TABLE, Mode::Replace),
    (user_roles::TABLE, Mode::Replace),
];

/// Every table the products, admin and auth blocks declare that
/// [`TABLE_ALLOWLIST`] deliberately leaves out of every export.
pub const TABLE_EXCLUDED: &[&str] = &[
    // --- products: money moved, a specific order, or per-instance Stripe
    // state — none of it portable to a different deployment. ---
    PURCHASES_TABLE,
    LINE_ITEMS_TABLE,
    SUBSCRIPTIONS_TABLE,
    SUBSCRIPTION_ITEMS_TABLE,
    ENTITLEMENTS_TABLE,
    PAYMENT_LINKS_TABLE,
    SELLER_ACCOUNTS_TABLE,
    PROVIDER_OPERATIONS_TABLE,
    REFUNDS_TABLE,
    DISPUTES_TABLE,
    STRIPE_EVENTS_TABLE, // webhook idempotency ledger — this instance's own delivery history
    // --- admin: operational logs, and infrastructure state the runtime
    // re-derives at every boot rather than something anyone authored. ---
    block_settings::TABLE, // per-block enable flag + migration-hash tracking
    request_logs::TABLE,
    AUDIT_LOGS_TABLE,
    STORAGE_ACCESS_LOGS_TABLE,
    // Admin-created WRAP grants. Authored, but access policy: an import that
    // carried them would widen what blocks may touch on the destination
    // without its operator granting it, so each deployment keeps its own.
    wrap_grants::TABLE,
    // --- auth: session/credential plumbing scoped to this running
    // instance (bearer material this instance issued, not the owner's own
    // login — see `local_credentials` above), plus multi-tenant org
    // records a single-owner sandbox shop has no use for. ---
    sessions::TABLE,
    tokens::TABLE,
    api_keys::TABLE,
    bootstrap_tokens::TABLE,
    jwt_blocklist::TABLE,
    oauth_pkce::TABLE,
    pats::TABLE,
    provider_links::TABLE,
    rate_limits::TABLE,
    orgs::TABLE,
    // The sweeper's throttle singleton: one row holding when the expired-row
    // sweep last ran on THIS instance. Exporting it would carry a foreign
    // clock into the destination and suppress its first sweep.
    maintenance::TABLE,
];

/// Whether one row of `impresspress__admin__variables` may leave in an
/// export.
///
/// **Fails closed.** A row is exportable only when *every* check below
/// affirmatively clears it — an odd shape (a `sensitive` value that isn't
/// cleanly `0`/`false`, a `key` that isn't a plain string, a missing field
/// entirely) is not "assume the safe default and export it", it is "cannot
/// prove this is safe, so don't". The row's own schema guarantees `sensitive
/// INTEGER NOT NULL DEFAULT 0` and `key TEXT NOT NULL`, so a row this
/// function refuses to clear is either corrupt or from a build with a wider
/// schema than this one reads — either way, the conservative answer is the
/// only correct one.
///
/// Reuses [`crate::util::is_sensitive_key`] — the same SEC-060 rule the
/// admin Variables page masks display values with (explicit `sensitive` flag
/// **or** a key the build knows to hold a secret: the `_SECRET`/`_KEY` suffix,
/// or a declared `Password`/`auto_generate` `ConfigVar`) — rather than checking
/// the flag alone: a second, weaker sensitivity check here would be exactly the
/// kind of disagreement that rule exists to prevent. Called with the flag
/// already pinned to "clean false" (checked below), so it only evaluates the
/// key half — which is the half that holds back a legacy
/// `WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_PASSWORD` row, still unflagged
/// because this deployment has not run the repair pass. Export is the
/// irreversible direction: a bundle that reaches another deployment cannot be
/// recalled.
///
/// The `IMPRESSPRESS_` prefix check is this module's own, additional rule,
/// and it is deliberately the *broad* prefix — it matches both shapes
/// `CLAUDE.md` spells with it, for two different reasons:
///
/// - `IMPRESSPRESS_*` (single underscore) is infrastructure config, reserved
///   never to reach the database at all. A row like that appearing here is
///   already a bug upstream; this is the export's backstop against it
///   leaving anyway.
/// - `IMPRESSPRESS__{BLOCK}__*` (double underscore) is block-scoped config,
///   which does live in the database and is perfectly ordinary — but it is
///   *this instance's*, and the destination is someone else's deployment.
///   `IMPRESSPRESS__PRODUCTS__CHECKOUT_ALLOWED_ORIGINS` names origins the
///   importing site does not have, `IMPRESSPRESS__EMAIL__MAILGUN_DOMAIN` a
///   sender it cannot send as, and [`crate::blocks::dev::seed::SEED_ERROR_KEY`]
///   a boot failure that happened somewhere else entirely. Shared config
///   (`WAFER_RUN_SHARED__*`) is the half that describes the *site* rather
///   than the instance hosting it, and that is the half that travels.
///
/// So an importing site starts with its block-scoped config unset and
/// configures it on its own admin page, rather than starting with another
/// deployment's and having to notice.
pub fn variable_is_exportable(row: &serde_json::Map<String, Value>) -> bool {
    let Some(key) = row.get("key").and_then(Value::as_str) else {
        return false;
    };
    let sensitive_is_clean_false = match row.get("sensitive") {
        Some(Value::Bool(b)) => !*b,
        Some(Value::Number(n)) => n.as_i64() == Some(0),
        // Missing, `null`, a numeric string, a float, an object/array — none
        // of these is the clean `0`/`false` the schema promises, so this
        // does not clear the row.
        _ => false,
    };
    if !sensitive_is_clean_false {
        return false;
    }
    if crate::util::is_sensitive_key(key, 0) {
        return false;
    }
    // Exactly the set [`import`] refuses, stated independently of the prefix
    // rule below it. The two exist for different reasons — the prefix rule is
    // about instance-specificity and also holds back block-scoped
    // `IMPRESSPRESS__{BLOCK}__*` — so if it were ever narrowed to let that
    // config travel, the infrastructure half would silently stop being
    // filtered here while the importer still refused it, and this exporter
    // would produce bundles its own importer rejects.
    //
    // It catches what the prefix rule cannot in any case: an internal key like
    // `__IMPRESSPRESS_RUNTIME_KIND__` starts with `__`, so it clears
    // `starts_with("IMPRESSPRESS_")`, and ends with `__` rather than a
    // `_SECRET`/`_KEY` suffix, so it clears `is_sensitive_key` too.
    !crate::config_vars::is_instance_owned_key(key) && !key.starts_with("IMPRESSPRESS_")
}

#[cfg(test)]
mod variable_is_exportable_tests {
    use super::*;
    use crate::{
        blocks::{
            email::MAILGUN_DOMAIN,
            products::config::{CHECKOUT_ALLOWED_ORIGINS, PLATFORM_COUNTRY},
        },
        config_vars::{APP_NAME_KEY, REQUEST_LOG_CONFIG_KEY},
    };

    fn row(fields: serde_json::Value) -> serde_json::Map<String, Value> {
        match fields {
            Value::Object(map) => map,
            _ => panic!("row fixture must be a JSON object"),
        }
    }

    #[test]
    fn a_clean_non_sensitive_row_exports() {
        assert!(variable_is_exportable(&row(serde_json::json!({
            "key": APP_NAME_KEY,
            "sensitive": false,
        }))));
        assert!(variable_is_exportable(&row(serde_json::json!({
            "key": APP_NAME_KEY,
            "sensitive": 0,
        }))));
    }

    #[test]
    fn an_explicitly_sensitive_row_never_exports() {
        assert!(!variable_is_exportable(&row(serde_json::json!({
            "key": APP_NAME_KEY,
            "sensitive": true,
        }))));
        assert!(!variable_is_exportable(&row(serde_json::json!({
            "key": APP_NAME_KEY,
            "sensitive": 1,
        }))));
    }

    #[test]
    fn a_suffix_flagged_key_never_exports_even_when_the_flag_is_clear() {
        assert!(!variable_is_exportable(&row(serde_json::json!({
            "key": "STRIPE_SECRET",
            "sensitive": false,
        }))));
        assert!(!variable_is_exportable(&row(serde_json::json!({
            "key": "JWT_KEY",
            "sensitive": 0,
        }))));
    }

    /// The irreversible half of the same leak: a row sensitive by DECLARATION
    /// only, stored unflagged by an older build, must not travel inside a seed
    /// bundle to another deployment. Nothing downstream can un-send it.
    #[test]
    fn a_declared_password_key_never_exports_even_when_the_flag_is_clear() {
        let key = crate::blocks::auth::config::BOOTSTRAP_ADMIN_PASSWORD_KEY;
        assert!(
            !crate::config_vars::has_sensitive_suffix(key),
            "the point of this test is a key the suffix rule cannot catch"
        );
        assert!(!variable_is_exportable(&row(serde_json::json!({
            "key": key,
            "sensitive": 0,
        }))));
        assert!(!variable_is_exportable(&row(serde_json::json!({
            "key": key,
            "sensitive": false,
        }))));
    }

    #[test]
    fn an_impresspress_prefixed_key_never_exports_even_when_the_flag_is_clear() {
        assert!(!variable_is_exportable(&row(serde_json::json!({
            "key": REQUEST_LOG_CONFIG_KEY,
            "sensitive": false,
        }))));
    }

    /// The double-underscore half of the prefix rule, pinned on purpose: these
    /// are ordinary database-backed block config, not infrastructure, and they
    /// are held back because they describe THIS instance — see the function's
    /// docs. Asserted separately from the single-underscore case above so that
    /// narrowing the check to infrastructure alone fails here rather than
    /// silently shipping one deployment's origins, sender domain and seed
    /// diagnostics to another.
    #[test]
    fn block_scoped_config_stays_with_the_instance_that_configured_it() {
        for key in [
            CHECKOUT_ALLOWED_ORIGINS,
            PLATFORM_COUNTRY,
            MAILGUN_DOMAIN,
            crate::blocks::dev::seed::SEED_ERROR_KEY,
        ] {
            assert!(
                !variable_is_exportable(&row(serde_json::json!({
                    "key": key,
                    "sensitive": false,
                }))),
                "{key} must not travel into another instance's bundle"
            );
        }
    }

    /// The INTERNAL, adapter-injected class (`__…__`), which the prefix rule
    /// above misses entirely: `__IMPRESSPRESS_RUNTIME_KIND__` starts with `__`,
    /// not `IMPRESSPRESS_`, and ends with `__` rather than a `_SECRET`/`_KEY`
    /// suffix, so it clears both existing checks.
    ///
    /// These are never variables-table config at all — a target's boot code
    /// sets them directly — so a row carrying one is a mistake or a forgery,
    /// and re-exporting it would carry that forgery into the next instance.
    /// `__IMPRESSPRESS_RUNTIME_KIND__` is the one that matters: it is what
    /// keeps Stripe secret-key operations off inside a visitor's browser.
    #[test]
    fn internal_runtime_keys_never_export() {
        for key in [
            "__IMPRESSPRESS_RUNTIME_KIND__",
            "__IMPRESSPRESS_BLOCK_SETTINGS_JSON__",
        ] {
            assert!(
                !variable_is_exportable(&row(serde_json::json!({
                    "key": key,
                    "sensitive": false,
                }))),
                "{key} must not travel into another instance's bundle"
            );
        }
    }

    /// The other half of the same rule: shared config describes the site, not
    /// the instance, so it is what an export is FOR.
    #[test]
    fn shared_config_travels() {
        assert!(variable_is_exportable(&row(serde_json::json!({
            "key": APP_NAME_KEY,
            "sensitive": false,
        }))));
    }

    #[test]
    fn odd_shapes_fail_closed_rather_than_defaulting_to_exportable() {
        // Missing `sensitive` entirely.
        assert!(!variable_is_exportable(&row(serde_json::json!({
            "key": APP_NAME_KEY,
        }))));
        // `sensitive` present but not a clean 0/false shape.
        assert!(!variable_is_exportable(&row(serde_json::json!({
            "key": APP_NAME_KEY,
            "sensitive": "0",
        }))));
        assert!(!variable_is_exportable(&row(serde_json::json!({
            "key": APP_NAME_KEY,
            "sensitive": 0.5,
        }))));
        assert!(!variable_is_exportable(&row(serde_json::json!({
            "key": APP_NAME_KEY,
            "sensitive": null,
        }))));
        // Missing `key` entirely, or `key` not a plain string.
        assert!(!variable_is_exportable(&row(serde_json::json!({
            "sensitive": false,
        }))));
        assert!(!variable_is_exportable(&row(serde_json::json!({
            "key": 12345,
            "sensitive": false,
        }))));
    }
}

/// Read every [`TABLE_ALLOWLIST`] table's rows into a [`DataSnapshot`].
///
/// Exhaustive by requirement, not by preference: the bundle is restored with
/// `Mode::Replace` over the live tables, so a table read only as far as some
/// ceiling would delete every row past it on restore. The cost is that one
/// table's rows are all in memory at once — an export is an operator action on
/// a deployment whose size the operator knows, and a short bundle that looks
/// complete is the worse failure. A snapshot too large to import is refused
/// whole instead: `super::export` checks the serialized snapshot against
/// [`super::seed::MAX_DATA_BYTES`], the bound the importer applies.
pub async fn export(ctx: &dyn Context) -> Result<DataSnapshot, WaferError> {
    let mut tables = BTreeMap::new();
    // The ids each owning table actually exported: the live products, and the
    // offers that survived them. Products are read live-only
    // ([`list_live_products`]), and the tables below hold rows that belong to
    // a product or an offer and mean nothing without it, so they are filtered
    // against these sets rather than read whole — see [`OWNED_TABLES`].
    let mut exported: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    for &(table, _mode) in TABLE_ALLOWLIST {
        // Products alone: read through the repo module's own live-only
        // lister, never the raw table name — see the comment on
        // `TABLE_ALLOWLIST`'s products entry.
        let records = if table == PRODUCTS_COLLECTION {
            list_live_products(ctx, Vec::new()).await?
        } else {
            crate::db_read::list_every(ctx, table, Vec::new()).await?
        };
        let rows: Vec<serde_json::Map<String, Value>> = records
            .into_iter()
            .map(record_to_row)
            .map(|mut row| {
                reset_provider_linkage(table, &mut row);
                row
            })
            // The one table with a per-row export decision — see
            // `variable_is_exportable`'s docs for why the check lives there
            // and not as a second `Mode`.
            //
            // An excluded row is ANNOUNCED. Ad hoc keys are stored sensitive by
            // default (`config_vars::is_sensitive_by_default_when_created`:
            // nothing here knows what an undeclared key holds), and a sensitive
            // row is not exportable — so a bundle legitimately leaves them
            // behind, and an operator who is not told will find out only when
            // the imported site is missing config. Saying which keys did not
            // travel costs one line and is the difference between a decision
            // and a surprise.
            .filter(|row| {
                if table == variables::TABLE && !variable_is_exportable(row) {
                    // Bound outside the macro: `tracing`'s field syntax
                    // resolves `Value` to its own trait, not `serde_json`'s.
                    let key = row
                        .get("key")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("<no key>");
                    tracing::info!(
                        key = %key,
                        "not exporting this config variable: it is sensitive, or names \
                         instance-scoped infrastructure. The imported site will need it set \
                         there"
                    );
                    return false;
                }
                true
            })
            .filter(|row| owner_was_exported(table, row, &exported))
            .collect();
        if OWNED_TABLES.iter().any(|(_, _, owner)| *owner == table) {
            exported.insert(table, ids_of(&rows));
        }
        tables.insert(table.to_string(), rows);
    }
    Ok(DataSnapshot {
        schema_version: SCHEMA_VERSION,
        tables,
    })
}

/// Every allowlisted table whose rows hang off another allowlisted table's
/// row, as `(table, the column naming its owner, the owner's table)`.
///
/// Products are exported live-only, so without this an offer whose product was
/// soft-deleted would travel while its product did not, and land in the
/// imported shop pointing at nothing — as would that offer's components, its
/// typed input variables and its checkout presets, which are two links from
/// the product that orphaned them. Inert (the catalog reads active products)
/// but it is data the export would be carrying without having decided to.
///
/// **This list is closed against the schema.** Every allowlisted table whose
/// migrations declare a `product_id` or `offer_id` column must appear here
/// with that column, and `owned_tables_covers_every_allowlisted_table_with_an_owner_column`
/// (`tests/dev_data_snapshot.rs`) reads the migrations to prove it — the same
/// discipline `every_declared_table_of_the_three_blocks_has_an_export_decision`
/// applies to the allowlist itself. A table added to the allowlist with a
/// dangling reference nobody thought about fails the build rather than
/// exporting orphans.
pub const OWNED_TABLES: &[(&str, &str, &str)] = &[
    (PRODUCT_VERSIONS_TABLE, "product_id", PRODUCTS_COLLECTION),
    (OFFERS_TABLE, "product_id", PRODUCTS_COLLECTION),
    (OFFER_COMPONENTS_TABLE, "offer_id", OFFERS_TABLE),
    (PRODUCTS_VARIABLES_TABLE, "offer_id", OFFERS_TABLE),
    (CHECKOUT_PRESETS_TABLE, "offer_id", OFFERS_TABLE),
];

/// Whether one row's owners are all in the export, for the [`OWNED_TABLES`];
/// every other table's rows pass unconditionally.
///
/// EVERY entry naming `table` has to clear, not the first one found: a table
/// that hangs off two rows (a `product_id` *and* an `offer_id`) is orphaned by
/// either of them going missing, and the schema check above admits exactly
/// that shape.
///
/// An EMPTY owner id is "unowned", and is kept. `variables.offer_id` is
/// `TEXT NOT NULL DEFAULT ''` (products migration 005 adds the column to rows
/// that predate offers entirely), so a blank there is a legitimate state and
/// not evidence of an orphan; the other four declare the column `NOT NULL`
/// with no default, so every row of theirs names a real owner and the rule
/// costs them nothing.
///
/// A row whose owner column is missing or is not a string is dropped: the
/// schema declares all five `TEXT NOT NULL`, so a row this cannot read is one
/// this build does not understand, and carrying it would be exporting a
/// dangling reference on the strength of not having looked.
fn owner_was_exported(
    table: &str,
    row: &serde_json::Map<String, Value>,
    exported: &BTreeMap<&str, BTreeSet<String>>,
) -> bool {
    OWNED_TABLES
        .iter()
        .filter(|(name, _, _)| *name == table)
        .all(|(_, column, owner)| {
            // Unreachable while `TABLE_ALLOWLIST` lists each owner before the
            // tables that hang off it — which
            // `every_owned_table_is_listed_after_its_owner` pins. Reading "no
            // owner exported" from a set that has not been filled yet would
            // silently empty the table, so the miss is treated as "keep the
            // row" and the test is what makes it impossible.
            let Some(ids) = exported.get(owner) else {
                return true;
            };
            match row.get(*column).and_then(Value::as_str) {
                Some("") => true,
                Some(id) => ids.contains(id),
                None => false,
            }
        })
}

/// The `id` of every row in `rows`.
fn ids_of(rows: &[serde_json::Map<String, Value>]) -> BTreeSet<String> {
    rows.iter()
        .filter_map(|row| row.get("id").and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod owned_table_tests {
    use super::*;

    /// [`export`] fills each owner's id set as it reaches that table and
    /// filters the dependent tables against it, so the allowlist's order is
    /// load-bearing: an owner listed after its dependents would leave the set
    /// empty at the moment it is read.
    #[test]
    fn every_owned_table_is_listed_after_its_owner() {
        let position = |table: &str| {
            TABLE_ALLOWLIST
                .iter()
                .position(|(name, _)| *name == table)
                .unwrap_or_else(|| panic!("{table:?} is not on TABLE_ALLOWLIST"))
        };
        for (table, _column, owner) in OWNED_TABLES {
            assert!(
                position(owner) < position(table),
                "{owner:?} must be exported before {table:?}, which is filtered against it"
            );
        }
    }
}

/// A database [`db::Record`] as the JSON object [`DataSnapshot`] stores.
/// `record.data` already carries an `"id"` entry read back off the row (every
/// backend echoes the primary key as an ordinary column), but this sets it
/// explicitly from `record.id` anyway — the field the typed client treats as
/// authoritative should be the one the snapshot is built from, not whatever
/// a backend happened to also put in `data`.
fn record_to_row(record: db::Record) -> serde_json::Map<String, Value> {
    let mut row: serde_json::Map<String, Value> = record.data.into_iter().collect();
    row.insert("id".to_string(), Value::String(record.id));
    row
}

/// Reset one row's provider-linkage columns to their "not yet synced with
/// this destination's own Stripe account" defaults, for the three tables
/// that carry any (`products`, `offers`, `offer_components`).
///
/// The importing instance has no Stripe account the exported ids belong to
/// — carrying them over would point a re-hosted shop's checkout at another
/// deployment's Stripe objects instead of re-creating its own the next time
/// someone syncs. The defaults match exactly what `repo::products::create`
/// and `repo::offers::create` write for a brand-new row (`stripe.rs`'s own
/// "is this synced yet" check is `str_field("stripe_product_id").is_empty()`
/// — the same sentinel this restores); `offer_components.stripe_price_id`
/// carries the identical `TEXT NOT NULL DEFAULT ''` shape (migration
/// `005_commerce_v2`) for the same reason `offers.stripe_price_id` does — a
/// per-component Stripe Price, not a per-offer one.
fn reset_provider_linkage(table: &str, row: &mut serde_json::Map<String, Value>) {
    const EMPTY: &str = "";
    let set = |row: &mut serde_json::Map<String, Value>, key: &str, value: &str| {
        row.insert(key.to_string(), Value::String(value.to_string()));
    };
    if table == PRODUCTS_COLLECTION {
        set(row, "stripe_product_id", EMPTY);
        set(row, "seller_account_id", EMPTY);
    } else if table == OFFERS_TABLE {
        set(row, "stripe_product_id", EMPTY);
        set(row, "stripe_price_id", EMPTY);
        set(row, "sync_status", "not_synced");
        set(row, "sync_error", EMPTY);
    } else if table == OFFER_COMPONENTS_TABLE {
        set(row, "stripe_price_id", EMPTY);
    }
}

/// Rows written per table, keyed by table name — only tables `snapshot`
/// actually carried rows for appear here, so a table the export decided to
/// include but that happened to be empty is present with `0`, and a table
/// the snapshot never mentions at all is simply absent.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ImportReport {
    pub tables: BTreeMap<String, usize>,
}

/// [`Mode::Replace`] tables, in the explicit dependency order [`import`]
/// applies them in: `local_credentials.user_id` and `user_roles.user_id` are
/// meaningful only once the row they name in `users` exists.
///
/// A fixed list rather than the snapshot's own (incidental, alphabetical)
/// `BTreeMap` order — `"impresspress__admin__user_roles"` sorts before
/// `"wafer_run__auth__users"`, which is exactly backwards.
const REPLACE_ORDER: &[&str] = &[users::TABLE, local_credentials::TABLE, user_roles::TABLE];

#[cfg(test)]
mod replace_order_tests {
    use super::*;

    /// The two lists this const and `TABLE_ALLOWLIST`'s `Mode::Replace`
    /// entries form have to agree, in both directions: a table in
    /// `REPLACE_ORDER` that `TABLE_ALLOWLIST` doesn't mark `Replace` is
    /// meaningless (nothing would ever route it through the `Replace` loop
    /// in `import` in the first place), and a `Replace` table missing from
    /// `REPLACE_ORDER` is worse — `import`'s loop below only ever applies
    /// `Mode::Upsert` to a table outside `REPLACE_ORDER`, so a forgotten
    /// entry would silently *upsert* a table meant to be replaced instead of
    /// erroring anywhere.
    #[test]
    fn replace_order_is_exactly_the_allowlists_replace_entries() {
        let allowlist_replace: std::collections::BTreeSet<&str> = TABLE_ALLOWLIST
            .iter()
            .filter(|(_, mode)| *mode == Mode::Replace)
            .map(|(table, _)| *table)
            .collect();
        let replace_order: std::collections::BTreeSet<&str> =
            REPLACE_ORDER.iter().copied().collect();

        for table in &replace_order {
            assert!(
                allowlist_replace.contains(table),
                "{table:?} is in REPLACE_ORDER but is not a Mode::Replace entry on \
                 TABLE_ALLOWLIST"
            );
        }
        for table in &allowlist_replace {
            assert!(
                replace_order.contains(table),
                "{table:?} is a Mode::Replace entry on TABLE_ALLOWLIST but is missing from \
                 REPLACE_ORDER"
            );
        }
        // A duplicate entry in `REPLACE_ORDER` would otherwise pass the two
        // set-membership checks above silently.
        assert_eq!(
            REPLACE_ORDER.len(),
            replace_order.len(),
            "REPLACE_ORDER has a duplicate entry"
        );
    }
}

/// Apply `snapshot`'s rows through typed database writes.
///
/// Every table `snapshot.tables` names must be on [`TABLE_ALLOWLIST`] — see
/// the module docs for why a name outside it is refused (`InvalidArgument`)
/// rather than silently skipped or written anyway.
///
/// The whole import is ONE `db::batch`, so one transaction: all of its
/// writes or none, and one OPFS flush in the browser. Each `Replace` table is
/// a filtered delete of every row (`BatchWrite::DeleteWhere` with no filters)
/// followed by a `Create` per snapshot row, in [`REPLACE_ORDER`]; then every
/// `Upsert` row of every other table. A write that fails (a duplicate key, a
/// bad row) rolls all of it back, so a failed import leaves the users, their
/// credentials and every other table as they were.
///
/// The database handler admits the batch, one statement per op, against the
/// backend's statement budget before anything runs. SQLite and PostgreSQL
/// take any size. On Cloudflare D1 an import larger than what the request has
/// left is refused whole (`ResourceExhausted`, or `InvalidArgument` past the
/// per-invocation limit) and nothing is written: a smaller bundle, or a
/// raised `IMPRESSPRESS_D1_QUERIES_PER_INVOCATION` on a plan that allows it,
/// is the fix, not a retry.
///
/// Every write is keyed on the snapshot's own row ids, so importing the same
/// snapshot again converges to the same end state —
/// `tests/dev_data_snapshot.rs`'s
/// `import_replaces_users_and_upserts_products_so_ownership_survives` test
/// re-imports and asserts no duplication.
pub async fn import(
    ctx: &dyn Context,
    snapshot: &DataSnapshot,
) -> Result<ImportReport, WaferError> {
    if snapshot.schema_version != SCHEMA_VERSION {
        return Err(WaferError::new(
            ErrorCode::InvalidArgument,
            format!(
                "the data snapshot declares schema_version {}; this build reads {SCHEMA_VERSION}",
                snapshot.schema_version
            ),
        ));
    }
    for table in snapshot.tables.keys() {
        if !TABLE_ALLOWLIST.iter().any(|(name, _)| name == table) {
            return Err(WaferError::new(
                ErrorCode::InvalidArgument,
                format!(
                    "the data snapshot names {table:?}, which is not on this build's export \
                     allowlist"
                ),
            ));
        }
    }

    // Third pre-flight pass, for the same reason as the two above: refuse
    // before writing anything, so a bundle naming one forged key imports none
    // of its rows rather than most of them and failing partway.
    //
    // `variables::TABLE` is the one allowlisted table whose rows carry a NAME
    // the runtime reserves. `admin::ops::reject_runtime_owned_key` refuses
    // these on the admin write path and `ui::settings_form`'s `CONFIG_SET`
    // refuses them too; this import reaches the same table through
    // `db::upsert`, so without this pass it is the one way in.
    //
    // The set is `is_instance_owned_key`, not the narrower
    // `is_runtime_owned_key`, and the difference is the dangerous case. A
    // planted INFRASTRUCTURE or INTERNAL row is inert for reads —
    // `blocks::config`'s `served_only_from_boot_map` answers those from the
    // boot map whatever the table holds (PR #65) — so it is only a forgery
    // that shows on the admin Variables page and travels into the next
    // export. The JWT SECRET is not inert: `seed_jwt_secret` writes through
    // `insert_if_absent`, so a row already present wins and auto-generation
    // never fires, and boot signs every session JWT and CSRF token with it.
    // A bundle shared between instances would give each one a signing secret
    // its author knows. Nothing legitimate carries either class — the secret
    // is unexportable twice over (`_SECRET` suffix, and the `sensitive` flag
    // the seeder sets) — so refusing them costs no real bundle.
    //
    // A row with no `key`, or a non-string one, is not judged here: no such
    // value can spell a reserved name, and the upsert's `["key"]` conflict
    // target is what refuses it.
    if let Some(rows) = snapshot.tables.get(variables::TABLE) {
        for row in rows {
            let Some(key) = row.get("key").and_then(Value::as_str) else {
                continue;
            };
            if crate::config_vars::is_instance_owned_key(key) {
                return Err(WaferError::new(
                    ErrorCode::InvalidArgument,
                    format!(
                        "the data snapshot carries the variable {key:?}, whose value this \
                         instance owns; a seed bundle may not set it"
                    ),
                ));
            }
        }
    }

    let mut report = ImportReport::default();
    let mut writes = Vec::new();
    // `Replace` tables first, in `REPLACE_ORDER` — not the snapshot's own
    // alphabetical order. `Upsert` tables carry no such dependency (every
    // foreign id they reference — `product_id`, `offer_id` — is validated by
    // the owning handler at write time, never enforced by this import), so
    // the remaining loop below keeps the snapshot's own order.
    for &table in REPLACE_ORDER {
        let Some(rows) = snapshot.tables.get(table) else {
            continue;
        };
        let rows = if table == user_roles::TABLE {
            one_grant_per_user_and_role(rows)
        } else {
            rows.iter().collect()
        };
        writes.push(BatchWrite::DeleteWhere {
            collection: table.to_string(),
            filters: Vec::new(),
        });
        report.tables.insert(table.to_string(), rows.len());
        writes.extend(rows.into_iter().map(|row| BatchWrite::Create {
            collection: table.to_string(),
            data: imported_row(table, row).into_iter().collect(),
        }));
    }
    for (table, rows) in &snapshot.tables {
        if REPLACE_ORDER.contains(&table.as_str()) {
            continue; // already queued above, in dependency order
        }
        // The mode comes from the allowlist rather than being assumed: it
        // carries the table's conflict target, and every remaining entry is
        // an `Upsert` (each `Mode::Replace` table is named in
        // `REPLACE_ORDER`). The loop above already refused any table not on
        // the list, so a lookup miss here is unreachable — and is reported
        // rather than defaulted, because defaulting to `BY_ID` is precisely
        // the assumption this field exists to stop making.
        let Some((_, Mode::Upsert(conflict))) =
            TABLE_ALLOWLIST.iter().find(|(name, _)| name == table)
        else {
            return Err(WaferError::new(
                ErrorCode::Internal,
                format!("{table:?} passed the allowlist check but has no upsert conflict target"),
            ));
        };
        writes.extend(rows.iter().map(|row| upsert_op(table, conflict, row)));
        report.tables.insert(table.clone(), rows.len());
    }
    if !writes.is_empty() {
        // The per-op results (each created row as stored) are not needed: the
        // report counts the snapshot's rows, which is what was written.
        db::batch(ctx, writes).await?;
    }
    Ok(report)
}

/// A snapshot's grant rows with every repeat of a `(user_id, role)` pair
/// dropped, in the snapshot's own order otherwise.
///
/// A bundle exported before admin migration 004 can carry twin grants, and
/// the destination's unique index over the pair would refuse the second —
/// failing the whole import. The twin
/// grants nothing its survivor does not, so it is dropped rather than
/// refused. The survivor is the least `(created_at, id)`, the pair 004 ranks
/// by, compared bytewise so the choice does not depend on row order.
fn one_grant_per_user_and_role(
    rows: &[serde_json::Map<String, Value>],
) -> Vec<&serde_json::Map<String, Value>> {
    let text = |row: &serde_json::Map<String, Value>, field: &str| {
        row.get(field)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let mut survivor: HashMap<(String, String), usize> = HashMap::new();
    for (index, row) in rows.iter().enumerate() {
        let rank = |i: usize| (text(&rows[i], "created_at"), text(&rows[i], "id"));
        survivor
            .entry((text(row, "user_id"), text(row, "role")))
            .and_modify(|kept| {
                if rank(index) < rank(*kept) {
                    *kept = index;
                }
            })
            .or_insert(index);
    }
    let kept: std::collections::HashSet<usize> = survivor.into_values().collect();
    rows.iter()
        .enumerate()
        .filter(|(index, _)| kept.contains(index))
        .map(|(_, row)| row)
        .collect()
}

/// Raise an imported `impresspress__admin__variables` row's `sensitive` column
/// to what its key requires, exactly as
/// [`crate::platform_state::variables::NewVariable::into_row`] does for every
/// row this instance creates itself.
///
/// Import is the one write to this table that does NOT go through that funnel —
/// it upserts the bundle's own columns straight through `db::batch` — and its
/// pre-flight refuses only [`crate::config_vars::is_instance_owned_key`]. So a
/// bundle carrying `WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_PASSWORD` with
/// `sensitive: 0` would re-create exactly the row this masking work exists to
/// prevent: served in the clear by `GET /b/admin/api/settings/{key}` and
/// KV-cacheable, until the next `/_deploy/init` or native boot happened to run
/// the repair pass.
///
/// Raise-only, like the funnel: a bundle may mark a row sensitive that this
/// build does not know to be, and that stands.
fn raise_imported_sensitive_flag(row: &mut serde_json::Map<String, Value>) {
    let Some(key) = row.get("key").and_then(Value::as_str) else {
        return;
    };
    if !crate::config_vars::is_sensitive_for_storage(key) {
        return;
    }
    // Write the canonical `1` UNCONDITIONALLY rather than returning early on a
    // truthy-looking value. The early return used `RecordExt::bool_field`,
    // which accepts `2`, `true` and `"true"` — shapes a bundle can carry and
    // the declared `INTEGER` column does not use — so such a row imported
    // unchanged. This is the one write path that accepts foreign data, so it
    // is the one that must not trust the sender's spelling.
    if row.get("sensitive") != Some(&serde_json::json!(1)) {
        tracing::warn!(
            key = %key,
            "the imported data snapshot did not mark this config key sensitive in the \
             canonical form; storing it sensitive, as its declaration requires"
        );
    }
    row.insert("sensitive".to_string(), serde_json::json!(1));
}

/// Neutralise an imported variables row's `updated_by` on INSERT, without ever
/// writing it over a row this instance already has.
///
/// That column is the local admin-ownership marker
/// ([`crate::platform_state::variables::is_pinned`]): non-empty means an
/// admin HERE edited the row, which is what makes it outrank the process
/// environment. It has to be protected from a bundle in BOTH directions, and
/// blanking the value alone only covered one of them:
///
/// - A bundle must not CLAIM ownership here. An admin on the exporting
///   instance is not an admin on this one, and a verbatim copy would let a seed
///   bundle silently pin keys against this deployment's own `.env` while the
///   boot log blamed an edit that never happened here. Blanking the value in
///   the row handles this: a newly inserted row arrives seeder-owned, so the
///   local environment can still seed it.
/// - A bundle must not REVOKE ownership here either — the case the first
///   version of this missed. On `Mode::Upsert` the bundle's columns are written
///   over the destination's, so a blank would erase a marker a local admin had
///   set and hand their key back to the local `.env`. That is why
///   `updated_by` is dropped from the update column set in [`upsert_op`]
///   rather than merely blanked: on a conflict the destination keeps its own.
fn neutralise_imported_owner(row: &mut serde_json::Map<String, Value>) {
    row.insert("updated_by".to_string(), serde_json::json!(""));
}

/// `row` as it is written into `table`: unchanged, except that a variables
/// row — the table whose columns carry a security decision, and the one
/// import writes without passing through `NewVariable::into_row` — has its
/// `sensitive` flag raised and its admin-ownership marker neutralised.
fn imported_row(
    table: &str,
    row: &serde_json::Map<String, Value>,
) -> serde_json::Map<String, Value> {
    let mut row = row.clone();
    if table == variables::TABLE {
        raise_imported_sensitive_flag(&mut row);
        neutralise_imported_owner(&mut row);
    }
    row
}

/// The `db::batch` write that upserts one snapshot `row` into `table` on
/// `conflict`.
fn upsert_op(table: &str, conflict: &[&str], row: &serde_json::Map<String, Value>) -> BatchWrite {
    let row = imported_row(table, row);
    // Neither `id` nor the conflict columns are updated on a
    // conflict. The conflict columns are equal by definition (that is
    // what conflicted), and `id` must stay the DESTINATION's: an
    // import that rewrote it would break every row already pointing
    // at it — a `user_roles.role_id`, say — to graft on an id whose
    // only merit is that another instance happened to mint it.
    //
    // `variables.updated_by` is excluded for a related reason: it is
    // the DESTINATION's admin-ownership marker, so writing the
    // bundle's over it on a conflict would revoke a local admin's
    // claim and hand their key back to the local `.env`. Excluded
    // rather than blanked — a blank is still a write — so a row this
    // instance already has keeps whatever it had. See
    // `neutralise_imported_owner`, which covers the insert direction.
    let update_columns: Vec<String> = row
        .keys()
        .filter(|key| key.as_str() != "id" && !conflict.contains(&key.as_str()))
        .filter(|key| !(table == variables::TABLE && key.as_str() == "updated_by"))
        .cloned()
        .collect();
    let data: Vec<(String, Value)> = row.into_iter().collect();
    let conflict: Vec<String> = conflict.iter().map(|c| (*c).to_string()).collect();
    // Products alone: built by the repo module's own wholesale-upsert
    // door, never on the raw table name — see the comment on
    // `TABLE_ALLOWLIST`'s products entry. The door takes the conflict
    // target the allowlist declared, exactly as the generic op below does,
    // so there is one statement of it.
    if table == PRODUCTS_COLLECTION {
        return product_snapshot_upsert(data, conflict, update_columns);
    }
    BatchWrite::Upsert(UpsertRequest {
        collection: table.to_string(),
        data,
        conflict_columns: conflict,
        on_conflict: OnConflict::SetColumns(update_columns),
    })
}
