//! The data snapshot: the export allowlist's coverage, what it filters out,
//! and the typed round trip through `seed::import`.
//!
//! Gated on `block-dev` for the same reason `dev_seed.rs` is: the block does
//! not exist in a default-feature build, so these tests must not compile
//! there.
//!
//! `PRODUCTS_TABLE`/`OFFERS_TABLE`/`PURCHASES_TABLE`
//! below restate table names `impresspress-core` keeps `pub(crate)` (the
//! products ones are further gated by that block's own door tests —
//! `blocks/products/tests/repo_door_test.rs` — which refuse a re-export
//! anywhere reachable from outside `repo::products`). That module owns the
//! literal strings; this is a separate compilation unit and cannot reach
//! them, so it restates the ones it needs — the same choice
//! `tests/dev_blocks.rs` already makes for `"impresspress__products__products"`.
#![cfg(feature = "block-dev")]

use std::collections::BTreeSet;

use impresspress_core::{
    blocks::{
        admin::AdminBlock,
        auth::repo::{local_credentials, users},
        dev::{
            self,
            data_snapshot::{self, DataSnapshot},
            seed::{self, SeedManifest},
            test_support::{seed_file, FakeControl, MapFetch},
        },
        products::ProductsBlock,
    },
    platform_state::{user_roles, variables},
    test_support::{TestContext, WriteLog},
    util::json_map,
};
use serde_json::json;
use wafer_core::{
    clients::database as db,
    interfaces::database::service::{DatabaseError, DatabaseService, StatementBudget},
};
use wafer_run::Block;

// ---------------------------------------------------------------------------
// Table names this crate keeps private, restated here — see the module docs.
// ---------------------------------------------------------------------------

const PRODUCTS_TABLE: &str = "impresspress__products__products";
const OFFERS_TABLE: &str = "impresspress__products__offers";
const OFFER_COMPONENTS_TABLE: &str = "impresspress__products__offer_components";
const PRODUCT_VERSIONS_TABLE: &str = "impresspress__products__product_versions";
const CHECKOUT_PRESETS_TABLE: &str = "impresspress__products__checkout_presets";
const PRODUCTS_VARIABLES_TABLE: &str = "impresspress__products__variables";
const PURCHASES_TABLE: &str = "impresspress__products__purchases";

// ---------------------------------------------------------------------------
// Coverage: every declared table has a decision.
// ---------------------------------------------------------------------------

/// Every table name created by a `CREATE TABLE [IF NOT EXISTS] <name>`
/// statement anywhere under the three blocks' own migration directories —
/// the ground truth of what tables actually exist, read straight off the
/// `.sql` files rather than off any Rust-side declaration of them.
///
/// This is why the coverage test below does not need (and, for auth, has no
/// other way to get) a `BlockInfo.collections`-style list: `BlockInfo`'s own
/// list is advisory (see the comment on `products/mod.rs`'s `.collections`
/// call) and, as this test caught once already, can fall behind a table a
/// migration created — `impresspress__products__stripe_events` (migration
/// `003_stripe_events`) was never added to `ProductsBlock::info().collections`
/// at all. Scanning the migrations directly cannot have that gap: a table
/// with no `CREATE TABLE` here does not exist, and one that exists has no
/// way to hide from this scan the way it could from a hand-kept list.
fn tables_created_in_migrations() -> BTreeSet<String> {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut tables = BTreeSet::new();
    for block in ["products", "admin", "auth"] {
        let dir = manifest_dir
            .join("src/blocks")
            .join(block)
            .join("migrations");
        let entries =
            std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));
        for entry in entries {
            let path = entry.expect("dir entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("sql") {
                continue;
            }
            let sql = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
            for line in sql.lines() {
                let Some(rest) = line.trim_start().strip_prefix("CREATE TABLE") else {
                    continue;
                };
                let rest = rest.trim_start();
                let rest = rest
                    .strip_prefix("IF NOT EXISTS")
                    .map_or(rest, str::trim_start);
                let name: String = rest
                    .chars()
                    .take_while(|c| !c.is_whitespace() && *c != '(')
                    .collect();
                assert!(
                    !name.is_empty(),
                    "{}: a `CREATE TABLE` line with no table name: {line:?}",
                    path.display()
                );
                tables.insert(name);
            }
        }
    }
    tables
}

#[test]
fn every_declared_table_of_the_three_blocks_has_an_export_decision() {
    let mut declared: BTreeSet<String> = tables_created_in_migrations();
    // Cross-checked, not replaced, against the two feature blocks' own
    // advisory `BlockInfo.collections` — a table one of them lists that no
    // migration actually creates would be exactly the "declared but not
    // real" mirror image of the `stripe_events` gap the migration scan
    // exists to catch, and belongs in this failure too.
    for info in [ProductsBlock::new().info(), AdminBlock::new().info()] {
        declared.extend(info.collections.iter().map(|c| c.name.clone()));
    }
    assert!(
        declared.len() > 40,
        "the migration scan found {} tables — it lost its way",
        declared.len()
    );

    let decided: BTreeSet<&str> = data_snapshot::TABLE_ALLOWLIST
        .iter()
        .map(|(table, _)| *table)
        .chain(data_snapshot::TABLE_EXCLUDED.iter().copied())
        .collect();
    let undecided: Vec<&String> = declared
        .iter()
        .filter(|table| !decided.contains(table.as_str()))
        .collect();
    assert!(
        undecided.is_empty(),
        "tables with no export decision: {undecided:?} — add each to TABLE_ALLOWLIST or \
         TABLE_EXCLUDED deliberately"
    );
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// `ctx` running as `impresspress/dev`, the block whose code the snapshot
/// is, in a deployment carrying the grants its consumer hands
/// `ImpresspressBuilder::wrap_grants` ([`dev::wrap_grants`]). What the
/// export reads and the import writes is authorized against exactly those.
fn as_dev(ctx: &TestContext) -> TestContext {
    let mut dev = ctx.fixture();
    dev.add_deployment_grants(dev::wrap_grants());
    dev.running_as(dev::BLOCK_NAME)
}

/// Insert one row directly, honoring the supplied `id` — the same
/// direct-write pattern `blocks::products::tests::harness::seed` uses inside
/// the crate, reimplemented here because that helper is `#[cfg(test)]`
/// private to `impresspress-core` and this file is a separate crate.
async fn seed_row(ctx: &TestContext, table: &str, id: &str, data: serde_json::Value) {
    let mut map = json_map(data);
    map.insert("id".to_string(), serde_json::Value::String(id.to_string()));
    db::create(&ctx.fixture(), table, map)
        .await
        .unwrap_or_else(|e| panic!("seed into {table} failed: {} ({:?})", e.message, e.code));
}

/// One active product with an offer, one purchase against it, and one
/// sensitive admin variable alongside a non-sensitive one. Returns the
/// seeded owner's user id.
async fn seed_product_and_order(ctx: &TestContext) -> String {
    let user_id = "user_owner".to_string();
    seed_row(
        ctx,
        users::TABLE,
        &user_id,
        json!({
            "email": "owner@example.com",
            "display_name": "Owner",
            "role": "user",
            "email_verified": true,
        }),
    )
    .await;

    seed_row(
        ctx,
        PRODUCTS_TABLE,
        "prod_widget",
        json!({
            "name": "Widget",
            "status": "active",
            "created_by": user_id,
            // Provider-linkage columns, deliberately non-default here:
            // `export_carries_products_but_never_secrets_or_orders` asserts
            // these come back reset (see `data_snapshot::reset_provider_linkage`).
            "stripe_product_id": "prod_stripe_source_owns_this",
            "seller_account_id": "seller_source_owns_this",
        }),
    )
    .await;

    seed_row(
        ctx,
        OFFERS_TABLE,
        "offer_standard",
        json!({
            "product_id": "prod_widget",
            "name": "Standard",
            "stripe_product_id": "prod_stripe_source_owns_this",
            "stripe_price_id": "price_source_owns_this",
            "sync_status": "synced",
        }),
    )
    .await;

    seed_row(
        ctx,
        OFFER_COMPONENTS_TABLE,
        "component_base",
        json!({
            "offer_id": "offer_standard",
            "component_key": "base",
            "label": "Base",
            "stripe_price_id": "price_component_source_owns_this",
        }),
    )
    .await;

    seed_row(
        ctx,
        PURCHASES_TABLE,
        "purchase_1",
        json!({ "user_id": user_id }),
    )
    .await;

    seed_row(
        ctx,
        user_roles::TABLE,
        "role_owner_admin",
        json!({ "user_id": user_id, "role": "admin" }),
    )
    .await;

    // Never exported: the `sensitive` flag is set.
    seed_row(
        ctx,
        variables::TABLE,
        "var_secret",
        json!({
            "key": "WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_PASSWORD",
            "value": "hunter2",
            "sensitive": true,
        }),
    )
    .await;
    // Exported: ordinary site config.
    seed_row(
        ctx,
        variables::TABLE,
        "var_public",
        json!({
            "key": "WAFER_RUN_SHARED__APP_NAME",
            "value": "Acme Shop",
            "sensitive": false,
        }),
    )
    .await;
    // Never exported: `IMPRESSPRESS_`-prefixed, even with a clear flag —
    // CLAUDE.md reserves that prefix for infrastructure config.
    seed_row(
        ctx,
        variables::TABLE,
        "var_infra",
        json!({
            "key": "IMPRESSPRESS_INTERNAL_FLAG",
            "value": "true",
            "sensitive": false,
        }),
    )
    .await;

    user_id
}

// ---------------------------------------------------------------------------
// export()
// ---------------------------------------------------------------------------

#[tokio::test]
async fn export_carries_products_but_never_secrets_or_orders() {
    let ctx = TestContext::with_products()
        .await
        .fixture()
        .with_auth_added()
        .await;
    seed_product_and_order(&ctx).await;

    let snap = data_snapshot::export(&as_dev(&ctx)).await.unwrap();

    assert_eq!(snap.tables[PRODUCTS_TABLE].len(), 1);
    assert!(!snap.tables.contains_key(PURCHASES_TABLE));

    let vars = &snap.tables[variables::TABLE];
    assert_eq!(
        vars.len(),
        1,
        "the sensitive and IMPRESSPRESS_-prefixed variables are filtered out at export"
    );
    assert!(vars.iter().all(|v| v["sensitive"] != json!(true)
        && !v["key"].as_str().unwrap().starts_with("IMPRESSPRESS_")));
    assert!(!vars
        .iter()
        .any(|v| v["key"] == "WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_PASSWORD"));
    // The vacuous half of the check above, made real: the row really was
    // seeded, and really is absent, not merely never present to begin with.
    assert!(!vars
        .iter()
        .any(|v| v["key"] == "IMPRESSPRESS_INTERNAL_FLAG"));

    // Provider-linkage columns come back reset — this destination has no
    // Stripe account the source's ids belong to.
    let product = &snap.tables[PRODUCTS_TABLE][0];
    assert_eq!(product["stripe_product_id"], json!(""));
    assert_eq!(product["seller_account_id"], json!(""));
    let offer = &snap.tables[OFFERS_TABLE][0];
    assert_eq!(offer["stripe_product_id"], json!(""));
    assert_eq!(offer["stripe_price_id"], json!(""));
    assert_eq!(offer["sync_status"], json!("not_synced"));
    assert_eq!(offer["sync_error"], json!(""));
    let component = &snap.tables[OFFER_COMPONENTS_TABLE][0];
    assert_eq!(component["stripe_price_id"], json!(""));
}

/// Products are read live-only, and everything that hangs off a product has
/// to be read the same way or the export carries rows pointing at nothing.
///
/// A soft-deleted product with an offer is the shape that breaks it: the
/// product is filtered out by `list_live_products`, while `offers`,
/// `product_versions` and — two links down — `offer_components`, `variables`
/// and `checkout_presets` were read whole. The imported shop then holds an
/// offer whose `product_id` names no row, and inputs and presets naming that
/// offer — inert (the catalog reads active products) but data the export
/// never decided to carry.
///
/// A variable with a BLANK `offer_id` is the deliberate exception and is
/// asserted here too: that column is `NOT NULL DEFAULT ''`, so an unowned
/// variable is a legitimate row and not an orphan.
#[tokio::test]
async fn a_soft_deleted_products_offers_and_versions_do_not_travel() {
    let ctx = TestContext::with_products()
        .await
        .fixture()
        .with_auth_added()
        .await;
    seed_product_and_order(&ctx).await;

    // A second product, its own offer, that offer's component and a version
    // row — then the product is soft-deleted, exactly as
    // `DELETE /b/products/api/admin/products/{id}` leaves it.
    seed_row(
        &ctx,
        PRODUCTS_TABLE,
        "prod_retired",
        json!({ "name": "Retired", "status": "active" }),
    )
    .await;
    seed_row(
        &ctx,
        OFFERS_TABLE,
        "offer_retired",
        json!({ "product_id": "prod_retired", "name": "Retired" }),
    )
    .await;
    seed_row(
        &ctx,
        OFFER_COMPONENTS_TABLE,
        "component_retired",
        json!({
            "offer_id": "offer_retired",
            "component_key": "base",
            "label": "Base",
        }),
    )
    .await;
    seed_row(
        &ctx,
        PRODUCT_VERSIONS_TABLE,
        "version_retired",
        json!({ "product_id": "prod_retired", "version": 1 }),
    )
    .await;
    seed_row(
        &ctx,
        PRODUCT_VERSIONS_TABLE,
        "version_live",
        json!({ "product_id": "prod_widget", "version": 1 }),
    )
    .await;
    // Two links from the product: a preset and a typed input on the retired
    // offer, the same pair on the live one, and one variable owned by no
    // offer at all.
    for (id, offer) in [
        ("preset_retired", "offer_retired"),
        ("preset_live", "offer_standard"),
    ] {
        seed_row(
            &ctx,
            CHECKOUT_PRESETS_TABLE,
            id,
            json!({ "offer_id": offer, "name": "Preset", "slug": id }),
        )
        .await;
    }
    for (id, offer) in [
        ("var_retired", "offer_retired"),
        ("var_live", "offer_standard"),
    ] {
        seed_row(
            &ctx,
            PRODUCTS_VARIABLES_TABLE,
            id,
            json!({ "offer_id": offer, "name": "pages", "var_type": "number" }),
        )
        .await;
    }
    seed_row(
        &ctx,
        PRODUCTS_VARIABLES_TABLE,
        "var_global",
        json!({ "offer_id": "", "name": "legacy", "var_type": "number" }),
    )
    .await;
    db::update(
        &ctx,
        PRODUCTS_TABLE,
        "prod_retired",
        json_map(json!({ "deleted_at": "2026-09-01T00:00:00Z" })),
    )
    .await
    .expect("soft-delete the second product");

    let snap = data_snapshot::export(&as_dev(&ctx)).await.unwrap();

    // Sorted: which rows a table carries is the subject, and their order is
    // the database's listing order rather than anything this asserts.
    let ids = |table: &str| -> Vec<String> {
        let mut ids: Vec<String> = snap.tables[table]
            .iter()
            .map(|row| row["id"].as_str().unwrap_or_default().to_string())
            .collect();
        ids.sort();
        ids
    };
    assert_eq!(ids(PRODUCTS_TABLE), vec!["prod_widget"]);
    // The live product's rows all travel…
    assert_eq!(ids(OFFERS_TABLE), vec!["offer_standard"]);
    assert_eq!(ids(OFFER_COMPONENTS_TABLE), vec!["component_base"]);
    assert_eq!(ids(PRODUCT_VERSIONS_TABLE), vec!["version_live"]);
    assert_eq!(ids(CHECKOUT_PRESETS_TABLE), vec!["preset_live"]);
    // …the unowned variable travels beside the live offer's own…
    assert_eq!(
        ids(PRODUCTS_VARIABLES_TABLE),
        vec!["var_global", "var_live"]
    );
    // …and the retired product's rows — including the three that are two
    // links from the product that orphaned them — travel with it or not at
    // all.
    for (table, id) in [
        (OFFERS_TABLE, "offer_retired"),
        (OFFER_COMPONENTS_TABLE, "component_retired"),
        (PRODUCT_VERSIONS_TABLE, "version_retired"),
        (CHECKOUT_PRESETS_TABLE, "preset_retired"),
        (PRODUCTS_VARIABLES_TABLE, "var_retired"),
    ] {
        assert!(
            !ids(table).contains(&id.to_string()),
            "{id} travelled in {table} without the product it belongs to"
        );
    }
}

// ---------------------------------------------------------------------------
// import()
// ---------------------------------------------------------------------------

#[tokio::test]
async fn import_replaces_users_and_upserts_products_so_ownership_survives() {
    let src = TestContext::with_products()
        .await
        .fixture()
        .with_auth_added()
        .await;
    let admin_id = seed_product_and_order(&src).await;

    let snap = data_snapshot::export(&as_dev(&src)).await.unwrap();
    assert_eq!(
        snap.tables[users::TABLE][0]["id"].as_str().unwrap(),
        admin_id
    );

    // Fresh: a different context, its own (differently-id'd) rows if it had
    // any — here, none at all, which is the more common "first import"
    // shape than a context that already has a bootstrap admin.
    let dst = TestContext::with_products()
        .await
        .fixture()
        .with_auth_added()
        .await;
    seed_row(
        &dst,
        users::TABLE,
        "user_fresh_bootstrap",
        json!({ "email": "fresh@example.com", "display_name": "Fresh Admin" }),
    )
    .await;
    seed_row(
        &dst,
        user_roles::TABLE,
        "role_fresh_bootstrap",
        json!({ "user_id": "user_fresh_bootstrap", "role": "admin" }),
    )
    .await;

    let report = data_snapshot::import(&as_dev(&dst), &snap).await.unwrap();
    assert_eq!(report.tables[users::TABLE], 1);
    assert_eq!(report.tables[user_roles::TABLE], 1);

    let users_rows = db::list_all(&dst, users::TABLE, Vec::new()).await.unwrap();
    assert_eq!(
        users_rows.len(),
        1,
        "replace semantics: the fresh bootstrap admin is gone"
    );
    assert_eq!(users_rows[0].id, admin_id);

    let role_rows = db::list_all(&dst, user_roles::TABLE, Vec::new())
        .await
        .unwrap();
    assert_eq!(
        role_rows.len(),
        1,
        "replace semantics: the fresh bootstrap admin's role assignment is gone too"
    );
    assert_eq!(role_rows[0].data["user_id"], json!(admin_id));

    let products = db::list_all(&dst, PRODUCTS_TABLE, Vec::new())
        .await
        .unwrap();
    assert_eq!(products.len(), 1);
    assert_eq!(products[0].data["created_by"], json!(admin_id));

    // Importing again is idempotent.
    data_snapshot::import(&as_dev(&dst), &snap).await.unwrap();
    assert_eq!(
        db::list_all(&dst, PRODUCTS_TABLE, Vec::new())
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        db::list_all(&dst, users::TABLE, Vec::new())
            .await
            .unwrap()
            .len(),
        1
    );
}

/// A bundle exported before admin migration 004 can repeat a grant; the
/// destination's unique index over `(user_id, role)` would refuse the twin
/// and fail the whole import. The twin is dropped instead, keeping the same
/// survivor 004 keeps.
#[tokio::test]
async fn import_collapses_twin_grants_from_a_pre_004_bundle() {
    let ctx = TestContext::with_products()
        .await
        .fixture()
        .with_auth_added()
        .await;
    let grant = |id: &str, user: &str, role: &str, at: &str| {
        json_map(json!({
            "id": id, "user_id": user, "role": role, "assigned_by": "",
            "created_at": at, "updated_at": at,
        }))
        .into_iter()
        .collect::<serde_json::Map<String, serde_json::Value>>()
    };
    let mut snap = DataSnapshot {
        schema_version: data_snapshot::SCHEMA_VERSION,
        tables: std::collections::BTreeMap::new(),
    };
    snap.tables.insert(
        user_roles::TABLE.to_string(),
        vec![
            grant("ur_later", "alice", "admin", "2026-02-01T00:00:00Z"),
            grant("ur_first", "alice", "admin", "2026-01-01T00:00:00Z"),
            grant("ur_other", "alice", "editor", "2026-03-01T00:00:00Z"),
        ],
    );

    let report = data_snapshot::import(&as_dev(&ctx), &snap)
        .await
        .expect("a bundle repeating a grant still imports");
    assert_eq!(report.tables[user_roles::TABLE], 2);
    let mut ids: Vec<String> = db::list_all(&ctx, user_roles::TABLE, Vec::new())
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect();
    ids.sort();
    assert_eq!(ids, vec!["ur_first", "ur_other"]);
}

#[tokio::test]
async fn import_refuses_a_table_outside_this_builds_allowlist() {
    let ctx = TestContext::with_products().await.fixture();
    let mut snap = DataSnapshot {
        schema_version: data_snapshot::SCHEMA_VERSION,
        tables: std::collections::BTreeMap::new(),
    };
    snap.tables.insert(
        "impresspress__products__stripe_events".to_string(),
        vec![json_map(json!({ "id": "evt_1" })).into_iter().collect()],
    );
    let err = data_snapshot::import(&as_dev(&ctx), &snap)
        .await
        .unwrap_err();
    assert_eq!(err.code, wafer_run::ErrorCode::InvalidArgument);
}

#[tokio::test]
async fn import_refuses_a_schema_version_this_build_does_not_read() {
    let ctx = TestContext::with_products().await.fixture();
    let snap = DataSnapshot {
        schema_version: data_snapshot::SCHEMA_VERSION + 1,
        tables: std::collections::BTreeMap::new(),
    };
    let err = data_snapshot::import(&as_dev(&ctx), &snap)
        .await
        .unwrap_err();
    assert_eq!(err.code, wafer_run::ErrorCode::InvalidArgument);
}

/// Import is the ONE write to the variables table that does not pass through
/// `NewVariable::into_row`: it upserts the bundle's own columns straight
/// through `db::upsert`, and its pre-flight refuses only
/// `is_instance_owned_key`. So a bundle understating a `sensitive` column
/// re-creates exactly the row the flag work exists to prevent — served in the
/// clear by `GET /b/admin/api/settings/{key}` and KV-cacheable — until some
/// later boot happens to run the repair pass.
///
/// Corrected rather than refused: an understated flag is far more likely a
/// bundle built by an older build than an attack, and refusing the whole
/// import would make every such bundle unusable.
#[tokio::test]
async fn import_raises_a_sensitive_flag_the_bundle_understated() {
    // A declared `InputType::Password` var, spelled with neither `_SECRET` nor
    // `_KEY` — so only its declaration knows it holds a credential, and only
    // the stored `sensitive` flag can carry that to the read path.
    let key = "WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_PASSWORD";
    let ctx = TestContext::with_products().await.fixture();
    let mut tables = std::collections::BTreeMap::new();
    tables.insert(
        variables::TABLE.to_string(),
        vec![json_map(json!({
            "id": "var_imported",
            "key": key,
            "value": "hunter2",
            "sensitive": false,
            "created_at": STAMP,
            "updated_at": STAMP,
        }))
        .into_iter()
        .collect()],
    );
    let snap = DataSnapshot {
        schema_version: data_snapshot::SCHEMA_VERSION,
        tables,
    };

    data_snapshot::import(&as_dev(&ctx), &snap)
        .await
        .expect("an understated flag is corrected, not refused");

    let vars = db::list_all(&ctx, variables::TABLE, Vec::new())
        .await
        .unwrap();
    let row = vars
        .iter()
        .find(|v| v.data["key"] == json!(key))
        .expect("the row was imported");
    assert_eq!(
        row.data["sensitive"],
        json!(1),
        "import is the one write that bypasses `NewVariable::into_row`, so it has to \
         apply the same rule itself: {row:?}"
    );
}

/// An imported row must not arrive claiming to be an admin edit of THIS
/// instance.
///
/// `updated_by` is the admin-ownership marker that makes a row outrank the
/// process environment. A bundle carries the EXPORTING instance's column, and
/// an admin over there is not an admin over here — importing it verbatim would
/// let a seed bundle silently pin keys against this deployment's own `.env`,
/// and the boot log would blame an admin edit that never happened here.
#[tokio::test]
async fn import_clears_another_instances_admin_ownership_marker() {
    let key = "WAFER_RUN_SHARED__APP_NAME";
    let ctx = TestContext::with_products().await.fixture();
    let mut tables = std::collections::BTreeMap::new();
    tables.insert(
        variables::TABLE.to_string(),
        vec![json_map(json!({
            "id": "var_imported",
            "key": key,
            "value": "FromBundle",
            "sensitive": false,
            "updated_by": "admin_on_another_instance",
            "created_at": STAMP,
            "updated_at": STAMP,
        }))
        .into_iter()
        .collect()],
    );
    let snap = DataSnapshot {
        schema_version: data_snapshot::SCHEMA_VERSION,
        tables,
    };

    data_snapshot::import(&as_dev(&ctx), &snap)
        .await
        .expect("import");

    let vars = db::list_all(&ctx, variables::TABLE, Vec::new())
        .await
        .unwrap();
    let row = vars
        .iter()
        .find(|v| v.data["key"] == json!(key))
        .expect("the row was imported");
    assert_eq!(
        row.data["updated_by"],
        json!(""),
        "an imported row must be seeder-owned here, so this deployment's own \
         environment can still seed it: {row:?}"
    );
}

/// The other direction of the same column: an import must not REVOKE an
/// ownership marker a local admin set.
///
/// `Mode::Upsert` writes the bundle's columns over the destination's, so
/// blanking `updated_by` in the imported row — which is right for an INSERT —
/// would erase a local admin's claim on a conflict and hand their key back to
/// the local `.env`. The column is therefore dropped from the update set.
#[tokio::test]
async fn import_does_not_revoke_a_local_admin_ownership_marker() {
    let key = "WAFER_RUN_SHARED__APP_NAME";
    let ctx = TestContext::with_products().await.fixture();

    // This instance already has the key, pinned by a local admin.
    variables::insert(
        &ctx,
        variables::NewVariable {
            key: key.to_string(),
            value: "LocalAdminChoice".to_string(),
            name: String::new(),
            description: String::new(),
            warning: String::new(),
            sensitive: false,
            updated_by: "local_admin".to_string(),
            block: variables::block_for_key(key),
        },
    )
    .await
    .expect("seed the local pinned row");

    let mut tables = std::collections::BTreeMap::new();
    tables.insert(
        variables::TABLE.to_string(),
        vec![json_map(json!({
            "id": "var_from_bundle",
            "key": key,
            "value": "FromBundle",
            "sensitive": false,
            "updated_by": "",
            "created_at": STAMP,
            "updated_at": STAMP,
        }))
        .into_iter()
        .collect()],
    );
    let snap = DataSnapshot {
        schema_version: data_snapshot::SCHEMA_VERSION,
        tables,
    };

    data_snapshot::import(&as_dev(&ctx), &snap)
        .await
        .expect("import");

    let row = variables::get_by_key(&ctx, key)
        .await
        .expect("get")
        .expect("row");
    assert_eq!(
        row.value, "FromBundle",
        "the bundle's VALUE still lands — that is what an import is for"
    );
    assert_eq!(
        row.updated_by, "local_admin",
        "but the local admin's ownership marker survives the import"
    );
}

/// A snapshot carries ordinary site config — that is what an export is FOR —
/// but never a key the runtime owns.
///
/// `admin::ops::reject_runtime_owned_key` refuses these on the admin write
/// path, and `ui::settings_form`'s `CONFIG_SET` refuses them too. This import
/// writes the same table through `db::upsert` and passed neither, so a bundle
/// could plant `__IMPRESSPRESS_RUNTIME_KIND__` or `IMPRESSPRESS_DEPLOY_TOKEN`
/// in the variables table of every instance that seeds from it.
///
/// The planted row is INERT FOR READS — `blocks::config`'s
/// `served_only_from_boot_map` answers those keys from the boot map whatever
/// the table holds, which is what PR #65 fixed — so this is a forged row that
/// shows up on the admin Variables page and travels on into the next export,
/// not a config override. It still must not land.
#[tokio::test]
async fn import_refuses_a_runtime_owned_variable_key() {
    for key in [
        // internal, adapter-injected (`__…__`)
        "__IMPRESSPRESS_RUNTIME_KIND__",
        "__IMPRESSPRESS_BLOCK_SETTINGS_JSON__",
        // infrastructure (`IMPRESSPRESS_*` with no `__`)
        "IMPRESSPRESS_DEPLOY_TOKEN",
    ] {
        let ctx = TestContext::with_products().await.fixture();
        let mut tables = std::collections::BTreeMap::new();
        tables.insert(
            variables::TABLE.to_string(),
            vec![json_map(json!({
                "id": "var_forged",
                "key": key,
                "value": "browser",
                "sensitive": false,
                "created_at": STAMP,
                "updated_at": STAMP,
            }))
            .into_iter()
            .collect()],
        );
        let snap = DataSnapshot {
            schema_version: data_snapshot::SCHEMA_VERSION,
            tables,
        };

        let err = data_snapshot::import(&as_dev(&ctx), &snap)
            .await
            .unwrap_err();
        assert_eq!(err.code, wafer_run::ErrorCode::InvalidArgument, "{key}");

        // Refused in PRE-FLIGHT, like the allowlist and schema-version checks
        // above: a bundle that names one forged key writes none of its rows,
        // rather than importing most of them and failing partway.
        let vars = db::list_all(&ctx, variables::TABLE, Vec::new())
            .await
            .unwrap();
        assert!(
            !vars.iter().any(|v| v.data["key"] == json!(key)),
            "a refused import must not have written {key}: {vars:?}",
        );
    }
}

/// The one reserved key the first version of this guard missed.
///
/// `WAFER_RUN__AUTH__JWT_SECRET` carries no `IMPRESSPRESS_` prefix and is not
/// `__…__`-bracketed, so `is_runtime_owned_key` does not name it — but
/// `blocks::config`'s `served_only_from_boot_map` DOES reserve it, precisely
/// so a stored row cannot rotate the signing key under a running process.
///
/// Unlike every key the guard already refuses, a planted one here is NOT
/// inert. `seed_jwt_secret` writes through `insert_if_absent`, so a row that
/// is already present wins and auto-generation never fires, and
/// `impresspress_server::build_native_runtime` hands that value to boot as the HMAC key for every session
/// JWT and CSRF token. A bundle shared between instances would give each of
/// them one signing secret its author knows — exactly what per-instance
/// auto-generation exists to prevent.
///
/// Nothing legitimate carries such a row: the `_SECRET` suffix makes it
/// unexportable, so this can only be hand-authored.
#[tokio::test]
async fn import_refuses_a_planted_jwt_secret() {
    let ctx = TestContext::with_products().await.fixture();
    let key = impresspress_core::blocks::auth::JWT_SECRET_KEY;
    let mut tables = std::collections::BTreeMap::new();
    tables.insert(
        variables::TABLE.to_string(),
        vec![json_map(json!({
            "id": "var_forged_jwt",
            "key": key,
            "value": "00000000000000000000000000000000",
            "sensitive": true,
            "created_at": STAMP,
            "updated_at": STAMP,
        }))
        .into_iter()
        .collect()],
    );
    let snap = DataSnapshot {
        schema_version: data_snapshot::SCHEMA_VERSION,
        tables,
    };

    let err = data_snapshot::import(&as_dev(&ctx), &snap)
        .await
        .unwrap_err();
    assert_eq!(err.code, wafer_run::ErrorCode::InvalidArgument);

    let vars = db::list_all(&ctx, variables::TABLE, Vec::new())
        .await
        .unwrap();
    assert!(
        !vars.iter().any(|v| v.data["key"] == json!(key)),
        "a refused import must not have planted a signing secret: {vars:?}",
    );
}

/// The env-precedence transition's gate row never travels in a bundle.
///
/// It records that a one-time upgrade pass has run on THIS database. Exported
/// and re-imported it would disarm that pass on a deployment that has not run
/// it — silently turning off the protection for every pre-upgrade admin edit on
/// the importing side.
///
/// It is already held back, by the rule that holds back every
/// `IMPRESSPRESS_`-prefixed key (`variable_is_exportable`): the gate is
/// `IMPRESSPRESS__ADMIN__ENV_PRECEDENCE_TRANSITION`, which is block-scoped
/// config describing the exporting instance, exactly what that rule exists for.
/// Asserted here rather than left to be re-derived, because the consequence of
/// the prefix rule ever narrowing is not obvious from the gate's own code.
#[test]
fn the_env_precedence_transition_gate_is_not_exportable() {
    let row: serde_json::Map<String, serde_json::Value> = json_map(json!({
        "key": variables::ENV_PRECEDENCE_TRANSITION_KEY,
        "value": "done",
        "sensitive": false,
    }))
    .into_iter()
    .collect();
    assert!(
        !data_snapshot::variable_is_exportable(&row),
        "the gate row must never reach another deployment's database"
    );
}

/// The other side of that boundary: the guard refuses only what the RUNTIME
/// owns, not everything with a prefix.
///
/// `WAFER_RUN_SHARED__*` is the half an export exists to carry, and
/// `IMPRESSPRESS__{BLOCK}__*` is ordinary database-backed block config —
/// `variable_is_exportable` holds the latter back at export time because it
/// describes the exporting instance, but a bundle that legitimately carries
/// one (hand-authored, or from a build whose filter differs) must still
/// import rather than being refused as a forgery.
#[tokio::test]
async fn import_accepts_ordinary_config_keys() {
    let ctx = TestContext::with_products().await.fixture();
    let mut tables = std::collections::BTreeMap::new();
    tables.insert(
        variables::TABLE.to_string(),
        vec![
            json_map(json!({
                "id": "var_shared",
                "key": "WAFER_RUN_SHARED__APP_NAME",
                "value": "The print shop",
                "sensitive": false,
                "created_at": STAMP,
                "updated_at": STAMP,
            }))
            .into_iter()
            .collect(),
            json_map(json!({
                "id": "var_block_scoped",
                "key": "IMPRESSPRESS__PRODUCTS__PLATFORM_COUNTRY",
                "value": "NZ",
                "sensitive": false,
                "created_at": STAMP,
                "updated_at": STAMP,
            }))
            .into_iter()
            .collect(),
        ],
    );
    let snap = DataSnapshot {
        schema_version: data_snapshot::SCHEMA_VERSION,
        tables,
    };

    data_snapshot::import(&as_dev(&ctx), &snap)
        .await
        .expect("ordinary config must still import");

    let vars = db::list_all(&ctx, variables::TABLE, Vec::new())
        .await
        .unwrap();
    for key in [
        "WAFER_RUN_SHARED__APP_NAME",
        "IMPRESSPRESS__PRODUCTS__PLATFORM_COUNTRY",
    ] {
        assert!(
            vars.iter().any(|v| v.data["key"] == json!(key)),
            "{key} must have imported: {vars:?}",
        );
    }
}

// ---------------------------------------------------------------------------
// seed::import wiring
// ---------------------------------------------------------------------------

fn index_file() -> seed::SeedFile {
    seed_file("index.html", b"<h1>shop</h1>")
}

/// One product (`Mode::Upsert`), one admin variable (`Mode::Upsert`) and one
/// user (`Mode::Replace`) — at least one table from each of the three
/// blocks, so a test importing this under a WRAP-enforced dev context (see
/// `seed_import_applies_data_json_when_present`) exercises the typed Db
/// grant `dev::wrap_grants` adds per `TABLE_ALLOWLIST` table on more than
/// just the specially-routed products path.
fn mixed_snapshot() -> DataSnapshot {
    let mut tables = std::collections::BTreeMap::new();
    tables.insert(
        PRODUCTS_TABLE.to_string(),
        // `db::upsert` (the path `Mode::Upsert` writes through) issues the
        // insert exactly as given — unlike `db::create`, it does not
        // synthesize `created_at`/`updated_at` for a caller who omits them,
        // so a real export's row (read back off an existing one, which
        // always carries both) is what this fixture has to imitate.
        vec![json_map(json!({
            "id": "prod_seeded",
            "name": "Seeded Widget",
            "status": "active",
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z",
        }))
        .into_iter()
        .collect()],
    );
    tables.insert(
        variables::TABLE.to_string(),
        vec![json_map(json!({
            "id": "var_seeded",
            "key": "WAFER_RUN_SHARED__APP_NAME",
            "value": "Seeded Shop",
            "sensitive": false,
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z",
        }))
        .into_iter()
        .collect()],
    );
    tables.insert(
        users::TABLE.to_string(),
        vec![json_map(json!({
            "id": "user_seeded",
            "email": "seeded@example.com",
            "display_name": "Seeded Owner",
        }))
        .into_iter()
        .collect()],
    );
    DataSnapshot {
        schema_version: data_snapshot::SCHEMA_VERSION,
        tables,
    }
}

/// A [`seed::SeedFile`] declaring `bytes` at `path`, the way an exporter
/// would — the same derivation [`seed_file`] uses for workspace files,
/// reimplemented here because `manifest.data` is a bare file, not one that
/// lands in the workspace tree `seed_file` targets.
fn data_file(path: &str, bytes: &[u8]) -> seed::SeedFile {
    seed::SeedFile {
        path: path.to_string(),
        sha256: impresspress_core::blocks::dev::blobs::sha256_hex(bytes),
        size: bytes.len() as u64,
        content_type: "application/json".to_string(),
    }
}

#[tokio::test]
async fn seed_import_applies_data_json_when_present() {
    // The importer takes the runtime seam explicitly, so a test binds the
    // control it built the context over rather than passing a second one.
    let control = FakeControl::new();
    let ctx = TestContext::with_products()
        .await
        .fixture()
        .with_auth_added()
        .await
        .with_dev_added(control.clone())
        .await;
    let data_bytes = serde_json::to_vec(&mixed_snapshot()).unwrap();
    let manifest = SeedManifest {
        schema_version: seed::SCHEMA_VERSION,
        source_generation: None,
        site: vec![index_file()],
        blocks: vec![],
        data: Some(data_file("data.json", &data_bytes)),
    };
    let fetch = MapFetch::default()
        .with(&seed::site_url("index.html"), b"<h1>shop</h1>")
        .with(&seed::data_url("data.json"), &data_bytes);

    seed::import(&ctx, control.as_ref(), &manifest, &fetch)
        .await
        .unwrap();

    let products = db::list_all(&ctx, PRODUCTS_TABLE, Vec::new())
        .await
        .unwrap();
    assert_eq!(products.len(), 1);
    assert_eq!(products[0].id, "prod_seeded");

    let vars = db::list_all(&ctx, variables::TABLE, Vec::new())
        .await
        .unwrap();
    assert_eq!(vars.len(), 1);
    assert_eq!(vars[0].id, "var_seeded");

    let users_rows = db::list_all(&ctx, users::TABLE, Vec::new()).await.unwrap();
    assert_eq!(users_rows.len(), 1);
    assert_eq!(users_rows[0].id, "user_seeded");
}

/// A `data.json` whose declared hash doesn't match its bytes fails the same
/// way a corrupted `site`/block-source file would (design §10.2) — and the
/// failure propagates out of `seed::import` as a whole, not just out of the
/// data-snapshot step.
#[tokio::test]
async fn seed_import_fails_when_data_json_does_not_verify() {
    let control = FakeControl::new();
    let ctx = TestContext::with_products()
        .await
        .fixture()
        .with_dev_added(control.clone())
        .await;
    let data_bytes = serde_json::to_vec(&mixed_snapshot()).unwrap();
    let mut declared = data_file("data.json", &data_bytes);
    declared.sha256 = "0".repeat(64); // does not match `data_bytes`
    let manifest = SeedManifest {
        schema_version: seed::SCHEMA_VERSION,
        source_generation: None,
        site: vec![index_file()],
        blocks: vec![],
        data: Some(declared),
    };
    let fetch = MapFetch::default()
        .with(&seed::site_url("index.html"), b"<h1>shop</h1>")
        .with(&seed::data_url("data.json"), &data_bytes);

    let err = seed::import(&ctx, control.as_ref(), &manifest, &fetch)
        .await
        .expect_err("a hash mismatch on data.json must fail the whole seed import");
    assert!(
        err.contains("hashes to"),
        "expected a hash-mismatch message, got: {err}"
    );

    // Nothing from the (unverified) snapshot was applied.
    assert_eq!(
        db::list_all(&ctx, PRODUCTS_TABLE, Vec::new())
            .await
            .unwrap()
            .len(),
        0
    );
}

// ---------------------------------------------------------------------------
// Identity: the destination mints its own ids for some allowlisted tables
// ---------------------------------------------------------------------------

const ADMIN_ROLES_TABLE: &str = "impresspress__admin__roles";

/// `created_at`/`updated_at` are `TEXT NOT NULL` with no default on these
/// tables, and a real exported row always carries them — it came out of
/// `db::list_all`. A fixture row that omitted them would fail the insert for
/// a reason that has nothing to do with what these tests are about.
const STAMP: &str = "2026-09-03T00:00:00Z";

/// An import into an instance that has ALREADY seeded its own copies of the
/// rows the snapshot carries must succeed.
///
/// This is the case design §10.2 is entirely about — a bundle importing into
/// a fresh instance — and "fresh" does not mean empty: by the time the seed
/// import runs, admin's migration has seeded `roles` and the boot hook has
/// seeded `variables`, each with an id this instance minted for itself. The
/// exporting instance minted different ids for the same rows. Both tables
/// mark their natural key `UNIQUE` (`roles.name`, `variables.key`), so an
/// upsert keyed on `id` does not conflict on the id at all: it is an INSERT
/// that then violates that index, and `import` fails wholesale with a bare
/// "internal database error".
///
/// It is written with two rows per table on purpose — one whose natural key
/// the destination already has (the collision) and one it does not (a plain
/// insert) — because a fix that simply skipped conflicting rows would pass a
/// test that only had the first.
#[tokio::test]
async fn an_import_lands_on_rows_the_destination_seeded_with_its_own_ids() {
    let ctx = TestContext::with_products().await.fixture();

    // What the DESTINATION seeded for itself, with its own ids.
    seed_row(
        &ctx,
        ADMIN_ROLES_TABLE,
        "role_minted_here",
        json!({ "name": "admin", "description": "this instance's own admin role" }),
    )
    .await;
    seed_row(
        &ctx,
        variables::TABLE,
        "var_minted_here",
        json!({
            "key": "WAFER_RUN_SHARED__APP_NAME",
            "value": "Untitled",
            "sensitive": false,
        }),
    )
    .await;

    // What the SNAPSHOT carries: the same natural keys under the exporting
    // instance's ids, plus one row of each that is genuinely new.
    let mut tables = std::collections::BTreeMap::new();
    tables.insert(
        ADMIN_ROLES_TABLE.to_string(),
        vec![
            json_map(json!({
                "id": "role_minted_over_there",
                "name": "admin",
                "description": "the exporting instance's admin role",
                "created_at": STAMP,
                "updated_at": STAMP,
            }))
            .into_iter()
            .collect(),
            json_map(json!({
                "id": "role_editor",
                "name": "editor",
                "description": "a role the destination has never heard of",
                "created_at": STAMP,
                "updated_at": STAMP,
            }))
            .into_iter()
            .collect(),
        ],
    );
    tables.insert(
        variables::TABLE.to_string(),
        vec![
            json_map(json!({
                "id": "var_minted_over_there",
                "key": "WAFER_RUN_SHARED__APP_NAME",
                "value": "The print shop",
                "sensitive": false,
                "created_at": STAMP,
                "updated_at": STAMP,
            }))
            .into_iter()
            .collect(),
            json_map(json!({
                "id": "var_new",
                "key": "WAFER_RUN_SHARED__HAS_LANDING_PAGE",
                "value": "true",
                "sensitive": false,
                "created_at": STAMP,
                "updated_at": STAMP,
            }))
            .into_iter()
            .collect(),
        ],
    );
    let snapshot = DataSnapshot {
        schema_version: data_snapshot::SCHEMA_VERSION,
        tables,
    };

    data_snapshot::import(&as_dev(&ctx), &snapshot)
        .await
        .expect("an import must land on rows the destination seeded for itself");

    // One `admin` role, not two — and it carries the SNAPSHOT's description
    // under the DESTINATION's id. Keeping the destination's id is what stops
    // an import from orphaning every `user_roles.role_id` already pointing at
    // it.
    let roles = db::list_all(&ctx, ADMIN_ROLES_TABLE, Vec::new())
        .await
        .unwrap();
    let admin_roles: Vec<_> = roles
        .iter()
        .filter(|r| r.data["name"] == json!("admin"))
        .collect();
    assert_eq!(admin_roles.len(), 1, "{roles:?}");
    assert_eq!(admin_roles[0].id, "role_minted_here");
    assert_eq!(
        admin_roles[0].data["description"],
        json!("the exporting instance's admin role")
    );
    // …and the role the destination had never heard of was inserted, under
    // the id the snapshot gave it.
    assert!(roles.iter().any(|r| r.id == "role_editor"), "{roles:?}");

    let vars = db::list_all(&ctx, variables::TABLE, Vec::new())
        .await
        .unwrap();
    let app_name: Vec<_> = vars
        .iter()
        .filter(|v| v.data["key"] == json!("WAFER_RUN_SHARED__APP_NAME"))
        .collect();
    assert_eq!(app_name.len(), 1, "{vars:?}");
    assert_eq!(app_name[0].id, "var_minted_here");
    assert_eq!(app_name[0].data["value"], json!("The print shop"));
    assert!(vars.iter().any(|v| v.id == "var_new"), "{vars:?}");
}

/// Re-importing the SAME snapshot converges rather than duplicating — the
/// idempotence the module docs claim, now that the conflict target is the
/// natural key rather than the id.
#[tokio::test]
async fn re_importing_one_snapshot_converges_on_the_natural_key() {
    let ctx = TestContext::with_products().await.fixture();
    let mut tables = std::collections::BTreeMap::new();
    tables.insert(
        ADMIN_ROLES_TABLE.to_string(),
        vec![json_map(json!({
            "id": "role_a",
            "name": "admin",
            "created_at": STAMP,
            "updated_at": STAMP,
        }))
        .into_iter()
        .collect()],
    );
    let snapshot = DataSnapshot {
        schema_version: data_snapshot::SCHEMA_VERSION,
        tables,
    };

    data_snapshot::import(&as_dev(&ctx), &snapshot)
        .await
        .unwrap();
    data_snapshot::import(&as_dev(&ctx), &snapshot)
        .await
        .unwrap();

    let roles = db::list_all(&ctx, ADMIN_ROLES_TABLE, Vec::new())
        .await
        .unwrap();
    assert_eq!(roles.len(), 1, "{roles:?}");
}

/// Every `Upsert` table's declared conflict columns must be columns the table
/// actually has a `UNIQUE` constraint on — an upsert whose conflict target is
/// not unique is not an upsert, it is an insert that will one day collide.
///
/// Read off the migration SQL, like
/// `every_declared_table_of_the_three_blocks_has_an_export_decision` above:
/// the schema is the ground truth, and a `UNIQUE` added or dropped there
/// without a matching change here should fail rather than wait for an export
/// to fail in someone's browser.
#[test]
fn every_upsert_target_is_a_unique_key_of_its_table() {
    // Whitespace-normalised so the checks below are about the SQL rather than
    // about how it happens to be laid out.
    let sql: String = sqlite_migration_sql()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    for (table, mode) in data_snapshot::TABLE_ALLOWLIST {
        let data_snapshot::Mode::Upsert(conflict) = mode else {
            continue;
        };
        for column in *conflict {
            if *column == "id" {
                // Every allowlisted table declares `id TEXT PRIMARY KEY`, so
                // an id conflict target is unique by construction. Assert the
                // table exists at all, which is what would break first.
                assert!(
                    sql.contains(&format!("CREATE TABLE IF NOT EXISTS {table} (")),
                    "{table} has no CREATE TABLE in the migrations"
                );
                continue;
            }
            // Either an inline `<column> TEXT … UNIQUE` in the CREATE TABLE,
            // or a `CREATE UNIQUE INDEX … ON <table>(<column>)`.
            let create = format!("CREATE TABLE IF NOT EXISTS {table} (");
            let body = sql
                .split_once(&create)
                .map(|(_, rest)| rest.split_once(");").map_or(rest, |(body, _)| body))
                .unwrap_or_else(|| panic!("{table} has no CREATE TABLE in the migrations"));
            let inline = body.split(',').any(|col| {
                let col = col.trim();
                col.starts_with(&format!("{column} ")) && col.contains("UNIQUE")
            });
            let indexed = sql.contains(&format!("ON {table}({column})"))
                || sql.contains(&format!("ON {table} ({column})"));
            assert!(
                inline || indexed,
                "{table}'s upsert conflicts on {column:?}, but no UNIQUE constraint on it \
                 appears in the migrations — the upsert would insert and then collide"
            );
        }
    }
}

/// The `product_id` / `offer_id` columns each allowlisted table declares,
/// read off the migrations.
///
/// Both shapes count, because both are how the schema got here: a column in
/// the `CREATE TABLE` body, and a later `ALTER TABLE … ADD COLUMN` (which is
/// how `variables` got its `offer_id` in migration 005, and the reason a scan
/// of `CREATE TABLE`s alone would have missed exactly the table this test
/// exists to catch).
fn owner_columns_declared_in_migrations() -> BTreeSet<(String, String)> {
    const OWNER_COLUMNS: &[&str] = &["product_id", "offer_id"];
    let sql: String = sqlite_migration_sql()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let mut found = BTreeSet::new();
    for (table, _mode) in data_snapshot::TABLE_ALLOWLIST {
        let create = format!("CREATE TABLE IF NOT EXISTS {table} (");
        let body = sql
            .split_once(&create)
            .map(|(_, rest)| rest.split_once(");").map_or(rest, |(body, _)| body))
            .unwrap_or_else(|| panic!("{table} has no CREATE TABLE in the migrations"));
        // The column NAME is the first token of each comma-separated
        // declaration, compared whole: `stripe_product_id` is not
        // `product_id`, and a substring match would call it one.
        for column in body.split(',') {
            let name = column.split_whitespace().next().unwrap_or_default();
            if OWNER_COLUMNS.contains(&name) {
                found.insert((table.to_string(), name.to_string()));
            }
        }
        let alter = format!("ALTER TABLE {table} ADD COLUMN ");
        let mut rest = sql.as_str();
        while let Some((_, tail)) = rest.split_once(&alter) {
            let name = tail.split_whitespace().next().unwrap_or_default();
            if OWNER_COLUMNS.contains(&name) {
                found.insert((table.to_string(), name.to_string()));
            }
            rest = tail;
        }
    }
    found
}

/// `OWNED_TABLES` is closed against the schema, in both directions.
///
/// The export filters an owned table's rows against the ids its owner
/// actually exported, and products are read live-only — so a table with a
/// `product_id`/`offer_id` that is NOT on that list exports rows pointing at
/// products the archive does not carry. `M9` was three such tables; the point
/// of this test is that the fourth one to be added to `TABLE_ALLOWLIST` fails
/// the build instead of shipping orphans.
///
/// The reverse direction matters too: an entry naming a column its table does
/// not have would filter every row of it out (`None` is dropped), silently
/// emptying the table in every export.
#[test]
fn owned_tables_covers_every_allowlisted_table_with_an_owner_column() {
    let declared = owner_columns_declared_in_migrations();
    // The scan found something at all — a broken parser that found nothing
    // would make both assertions below vacuous.
    assert!(
        declared.len() >= 5,
        "the owner-column scan found {declared:?} — it lost its way"
    );
    let listed: BTreeSet<(String, String)> = data_snapshot::OWNED_TABLES
        .iter()
        .map(|(table, column, _)| (table.to_string(), column.to_string()))
        .collect();

    let unowned: Vec<&(String, String)> = declared.difference(&listed).collect();
    assert!(
        unowned.is_empty(),
        "allowlisted tables with an owner column that OWNED_TABLES does not filter on: \
         {unowned:?} — add each with its owner, or their rows travel orphaned when the owner \
         is soft-deleted"
    );
    let imaginary: Vec<&(String, String)> = listed.difference(&declared).collect();
    assert!(
        imaginary.is_empty(),
        "OWNED_TABLES filters on columns the schema does not declare: {imaginary:?} — every \
         row of those tables would be dropped from every export"
    );

    // And each entry's OWNER is itself exported, or the filter reads against
    // a set that is never filled.
    let allowlisted: BTreeSet<&str> = data_snapshot::TABLE_ALLOWLIST
        .iter()
        .map(|(table, _)| *table)
        .collect();
    for (table, _column, owner) in data_snapshot::OWNED_TABLES {
        assert!(
            allowlisted.contains(owner),
            "{table:?} is filtered against {owner:?}, which is not on TABLE_ALLOWLIST"
        );
    }
}

/// Every SQLite migration of the three blocks, concatenated.
///
/// The same three directories `tables_created_in_migrations` reads, and for
/// the same reason: the schema is the ground truth for what the import can
/// actually do, and a Rust-side declaration of it is a second copy that can
/// fall behind.
fn sqlite_migration_sql() -> String {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut out = String::new();
    for block in ["products", "admin", "auth"] {
        let dir = manifest_dir
            .join("src/blocks")
            .join(block)
            .join("migrations");
        let entries =
            std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));
        for entry in entries {
            let path = entry.expect("dir entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("sql")
                || !path.to_string_lossy().contains("sqlite")
            {
                continue;
            }
            out.push_str(
                &std::fs::read_to_string(&path)
                    .unwrap_or_else(|e| panic!("read {}: {e}", path.display())),
            );
            out.push('\n');
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Write cost: one call per import, not one per row.
// ---------------------------------------------------------------------------

/// `n` users (a `Mode::Replace` table) and `n` products (a `Mode::Upsert`
/// one).
fn wide_snapshot(users_n: usize, products_n: usize) -> DataSnapshot {
    let mut tables = std::collections::BTreeMap::new();
    tables.insert(
        users::TABLE.to_string(),
        (0..users_n)
            .map(|i| {
                json_map(json!({
                    "id": format!("user_{i}"),
                    "email": format!("owner{i}@example.com"),
                    "display_name": format!("Owner {i}"),
                }))
                .into_iter()
                .collect()
            })
            .collect(),
    );
    tables.insert(
        PRODUCTS_TABLE.to_string(),
        (0..products_n)
            .map(|i| {
                json_map(json!({
                    "id": format!("prod_{i}"),
                    "name": format!("Widget {i}"),
                    "status": "active",
                    "created_at": "2026-01-01T00:00:00Z",
                    "updated_at": "2026-01-01T00:00:00Z",
                }))
                .into_iter()
                .collect()
            })
            .collect(),
    );
    DataSnapshot {
        schema_version: data_snapshot::SCHEMA_VERSION,
        tables,
    }
}

async fn counting_ctx() -> (TestContext, std::sync::Arc<std::sync::Mutex<WriteLog>>) {
    TestContext::with_products()
        .await
        .fixture()
        .with_auth_added()
        .await
        .record_writes()
}

/// **An import is one write, not one per row.** The replaced table's delete
/// and rows and every upserted row go in ONE `batch`; not one row goes through
/// a single-row `create` or `upsert`, nor a `create_many` of its own. In the
/// browser each database call is a whole-database save to OPFS, so this is
/// the difference between one save and one per row.
#[tokio::test]
async fn an_import_is_one_batch_not_one_call_per_row() {
    let (ctx, log) = counting_ctx().await;

    let report = data_snapshot::import(&as_dev(&ctx), &wide_snapshot(30, 30))
        .await
        .expect("import");

    {
        let log = log.lock().unwrap();
        assert_eq!(log.creates, 0, "no row goes through a single-row create");
        assert_eq!(log.upserts, 0, "no row goes through a single-row upsert");
        assert!(log.create_many_rows.is_empty(), "no create_many call");
        assert_eq!(
            log.batch_ops,
            [1 + 30 + 30],
            "one batch: the users' delete, 30 creates, 30 product upserts"
        );
    }
    assert_eq!(report.tables.get(users::TABLE), Some(&30));
    assert_eq!(report.tables.get(PRODUCTS_TABLE), Some(&30));
    assert_eq!(
        db::list_all(&ctx, users::TABLE, Vec::new())
            .await
            .unwrap()
            .len(),
        30
    );
    assert_eq!(
        db::list_all(&ctx, PRODUCTS_TABLE, Vec::new())
            .await
            .unwrap()
            .len(),
        30
    );
}

/// Native SQLite has no per-invocation statement limit, so an import larger
/// than any fixed per-call cap (the handler once refused a call over 1000
/// rows or ops) is still one call, not split into several transactions.
#[tokio::test]
async fn an_import_past_a_thousand_rows_is_still_one_batch() {
    let (ctx, log) = counting_ctx().await;

    data_snapshot::import(&as_dev(&ctx), &wide_snapshot(1001, 1001))
        .await
        .expect("import");

    let log = log.lock().unwrap();
    assert!(log.create_many_rows.is_empty());
    assert_eq!(log.batch_ops, [1 + 1001 + 1001]);
}

// ---------------------------------------------------------------------------
// Atomicity: a failed import leaves every table as it was.
// ---------------------------------------------------------------------------

/// A destination holding one account with its password and its admin role —
/// the rows a failed import must leave in place.
async fn ctx_with_an_existing_owner() -> TestContext {
    let ctx = TestContext::with_products()
        .await
        .fixture()
        .with_auth_added()
        .await;
    seed_row(
        &ctx,
        users::TABLE,
        "user_existing",
        json!({ "email": "existing@example.com", "display_name": "Existing Owner" }),
    )
    .await;
    seed_row(
        &ctx,
        local_credentials::TABLE,
        "cred_existing",
        json!({
            "user_id": "user_existing",
            "password_hash": "$argon2id$existing",
            "created_at": STAMP,
        }),
    )
    .await;
    seed_row(
        &ctx,
        user_roles::TABLE,
        "role_existing",
        json!({ "user_id": "user_existing", "role": "admin" }),
    )
    .await;
    ctx
}

async fn ids_in(ctx: &TestContext, table: &str) -> Vec<String> {
    let mut ids: Vec<String> = db::list_all(ctx, table, Vec::new())
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect();
    ids.sort();
    ids
}

/// A snapshot whose credential rows repeat an id fails on the second one —
/// after its users were written and the old credentials cleared, if those
/// were separate writes. The import is one transaction, so the failure
/// leaves the destination's users, credentials and roles exactly as they
/// were: its owner can still sign in.
#[tokio::test]
async fn a_failed_import_leaves_the_existing_users_and_credentials_in_place() {
    let ctx = ctx_with_an_existing_owner().await;
    let row = |value: serde_json::Value| -> serde_json::Map<String, serde_json::Value> {
        json_map(value).into_iter().collect()
    };
    let mut snap = DataSnapshot {
        schema_version: data_snapshot::SCHEMA_VERSION,
        tables: std::collections::BTreeMap::new(),
    };
    snap.tables.insert(
        users::TABLE.to_string(),
        vec![
            row(json!({ "id": "user_a", "email": "a@example.com", "display_name": "A" })),
            row(json!({ "id": "user_b", "email": "b@example.com", "display_name": "B" })),
        ],
    );
    snap.tables.insert(
        local_credentials::TABLE.to_string(),
        vec![
            row(json!({
                "id": "cred_dup", "user_id": "user_a",
                "password_hash": "$argon2id$a", "created_at": STAMP,
            })),
            row(json!({
                "id": "cred_dup", "user_id": "user_b",
                "password_hash": "$argon2id$b", "created_at": STAMP,
            })),
        ],
    );

    let err = data_snapshot::import(&as_dev(&ctx), &snap)
        .await
        .expect_err("a repeated credential id cannot import");
    assert_eq!(err.code, wafer_run::ErrorCode::AlreadyExists, "{err:?}");

    assert_eq!(ids_in(&ctx, users::TABLE).await, ["user_existing"]);
    assert_eq!(
        ids_in(&ctx, local_credentials::TABLE).await,
        ["cred_existing"]
    );
    assert_eq!(ids_in(&ctx, user_roles::TABLE).await, ["role_existing"]);
}

/// Reports a fixed per-invocation statement budget in front of the real
/// SQLite service, the way a Cloudflare D1 service reports its query limit.
/// Every operation is the inner service's.
struct BudgetedDb {
    inner: std::sync::Arc<dyn DatabaseService>,
    budget: StatementBudget,
}

impl BudgetedDb {
    fn inner_service(&self) -> &dyn DatabaseService {
        self.inner.as_ref()
    }
}

wafer_core::forward_database_service! {
    impl DatabaseService for BudgetedDb {
        forward_to inner_service();

        ops {
            get: forward,
            list: forward,
            create: forward,
            create_many: forward,
            update: forward,
            delete: forward,
            count: forward,
            sum: forward,
            query_raw: forward,
            exec_raw: forward,
            delete_where: forward,
            delete_where_count: forward,
            take_where: forward,
            update_where: forward,
            update_where_count: forward,
            increment_field_where: forward,
            upsert: forward,
            aggregate: forward,
            batch: forward,
            insert_guarded: forward,
            update_guarded: forward,
            ensure_schema_table: forward,
            ensure_schema_tables: forward,
            schema_table_exists: forward,
            schema_columns: forward,
            schema_drop_table: forward,
            schema_add_column: forward,
            set_strict_schema: forward,
            statement_budget: custom,
        }

        fn statement_budget(&self) -> Result<StatementBudget, DatabaseError> {
            Ok(self.budget)
        }
    }
}

/// An import is admitted against the backend's statement budget as a whole:
/// one that does not fit what the invocation has left is refused before
/// anything runs. Split into one call per table, its first calls would each
/// fit and commit, and the refusal would land partway through.
#[tokio::test]
async fn an_import_over_the_statement_budget_writes_nothing() {
    let ctx = ctx_with_an_existing_owner()
        .await
        .wrap_database_service(|inner| {
            std::sync::Arc::new(BudgetedDb {
                inner,
                // 61 statements fit the limit; 40 are left.
                budget: StatementBudget::Limited {
                    limit: 100,
                    used: 60,
                },
            })
        });

    let err = data_snapshot::import(&as_dev(&ctx), &wide_snapshot(30, 30))
        .await
        .expect_err("61 statements do not fit the 40 left");
    assert_eq!(err.code, wafer_run::ErrorCode::ResourceExhausted, "{err:?}");

    assert_eq!(ids_in(&ctx, users::TABLE).await, ["user_existing"]);
    assert!(ids_in(&ctx, PRODUCTS_TABLE).await.is_empty());
}
