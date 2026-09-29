//! Per-block `/openapi.json` snapshots.
//!
//! This is the gate for the derive migration. Replacing a hand-written
//! schema with a derived one can change the *public contract* in two ways
//! that are invisible at the call site:
//!
//! 1. **Widening** — derive exposes every field not marked `serde(skip)`,
//!    including any a hand-written schema deliberately omitted.
//! 2. **Description loss** — editorial text vanishes unless it is
//!    reintroduced as a doc comment.
//!
//! So: snapshot a block before migrating it, migrate, then read the diff.
//! Every changed line is a decision. Regenerate with
//! `UPDATE_OPENAPI_SNAPSHOTS=1 cargo test -p impresspress-core --test openapi_snapshot`.
//!
//! Creating a baseline is that explicit run and nothing else. A missing
//! baseline fails the test rather than being written in passing, so deleting
//! a committed contract cannot be a green run and a new block's contract
//! cannot enter the tree unreviewed.

use std::path::PathBuf;

mod baselines;

/// Blocks under migration, mapped to the URL prefixes they actually serve.
///
/// **The prefix is NOT derivable from the block name**, and assuming it is
/// makes this whole gate silently vacuous:
///
/// * `auth_ui` serves `/b/auth/*` — nothing is under `/b/auth_ui/`.
/// * `files` serves TWO prefixes, `/b/storage/*` and `/b/cloudstorage/*`.
///
/// A name-derived prefix would produce a permanently empty snapshot for both,
/// which passes forever and reviews nothing.
const SNAPSHOTTED_BLOCKS: &[(&str, &[&str])] = &[
    ("products", &["/b/products"]),
    ("auth_ui", &["/b/auth"]),
    ("files", &["/b/storage", "/b/cloudstorage"]),
    ("messages", &["/b/messages"]),
    ("admin", &["/b/admin"]),
    ("tickets", &["/b/tickets"]),
    ("llm", &["/b/llm"]),
    ("vector", &["/b/vector"]),
    ("legalpages", &["/b/legalpages"]),
];

/// Blocks whose snapshot exists only under a non-default feature: the block,
/// its prefixes, and whether this run compiles it.
///
/// `dev` cannot live in [`SNAPSHOTTED_BLOCKS`]: a default-feature run does not
/// compile the block, `real_block_infos()` does not list it, and the empty-
/// snapshot guard below would (correctly) fail.
///
/// Each row carries its OWN gate, and one list serves both uses: a row whose
/// gate is true is compared, a row whose gate is false is the excuse the
/// unchecked-baseline check needs for its committed file. A cfg-gated *pair*
/// of slices would name a block twice and the copies could drift; a single
/// bool for the whole list would be worse still, excusing a row gated on some
/// other feature in every run that has that feature off.
const FEATURE_GATED_BLOCKS: &[(&str, &[&str], bool)] =
    &[("dev", &["/b/dev"], cfg!(feature = "block-dev"))];

/// Blocks that legitimately have no schema-carrying endpoints yet, so an
/// empty snapshot for them is correct rather than a sign the prefix map or
/// the document's block list is wrong.
///
/// The list is empty again. `admin` left it when its four JSON API reads were
/// typed, `tickets` when its thirteen JSON endpoints were, `llm` / `vector`
/// when their thirteen and nine were, and `legalpages` when its PATCH
/// declared the request body that makes `status` unreachable from it.
///
/// Every block in `SNAPSHOTTED_BLOCKS` has schemas, so an empty snapshot for
/// any of them means the gate is vacuous — wrong prefix, or the block missing
/// from the document's block list — and must fail loudly.
const LEGITIMATELY_EMPTY: &[&str] = &[];

fn snapshot_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots")
}

/// Every path in the generated OpenAPI document belonging to `block`,
/// pretty-printed with sorted keys so the output is diff-stable.
fn block_openapi(doc: &serde_json::Value, prefixes: &[&str]) -> String {
    let paths = doc["paths"].as_object().expect("openapi paths object");

    // BTreeMap gives deterministic key ordering regardless of how the
    // generator happened to insert them.
    let filtered: std::collections::BTreeMap<&String, &serde_json::Value> = paths
        .iter()
        .filter(|(path, _)| prefixes.iter().any(|p| path.starts_with(p)))
        .collect();

    serde_json::to_string_pretty(&filtered).expect("serialize block paths")
}

#[tokio::test]
async fn openapi_matches_committed_snapshots() {
    let ctx = impresspress_core::test_support::TestContext::new().await;
    let doc = impresspress_core::test_support::openapi_document(&ctx).await;

    let updating = std::env::var("UPDATE_OPENAPI_SNAPSHOTS").is_ok();
    std::fs::create_dir_all(snapshot_dir()).expect("create snapshot dir");

    let mut failures = Vec::new();
    let mut compared = Vec::new();

    let always = SNAPSHOTTED_BLOCKS
        .iter()
        .map(|(block, prefixes)| (*block, *prefixes));
    let gated = FEATURE_GATED_BLOCKS
        .iter()
        .filter(|(_, _, compiled)| *compiled)
        .map(|(block, prefixes, _)| (*block, *prefixes));
    for (block, prefixes) in always.chain(gated) {
        let actual = block_openapi(&doc, prefixes);
        let path = snapshot_dir().join(format!("{block}.openapi.json"));
        compared.push(block.to_string());

        // An empty snapshot for a block that has schema-carrying endpoints
        // means the prefix map is wrong and this block is being "guarded" by
        // a diff that can never change. Only the blocks in
        // `LEGITIMATELY_EMPTY` are exempt.
        if !LEGITIMATELY_EMPTY.contains(&block) && actual.trim() == "{}" {
            failures.push(format!(
                "\n=== {block} ===\nEMPTY snapshot. This block's prefixes {prefixes:?} matched no \
                 OpenAPI paths, so its gate is vacuous. Either the prefix map is wrong or the \
                 block is missing from the document's block list."
            ));
            continue;
        }

        if updating {
            std::fs::write(&path, &actual).expect("write snapshot");
            continue;
        }

        // A missing baseline is a failure, never a silent write. Writing it
        // here would mean `git rm`-ing a committed contract is a green run,
        // and a new block's published contract would get a baseline nobody
        // ever read.
        if !path.exists() {
            failures.push(format!(
                "\n=== {block} ===\nNo baseline at {}. A block's published OpenAPI contract is \
                 a decision: review the document below, then create the file with \
                 UPDATE_OPENAPI_SNAPSHOTS=1 cargo test -p impresspress-core --test \
                 openapi_snapshot. If the block is gone, drop its row above and `git rm` \
                 the baseline instead.\n{actual}",
                path.display()
            ));
            continue;
        }

        let expected = std::fs::read_to_string(&path).expect("read snapshot");
        if expected != actual {
            failures.push(format!(
                "\n=== {block} ===\nSnapshot differs. Review EVERY changed line:\n\
                 - a new property = the contract widened; decide serde(skip), a view type, or accept\n\
                 - a removed description = editorial text lost; restore it as a /// doc comment\n\
                 Accept with: UPDATE_OPENAPI_SNAPSHOTS=1 cargo test -p impresspress-core --test openapi_snapshot\n\
                 Snapshot: {}",
                path.display()
            ));
        }
    }

    let absent_by_feature: Vec<&str> = FEATURE_GATED_BLOCKS
        .iter()
        .filter(|(_, _, compiled)| !*compiled)
        .map(|(block, _, _)| *block)
        .collect();
    for stem in baselines::unchecked(
        &snapshot_dir(),
        ".openapi.json",
        &compared,
        &absent_by_feature,
    ) {
        failures.push(format!(
            "\n=== {stem} ===\n{stem}.openapi.json was not compared by this run, so the gate is \
             vacuous for it. Add the block to SNAPSHOTTED_BLOCKS with the prefixes it serves; or, \
             if it is compiled in only under a feature, to FEATURE_GATED_BLOCKS with the `cfg!` \
             that gates it; or `git rm` the baseline if the block is gone."
        ));
    }

    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// `admin` shipped 17 endpoints and zero schemas: its handlers returned the
/// database layer's untyped `RecordList` / `HashMap<String, Value>` directly,
/// so `has_schema()` filtered every one of them out and the whole JSON API was
/// absent from `/openapi.json`. These four reads and three role writes are the
/// ones that carry a contract; the HTML pages must stay absent.
#[tokio::test]
async fn admin_json_api_appears_in_openapi() {
    let ctx = impresspress_core::test_support::TestContext::new().await;
    let doc = impresspress_core::test_support::openapi_document(&ctx).await;

    for (path, method) in [
        ("/b/admin/api/users", "get"),
        ("/b/admin/api/iam/roles", "get"),
        ("/b/admin/api/iam/roles", "post"),
        ("/b/admin/api/iam/roles/{id}", "patch"),
        ("/b/admin/api/iam/roles/{id}", "delete"),
        ("/b/admin/api/settings", "get"),
        ("/b/admin/api/logs", "get"),
    ] {
        assert!(
            !doc["paths"][path][method].is_null(),
            "{method} {path} must carry a schema and appear in /openapi.json - admin's \
             JSON API was previously invisible because its handlers were untyped"
        );
        assert_eq!(
            doc["paths"][path][method]["security"],
            serde_json::json!([{ "bearerAuth": [] }]),
            "{method} {path} is AuthLevel::Admin and must carry a security requirement"
        );
    }

    // The two role writes that answer with a role publish the list's row
    // projection, so a consumer reading one role from any of them can rely on
    // one shape. The update adds one optional field beside it: `warning`,
    // present only when a rename's grants did not all follow it.
    let list_row = &doc["paths"]["/b/admin/api/iam/roles"]["get"]["responses"]["200"]["content"]
        ["application/json"]["schema"]["properties"]["records"]["items"];
    for (path, method, extra) in [
        ("/b/admin/api/iam/roles", "post", None),
        ("/b/admin/api/iam/roles/{id}", "patch", Some("warning")),
    ] {
        let written = &doc["paths"][path][method]["responses"]["200"]["content"]
            ["application/json"]["schema"];
        let mut properties = written["properties"].clone();
        if let Some(extra) = extra {
            assert!(
                properties
                    .as_object_mut()
                    .expect("properties is an object")
                    .remove(extra)
                    .is_some(),
                "{method} {path} must publish `{extra}`"
            );
        }
        assert_eq!(
            properties, list_row["properties"],
            "{method} {path} must publish the same row projection as the list"
        );
        assert_eq!(
            written["required"], list_row["required"],
            "{method} {path} must publish the same row projection as the list, and \
             nothing beside it that is required"
        );
    }
}

/// Every endpoint `packages/impresspress-js` calls must publish a response
/// schema.
///
/// This is the anti-vacuity gate for the SDK's type-freshness check
/// (`packages/impresspress-js/scripts/generate-api-types.mjs`). That check
/// regenerates TypeScript types from these snapshots and diffs them, so it can
/// only see a reshaped response body on an endpoint that *describes* its
/// response. Applied to the surface as it stood before this test, it would
/// have covered eleven of the SDK's twenty-eight JSON call sites and reported
/// green on the other seventeen — including the seven files handlers PR #22
/// reshaped, which passed both existing snapshot gates for exactly this reason
/// (`1ccbb452`, "publish the RecordList envelope the JS SDK reads").
///
/// The list below is the SDK's non-products call surface, one row per
/// `this.request({ method, url })` in `src/services/{auth,iam,storage,
/// extensions}.service.ts`, with `${…}` interpolations written as the path
/// parameter the route declares. `packages/impresspress-js/test/
/// api-coverage.test.ts` derives the same set from the SDK source and fails if
/// a call site is added that this list does not carry, so the two cannot drift
/// apart silently.
///
/// The products surface (`this.call("products", …)`) is not listed: it is
/// already described end-to-end and its paths are assembled by helpers rather
/// than written literally. The TypeScript census counts it.
#[tokio::test]
async fn endpoints_the_sdk_calls_publish_a_response_schema() {
    // The two SDK call sites whose success response is deliberately not JSON.
    // Each names what it answers instead; a JSON schema on either would
    // describe a body the handler never sends.
    const NOT_JSON: &[(&str, &str, &str)] = &[
        (
            "get",
            "/b/storage/api/buckets/{name}/objects/{key}",
            "raw object bytes with the stored Content-Type - \
             `storage::objects::handle_get_object`",
        ),
        (
            "post",
            "/b/auth/api/verify",
            "an SSR HTML page - `api::verify::handle` answers `html_respond(...)` on \
             every branch",
        ),
    ];

    const SDK_JSON_CALL_SITES: &[(&str, &str)] = &[
        // auth.service.ts
        ("post", "/b/auth/api/signup"),
        ("post", "/b/auth/api/login"),
        ("post", "/b/auth/api/logout"),
        ("get", "/b/auth/api/me"),
        ("patch", "/b/auth/api/me"),
        ("post", "/b/auth/api/forgot-password"),
        ("post", "/b/auth/api/reset-password"),
        ("post", "/b/auth/api/change-password"),
        ("post", "/b/auth/api/refresh"),
        ("post", "/b/auth/api/resend-verification"),
        // `/b/auth/oauth/login` is deliberately absent: `signInWithOAuth`
        // builds that URL for the caller to NAVIGATE to and never requests
        // it, because the endpoint's binding cookie is only stored
        // first-party on a top-level navigation. There is no response body
        // for the SDK's types to go stale against.
        // iam.service.ts
        ("get", "/b/admin/api/iam/roles"),
        ("post", "/b/admin/api/iam/roles"),
        ("patch", "/b/admin/api/iam/roles/{id}"),
        ("delete", "/b/admin/api/iam/roles/{id}"),
        // extensions.service.ts
        ("get", "/b/admin/api/extensions"),
        // storage.service.ts
        ("get", "/b/storage/api/buckets"),
        ("post", "/b/storage/api/buckets"),
        ("delete", "/b/storage/api/buckets/{name}"),
        ("get", "/b/storage/api/buckets/{name}/objects"),
        ("post", "/b/storage/api/buckets/{name}/objects"),
        ("delete", "/b/storage/api/buckets/{name}/objects/{key}"),
        ("get", "/b/storage/api/search"),
        ("get", "/b/storage/api/recent"),
        // extensions.service.ts - CloudStorageExtension
        ("get", "/b/cloudstorage/shares"),
        ("post", "/b/cloudstorage/shares"),
        ("delete", "/b/cloudstorage/shares/{id}"),
        ("get", "/b/cloudstorage/quota"),
    ];

    let ctx = impresspress_core::test_support::TestContext::new().await;
    let doc = impresspress_core::test_support::openapi_document(&ctx).await;

    let mut undescribed = Vec::new();
    for (method, path) in SDK_JSON_CALL_SITES {
        let operation = &doc["paths"][*path][*method];
        if operation.is_null() {
            undescribed.push(format!(
                "{} {path}: absent from /openapi.json entirely (the row declares no schema at all)",
                method.to_uppercase()
            ));
            continue;
        }
        if operation["responses"]["200"]["content"]["application/json"]["schema"].is_null() {
            undescribed.push(format!(
                "{} {path}: declared, but its 200 has no `application/json` schema - add \
                 `.output(response_schema_of::<T>)` to the row",
                method.to_uppercase()
            ));
        }
    }

    assert!(
        undescribed.is_empty(),
        "the SDK's type-freshness check cannot see a reshaped response body on an endpoint \
         that does not describe one:\n{}",
        undescribed.join("\n")
    );

    // Anti-vacuity, both directions.
    //
    // The exceptions are the only two call sites allowed to answer without a
    // JSON schema, so each must still be a *declared* row - an exception for a
    // path the block does not serve would silently excuse nothing, and would
    // survive the route being deleted.
    let declared: Vec<(String, String)> = impresspress_core::test_support::real_block_infos()
        .iter()
        .flat_map(|info| {
            info.endpoints.iter().map(|ep| {
                let published = ep
                    .path
                    .split('/')
                    .map(|seg| {
                        match seg
                            .strip_prefix('{')
                            .and_then(|n| n.strip_suffix('}'))
                            .and_then(|n| n.strip_suffix("..."))
                        {
                            Some(rest) => format!("{{{rest}}}"),
                            None => seg.to_string(),
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("/");
                (format!("{:?}", ep.method).to_lowercase(), published)
            })
        })
        .collect();

    for (method, path, why) in NOT_JSON {
        assert!(
            declared.iter().any(|(m, p)| m == method && p == path),
            "`{} {path}` is excused from the JSON-schema requirement ({why}), but no block \
             declares it - drop the exception or fix the path",
            method.to_uppercase()
        );
        assert!(
            doc["paths"][*path][*method]["responses"]["200"]["content"]["application/json"]
                ["schema"]
                .is_null(),
            "`{} {path}` answers {why}, so it must NOT publish an application/json response \
             schema - either the handler changed or the exception is stale",
            method.to_uppercase()
        );
    }
}

/// The products block's row endpoints used to echo database records, and the
/// hand-written schemas beside them documented a row the handler never
/// consulted. Each is now a `contracts::*View` the handler builds, and the
/// schema is derived from it. A derived row schema always carries a complete
/// `required` list — the view has no optional fields — so `required` is how
/// this test tells a derived schema from a hand-written one that happened to
/// list the same properties.
#[tokio::test]
async fn products_row_schemas_are_derived_from_the_views() {
    let ctx = impresspress_core::test_support::TestContext::new().await;
    let doc = impresspress_core::test_support::openapi_document(&ctx).await;
    let paths = &doc["paths"];

    let subscription = &paths["/b/products/subscription"]["get"]["responses"]["200"]["content"]
        ["application/json"]["schema"]["properties"]["subscription"];
    assert_eq!(
        subscription["type"],
        serde_json::json!(["object", "null"]),
        "the subscription stays nullable: {subscription}"
    );
    let required = subscription["required"]
        .as_array()
        .unwrap_or_else(|| panic!("a derived row lists its fields as required: {subscription}"));
    for field in ["id", "plan", "status", "addon_projects", "addon_d1_bytes"] {
        assert!(
            required.contains(&serde_json::json!(field)),
            "`{field}` is always emitted: {subscription}"
        );
    }
    assert!(
        subscription["properties"]["user_id"].is_null()
            && subscription["properties"]["stripe_customer_id"].is_null(),
        "the projection withholds the owner and provider customer: {subscription}"
    );
}

/// The public catalog (`/b/products/catalog`, `/b/products/catalog/{id}`) is
/// the block's `AuthLevel::Public` row surface, and it used to publish
/// whatever the products row held. `CatalogProductView` withholds the
/// ownership, moderation and provider columns; this walks every published
/// field name under the public paths and pins that. The admin list is
/// checked to carry the same names so the walk is known not to be vacuous.
#[tokio::test]
async fn products_public_catalog_withholds_the_internal_columns() {
    let ctx = impresspress_core::test_support::TestContext::new().await;
    let doc = impresspress_core::test_support::openapi_document(&ctx).await;

    const WITHHELD: &[&str] = &[
        "created_by",
        "owner_kind",
        "owner_id",
        "seller_account_id",
        "approval_status",
        "stripe_product_id",
        "current_version",
        "submitted_at",
        "deleted_at",
    ];

    let public = published_field_names(&doc, &["/b/products/catalog"]);
    assert!(
        public.contains(&"stock".to_string()),
        "no catalog fields found - the walk is looking in the wrong place and \
         this test would pass forever: {public:?}"
    );
    for field in WITHHELD {
        assert!(
            !public.contains(&field.to_string()),
            "the public catalog publishes `{field}`: {public:?}"
        );
    }

    let admin = published_field_names(&doc, &["/b/products/api/admin/products"]);
    for field in WITHHELD {
        assert!(
            admin.contains(&field.to_string()),
            "the admin product list is expected to carry `{field}`, so its absence \
             from the catalog is a projection and not a missing column: {admin:?}"
        );
    }
}

/// The order state columns are published on seven surfaces: the buyer,
/// seller and admin lists and details, and the guest status endpoint. Their
/// value sets are defined once, by `contracts::ReconciliationStatus` and
/// `contracts::OrderStatus`, and every surface must carry that set as an
/// `enum`. Prose that named three of nine values is how the published
/// description drifted from the code in the first place.
#[tokio::test]
async fn products_order_surfaces_publish_the_state_enums() {
    let ctx = impresspress_core::test_support::TestContext::new().await;
    let doc = impresspress_core::test_support::openapi_document(&ctx).await;

    let surfaces =
        objects_with_property(&doc, &["/b/products"], "responses", "reconciliation_status");
    assert!(
        surfaces.len() >= 7,
        "expected the buyer, seller and admin list/detail rows plus the guest status \
         endpoint, found {} schemas carrying `reconciliation_status`",
        surfaces.len()
    );
    for props in &surfaces {
        assert_eq!(
            props["reconciliation_status"]["enum"],
            serde_json::json!([
                "pending",
                "awaiting_payment",
                "reconciled",
                "provider_error",
                "payment_succeeded_awaiting_checkout",
                "payment_failed",
                "payment_processing",
                "payment_requires_action",
                "payment_canceled"
            ]),
            "every value `repo::purchases` and `stripe` store must be published: {:?}",
            props["reconciliation_status"]
        );
    }

    let order_states = serde_json::json!([
        "pending",
        "checkout_started",
        "completed",
        "partially_refunded",
        "refunded",
        "failed"
    ]);
    for props in &surfaces {
        assert_eq!(
            props["status"]["enum"], order_states,
            "`claim_for_checkout` writes `checkout_started`; every stored order state must \
             be published: {:?}",
            props["status"]
        );
    }
    let failures = objects_with_property(
        &doc,
        &["/b/products/api/seller/stats"],
        "responses",
        "order_id",
    );
    assert!(
        !failures.is_empty(),
        "the seller stats publish failed-order summaries; the walk found none"
    );
    for props in &failures {
        assert_eq!(
            props["status"]["enum"], order_states,
            "the seller failure summary is an order surface too: {:?}",
            props["status"]
        );
    }
}

/// `RefundView.provider_status` used to say "or `manual`". No refund row
/// ever holds that: a refund recorded without a provider goes through the
/// ledger, and `mark_succeeded` writes `succeeded` to both state columns;
/// `manual` exists only on the ephemeral `RefundResult` a refund call
/// returns. The column defaults to the empty string, so a row is empty until
/// the provider answers. Refund rows are the schemas carrying
/// `target_refunded_total_minor`, which `RefundResult` does not.
#[tokio::test]
async fn products_refund_rows_describe_provider_status_truthfully() {
    let ctx = impresspress_core::test_support::TestContext::new().await;
    let doc = impresspress_core::test_support::openapi_document(&ctx).await;

    // `target_refunded_total_minor` is on the full `RefundView`, which the
    // seller and admin details embed. The buyer's detail embeds
    // `BuyerRefundView`, which withholds the provider handles and the
    // operator fields but keeps `provider_status` — so it is collected by
    // that name instead, and held to the same description.
    let mut rows = objects_with_property(
        &doc,
        &["/b/products"],
        "responses",
        "target_refunded_total_minor",
    );
    assert!(
        rows.len() >= 2,
        "the seller and admin order details embed full refund rows; found {}",
        rows.len()
    );
    let buyer_rows: Vec<_> =
        objects_with_property(&doc, &["/b/products"], "responses", "provider_status")
            .into_iter()
            .filter(|props| {
                // `completed_at` is what distinguishes the buyer's refund ROW from
                // `RefundResult`, the ephemeral body a refund CALL returns —
                // which also carries `amount_minor` and `provider_status`.
                props.get("target_refunded_total_minor").is_none()
                    && props.get("amount_minor").is_some()
                    && props.get("completed_at").is_some()
            })
            .collect();
    assert!(
        !buyer_rows.is_empty(),
        "the buyer's order detail must embed its own refund row"
    );
    rows.extend(buyer_rows);
    for props in &rows {
        // Collapse whitespace before matching: rustdoc wraps these sentences,
        // so a phrase can land with a newline in the middle of it and a
        // literal `contains` would fail on prose that is perfectly correct.
        let raw = props["provider_status"]["description"]
            .as_str()
            .unwrap_or_default();
        let description = raw.split_whitespace().collect::<Vec<_>>().join(" ");
        let description = description.as_str();
        assert!(
            !description.contains("`manual`"),
            "no refund row ever holds `manual`: {description}"
        );
        assert!(
            description.contains("until the provider answers")
                && description.contains("`succeeded`"),
            "the description must say what the column holds before the provider answers \
             and for a refund recorded without one: {description}"
        );
    }
}

/// Every field name `block` publishes: the keys of every `properties` object
/// anywhere in its schemas, plus the `name` of every declared parameter.
///
/// Names, not prose. A description that explains which column is deliberately
/// withheld is documentation; a `properties` key with the same text is a
/// published field. Only the second is a leak, so only the second is checked.
fn published_field_names(doc: &serde_json::Value, prefixes: &[&str]) -> Vec<String> {
    fn walk(node: &serde_json::Value, out: &mut Vec<String>) {
        match node {
            serde_json::Value::Object(map) => {
                if let Some(serde_json::Value::Object(props)) = map.get("properties") {
                    out.extend(props.keys().cloned());
                }
                if let Some(serde_json::Value::Array(params)) = map.get("parameters") {
                    out.extend(
                        params
                            .iter()
                            .filter_map(|p| p.get("name")?.as_str().map(str::to_string)),
                    );
                }
                for value in map.values() {
                    walk(value, out);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    walk(item, out);
                }
            }
            _ => {}
        }
    }

    let paths = doc["paths"].as_object().expect("openapi paths object");
    let mut out = Vec::new();
    for (path, node) in paths {
        if prefixes.iter().any(|p| path.starts_with(p)) {
            walk(node, &mut out);
        }
    }
    out
}

/// The admin block is the most sensitive surface in the document, and this is
/// the first release in which any of it is publicly described. Its handlers
/// read `wafer_run__auth__users` and `impresspress__admin__variables`, tables
/// that hold credential material, so every published field must be an explicit
/// projection rather than an echoed row.
#[tokio::test]
async fn admin_openapi_publishes_no_credential_field() {
    let ctx = impresspress_core::test_support::TestContext::new().await;
    let doc = impresspress_core::test_support::openapi_document(&ctx).await;
    let fields = published_field_names(&doc, &["/b/admin"]);

    assert!(
        !fields.is_empty(),
        "no admin fields found - the walk is looking in the wrong place and this \
         test would pass forever"
    );

    for field in &fields {
        let lower = field.to_lowercase();
        for forbidden in [
            "password",
            "verification_token",
            "token_hash",
            "access_token",
            "refresh_token",
            "session_token",
            "secret",
            "hash",
        ] {
            assert!(
                !lower.contains(forbidden),
                "admin publishes a field named `{field}`, which matches the \
                 credential pattern `{forbidden}`"
            );
        }
    }
}

/// `llm` declared 18 endpoints and zero schemas: every handler deserialized
/// into a private in-function struct and answered with a `serde_json::json!`
/// literal, so `has_schema()` filtered all of them out. The thirteen JSON
/// endpoints below are the ones that carry a contract; the five HTML pages
/// must stay absent, because a schema is what turns an endpoint into a tool.
#[tokio::test]
async fn llm_json_api_appears_in_openapi_and_its_pages_do_not() {
    let ctx = impresspress_core::test_support::TestContext::new().await;
    let doc = impresspress_core::test_support::openapi_document(&ctx).await;

    for (method, path) in [
        ("post", "/b/llm/api/chat"),
        ("post", "/b/llm/api/chat/stream"),
        ("get", "/b/llm/api/providers"),
        ("post", "/b/llm/api/providers"),
        ("patch", "/b/llm/api/providers/{id}"),
        ("delete", "/b/llm/api/providers/{id}"),
        ("post", "/b/llm/api/providers/{id}/discover-models"),
        ("get", "/b/llm/api/models"),
        ("get", "/b/llm/api/models/{backend_id}/{model_id}/status"),
        ("post", "/b/llm/api/models/{backend_id}/{model_id}/load"),
        ("post", "/b/llm/api/models/{backend_id}/{model_id}/unload"),
        ("get", "/b/llm/api/config"),
        ("post", "/b/llm/api/config"),
    ] {
        assert!(
            !doc["paths"][path][method].is_null(),
            "{method} {path} must carry a schema and appear in /openapi.json"
        );
        assert_eq!(
            doc["paths"][path][method]["security"],
            serde_json::json!([{ "bearerAuth": [] }]),
            "{method} {path} is Authenticated or Admin and must carry a security requirement"
        );
    }

    for path in [
        "/b/llm/",
        "/b/llm/threads/{id}",
        "/b/llm/settings",
        "/b/llm/providers",
        "/b/llm/models",
    ] {
        assert!(
            doc["paths"][path].is_null(),
            "{path} serves HTML and must carry no schema - a schema would make it \
             a callable tool"
        );
    }

    // The two SSE endpoints answer with `text/event-stream`, one frame per
    // `ChatChunk` / `LoadProgress`. An `application/json` response schema
    // there would describe a body that is never sent, so they carry their
    // request/path contract only.
    for (method, path) in [
        ("post", "/b/llm/api/chat/stream"),
        ("post", "/b/llm/api/models/{backend_id}/{model_id}/load"),
    ] {
        assert!(
            doc["paths"][path][method]["responses"]["200"]["content"].is_null(),
            "{method} {path} streams SSE and must not publish a JSON response schema"
        );
    }
    assert!(
        !doc["paths"]["/b/llm/api/chat/stream"]["post"]["requestBody"].is_null(),
        "the streaming chat endpoint takes the same body as the buffered one and must say so"
    );

    // Both chat endpoints deserialize the same struct, so they must publish
    // the same request schema; a divergence would mean one of them stopped
    // going through the shared prelude.
    assert_eq!(
        doc["paths"]["/b/llm/api/chat"]["post"]["requestBody"],
        doc["paths"]["/b/llm/api/chat/stream"]["post"]["requestBody"],
        "chat and chat/stream must publish one request contract"
    );

    // The three provider writes publish the list's row projection, so a
    // consumer reading one provider from any of them can rely on one shape.
    let list_row = &doc["paths"]["/b/llm/api/providers"]["get"]["responses"]["200"]["content"]
        ["application/json"]["schema"]["properties"]["providers"]["items"];
    assert!(
        !list_row["properties"].is_null(),
        "the provider list must publish a row projection: {doc}"
    );
    for (method, path) in [
        ("post", "/b/llm/api/providers"),
        ("patch", "/b/llm/api/providers/{id}"),
    ] {
        let written = &doc["paths"][path][method]["responses"]["200"]["content"]
            ["application/json"]["schema"];
        assert_eq!(
            written["properties"], list_row["properties"],
            "{method} {path} must publish the same row projection as the list"
        );
        assert_eq!(
            written["required"], list_row["required"],
            "{method} {path} must publish the same row projection as the list"
        );
    }
}

/// Provider rows reference an API key by the *name* of the admin variable
/// that holds it (`key_var`); the key itself is resolved into the in-memory
/// router at reload time and lives on the very `ProviderConfig` the handlers
/// hold. Every published field must therefore be an explicit projection, and
/// this pins two things structurally:
///
/// 1. No published field name matches a credential pattern. `api_key` is the
///    one this block could leak; the rest are the shared list.
/// 2. `key_var` is the only `key`-bearing name, and it is a plain string (a
///    variable *name*), never an object that could carry a value.
#[tokio::test]
async fn llm_openapi_publishes_no_credential_field() {
    let ctx = impresspress_core::test_support::TestContext::new().await;
    let doc = impresspress_core::test_support::openapi_document(&ctx).await;
    let fields = published_field_names(&doc, &["/b/llm"]);

    assert!(
        !fields.is_empty(),
        "no llm fields found - the walk is looking in the wrong place and this \
         test would pass forever"
    );

    let mut saw_key_var = false;
    for field in &fields {
        let lower = field.to_lowercase();
        for forbidden in [
            "api_key",
            "apikey",
            "secret",
            "password",
            "hash",
            "access_token",
            "refresh_token",
            "session_token",
            "bearer",
            "credential",
        ] {
            assert!(
                !lower.contains(forbidden),
                "llm publishes a field named `{field}`, which matches the \
                 credential pattern `{forbidden}`"
            );
        }
        if lower.contains("key") {
            assert_eq!(
                field, "key_var",
                "the only key-bearing name llm may publish is `key_var`, the \
                 admin variable *name*; got `{field}`"
            );
            saw_key_var = true;
        }
    }
    assert!(
        saw_key_var,
        "`key_var` must be published (it is how an admin sees which variable a \
         provider reads) - its absence means the walk missed the provider view"
    );

    for props in objects_with_property(&doc, &["/b/llm"], "responses", "key_var") {
        assert_eq!(
            props["key_var"]["type"],
            serde_json::json!(["string", "null"]),
            "`key_var` must stay a nullable string naming a variable, never a \
             value-carrying object: {}",
            props["key_var"]
        );
    }
}

/// `vector` declared 11 endpoints and zero schemas: every handler
/// deserialized into a private in-function struct and answered with a
/// `serde_json::json!` literal, so `has_schema()` filtered all of them out.
/// The nine JSON endpoints below are the ones that carry a contract; the two
/// HTML pages must stay absent, because a schema is what turns an endpoint
/// into a tool.
#[tokio::test]
async fn vector_json_api_appears_in_openapi_and_its_pages_do_not() {
    let ctx = impresspress_core::test_support::TestContext::new().await;
    let doc = impresspress_core::test_support::openapi_document(&ctx).await;

    for (method, path) in [
        ("post", "/b/vector/api/indexes"),
        ("get", "/b/vector/api/indexes"),
        ("delete", "/b/vector/api/indexes/{name}"),
        ("post", "/b/vector/api/upsert"),
        ("post", "/b/vector/api/query"),
        ("post", "/b/vector/api/ingest"),
        ("post", "/b/vector/api/embed"),
        ("delete", "/b/vector/api/{index}/{id}"),
        ("get", "/b/vector/api/stats"),
    ] {
        assert!(
            !doc["paths"][path][method].is_null(),
            "{method} {path} must carry a schema and appear in /openapi.json"
        );
        assert_eq!(
            doc["paths"][path][method]["security"],
            serde_json::json!([{ "bearerAuth": [] }]),
            "{method} {path} is Authenticated and must carry a security requirement"
        );
    }

    for path in ["/b/vector/", "/b/vector/{name}/"] {
        assert!(
            doc["paths"][path].is_null(),
            "{path} serves HTML and must carry no schema - a schema would make it \
             a callable tool"
        );
    }

    // The create request is the one the admin modal also posts as a form,
    // where the checkbox arrives as the string `on`. The JSON contract must
    // type it as the boolean the handler stores, and require nothing but the
    // name.
    let create = &doc["paths"]["/b/vector/api/indexes"]["post"]["requestBody"]["content"]
        ["application/json"]["schema"];
    assert_eq!(create["properties"]["keyword_search"]["type"], "boolean");
    assert_eq!(create["required"], serde_json::json!(["name"]));
    // The metric variants carry doc comments, so schemars publishes them as
    // documented `const` alternatives rather than a bare `enum` list; the
    // tokens must still be exactly the backend's.
    let mut metric_tokens = const_strings(&create["properties"]["metric"]);
    metric_tokens.sort_unstable();
    assert_eq!(
        metric_tokens,
        ["cosine", "dotproduct", "euclidean"],
        "metric must publish the backend's exact tokens: {}",
        create["properties"]["metric"]
    );

    // A query is by text or by vector; the schema requires neither and the
    // handler refuses a body carrying neither.
    let query = &doc["paths"]["/b/vector/api/query"]["post"]["requestBody"]["content"]
        ["application/json"]["schema"];
    assert_eq!(query["required"], serde_json::json!(["index"]));
    for optional in ["text", "vector"] {
        assert!(
            query["properties"][optional]["type"]
                .as_array()
                .is_some_and(|t| t.contains(&serde_json::json!("null"))),
            "`{optional}` must be optional on the query request: {}",
            query["properties"][optional]
        );
    }
    // The handler accepts both — `vector` is then the query vector and
    // `text` only the keyword-query fallback — and refuses only neither.
    // The description must say that, not claim exclusivity.
    // Doc comments wrap at 80 columns, so compare with line breaks
    // collapsed to single spaces.
    let query_doc = query["description"]
        .as_str()
        .unwrap_or_default()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        query_doc.contains("At least one of `text` and `vector` is required"),
        "the query description must state the real contract: {query_doc}"
    );
    assert!(
        !query_doc.contains("Exactly one"),
        "the query description must not claim exclusivity: {query_doc}"
    );

    // A hit carries exactly id, score and the stored metadata.
    let hit = &doc["paths"]["/b/vector/api/query"]["post"]["responses"]["200"]["content"]
        ["application/json"]["schema"]["properties"]["matches"]["items"];
    let mut hit_keys: Vec<&str> = hit["properties"]
        .as_object()
        .expect("match properties")
        .keys()
        .map(String::as_str)
        .collect();
    hit_keys.sort_unstable();
    assert_eq!(hit_keys, ["id", "metadata", "score"]);

    // Both delete routes read their ids from the path and must say so.
    let param_names = |path: &str| -> Vec<String> {
        let mut names: Vec<String> = doc["paths"][path]["delete"]["parameters"]
            .as_array()
            .unwrap_or_else(|| panic!("delete {path} must declare path parameters"))
            .iter()
            .filter_map(|p| p["name"].as_str().map(str::to_string))
            .collect();
        names.sort_unstable();
        names
    };
    assert_eq!(param_names("/b/vector/api/indexes/{name}"), ["name"]);
    assert_eq!(param_names("/b/vector/api/{index}/{id}"), ["id", "index"]);
}

/// The vector API publishes index names, counts, caller-supplied metadata
/// and raw embeddings — none of it credential material — but every field
/// is still an explicit projection, and this pins that no name matching a
/// credential pattern ever enters it. `keyword_search` / `keyword_query`
/// are the reason the bare `key` pattern is not on the list.
#[tokio::test]
async fn vector_openapi_publishes_no_credential_field() {
    let ctx = impresspress_core::test_support::TestContext::new().await;
    let doc = impresspress_core::test_support::openapi_document(&ctx).await;
    let fields = published_field_names(&doc, &["/b/vector"]);

    assert!(
        !fields.is_empty(),
        "no vector fields found - the walk is looking in the wrong place and \
         this test would pass forever"
    );

    for field in &fields {
        let lower = field.to_lowercase();
        for forbidden in [
            "api_key",
            "apikey",
            "key_var",
            "secret",
            "password",
            "hash",
            "token",
            "bearer",
            "credential",
        ] {
            assert!(
                !lower.contains(forbidden),
                "vector publishes a field named `{field}`, which matches the \
                 credential pattern `{forbidden}`"
            );
        }
    }
}

/// `tickets` declared 21 endpoints and zero schemas, so `has_schema()` filtered
/// every one of them out and its JSON API was absent from `/openapi.json`. The
/// thirteen JSON endpoints below are the ones that carry a contract; the eight
/// that serve HTML (or redirect) must stay absent, because a schema is what
/// turns an endpoint into a tool.
#[tokio::test]
async fn tickets_json_api_appears_in_openapi_and_its_pages_do_not() {
    let ctx = impresspress_core::test_support::TestContext::new().await;
    let doc = impresspress_core::test_support::openapi_document(&ctx).await;

    for (method, path) in [
        ("post", "/b/tickets/api/submissions"),
        ("get", "/b/tickets/api/admin/tickets"),
        ("post", "/b/tickets/api/admin/tickets"),
        ("get", "/b/tickets/api/admin/tickets/{id}"),
        ("patch", "/b/tickets/api/admin/tickets/{id}"),
        ("post", "/b/tickets/api/admin/tickets/{id}/notes"),
        ("get", "/b/tickets/api/admin/tickets/{id}/analyses"),
        ("post", "/b/tickets/api/admin/tickets/{id}/analyses"),
        ("get", "/b/tickets/api/admin/types"),
        ("post", "/b/tickets/api/admin/types"),
        ("patch", "/b/tickets/api/admin/types/{id}"),
        ("get", "/b/tickets/api/admin/status"),
        ("post", "/b/tickets/api/admin/retention/prune"),
    ] {
        assert!(
            !doc["paths"][path][method].is_null(),
            "{method} {path} must carry a schema and appear in /openapi.json"
        );
    }

    for path in [
        "/b/tickets/submit",
        "/b/tickets/submitted",
        "/b/tickets/admin",
        "/b/tickets/admin/tickets",
        "/b/tickets/admin/tickets/{id}",
        "/b/tickets/admin/types",
        "/b/tickets/admin/settings",
        "/b/tickets/admin/endpoints",
    ] {
        assert!(
            doc["paths"][path].is_null(),
            "{path} serves HTML and must carry no schema - a schema would make it \
             a callable tool"
        );
    }
}

/// The ticket tables hold reporter-supplied text and one abuse digest, and the
/// untyped handlers echoed whole rows. Two invariants the projection exists to
/// hold, checked structurally rather than by reading the snapshot:
///
/// 1. No published field name matches the digest/credential pattern. That is
///    what keeps `dedupe_hash` — an HMAC over the reporter's IP-derived
///    rotating identity — out of every response.
/// 2. Reporter-controlled text is reachable only inside an untrusted-report
///    group, never flattened beside the workflow fields, so an agent client can
///    always tell data from instruction.
#[tokio::test]
async fn tickets_openapi_withholds_the_abuse_digest_and_groups_reporter_text() {
    let ctx = impresspress_core::test_support::TestContext::new().await;
    let doc = impresspress_core::test_support::openapi_document(&ctx).await;
    let fields = published_field_names(&doc, &["/b/tickets"]);

    assert!(
        !fields.is_empty(),
        "no tickets fields found - the walk is looking in the wrong place and \
         this test would pass forever"
    );

    // `…_secret_configured` on the readiness object is a boolean that reports
    // whether a secret is set, never a secret; it is the only field allowed to
    // match the pattern below.
    const READINESS_FLAGS: &[&str] = &["turnstile_secret_configured", "identity_secret_configured"];

    for field in &fields {
        if READINESS_FLAGS.contains(&field.as_str()) {
            continue;
        }
        let lower = field.to_lowercase();
        for forbidden in ["dedupe", "hash", "secret", "password", "remote_addr"] {
            assert!(
                !lower.contains(forbidden),
                "tickets publishes a field named `{field}`, which matches the \
                 withheld pattern `{forbidden}`"
            );
        }
    }

    for flag in READINESS_FLAGS {
        for props in objects_with_property(&doc, &["/b/tickets"], "responses", flag) {
            assert_eq!(
                props[*flag]["type"], "boolean",
                "`{flag}` must stay a boolean readiness flag, never carry a value"
            );
        }
    }

    // `reporter_email` is the marker: it belongs to exactly one shape, the
    // reporter-controlled group. Any other object carrying it would mean a raw
    // ticket row had been echoed again.
    let mut groups = 0;
    for props in objects_with_property(&doc, &["/b/tickets"], "responses", "reporter_email") {
        groups += 1;
        let mut keys: Vec<&str> = props.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "description",
                "evidence_url",
                "reporter_email",
                "reporter_wants_reply",
                "source_path",
                "subject",
                "subject_id",
                "subject_type",
            ],
            "reporter text must appear only as the untrusted-report group, but it \
             is flattened into an object with these keys: {keys:?}"
        );
    }
    assert!(
        groups > 0,
        "no untrusted-report group found - the walk is looking in the wrong place \
         and this test would pass forever"
    );
}

/// Every string `const` anywhere under `node` — the tokens a documented enum
/// publishes, whichever `oneOf` / `anyOf` nesting schemars wrapped them in.
fn const_strings(node: &serde_json::Value) -> Vec<String> {
    fn walk(node: &serde_json::Value, out: &mut Vec<String>) {
        match node {
            serde_json::Value::Object(map) => {
                if let Some(serde_json::Value::String(token)) = map.get("const") {
                    out.push(token.clone());
                }
                for value in map.values() {
                    walk(value, out);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    walk(item, out);
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(node, &mut out);
    out
}

/// Every `properties` map declaring `property` inside the `section` (e.g.
/// `"responses"`) of an operation under `prefixes`.
///
/// The section matters: a *request* body legitimately carries reporter text
/// flat - that is the reporter filling in the form - while a *response* that
/// does so is an echoed row.
fn objects_with_property<'a>(
    doc: &'a serde_json::Value,
    prefixes: &[&str],
    section: &str,
    property: &str,
) -> Vec<&'a serde_json::Map<String, serde_json::Value>> {
    fn walk<'a>(
        node: &'a serde_json::Value,
        property: &str,
        out: &mut Vec<&'a serde_json::Map<String, serde_json::Value>>,
    ) {
        match node {
            serde_json::Value::Object(map) => {
                if let Some(serde_json::Value::Object(props)) = map.get("properties") {
                    if props.contains_key(property) {
                        out.push(props);
                    }
                }
                for value in map.values() {
                    walk(value, property, out);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    walk(item, property, out);
                }
            }
            _ => {}
        }
    }

    let paths = doc["paths"].as_object().expect("openapi paths object");
    let mut out = Vec::new();
    for (path, node) in paths {
        if !prefixes.iter().any(|p| path.starts_with(p)) {
            continue;
        }
        for operation in node.as_object().into_iter().flat_map(|ops| ops.values()) {
            walk(&operation[section], property, &mut out);
        }
    }
    out
}

/// The public submission form carries a honeypot field. It works because a
/// bot that fills every field it can see fills that one too — which means it
/// must not be a field the schema can see. `deny_unknown_fields` on the JSON
/// contract forced it to be declared ("website — Must be left empty"),
/// publishing the bypass to every schema-reading caller.
#[tokio::test]
async fn tickets_openapi_does_not_publish_the_honeypot() {
    let ctx = impresspress_core::test_support::TestContext::new().await;
    let doc = impresspress_core::test_support::openapi_document(&ctx).await;

    let request = &doc["paths"]["/b/tickets/api/submissions"]["post"]["requestBody"]["content"]
        ["application/json"]["schema"];
    assert!(
        !request["properties"].is_null(),
        "the public submission request must stay documented: {doc}"
    );
    assert!(
        request["properties"]["website"].is_null(),
        "the honeypot must not be a published property: {request}"
    );
    assert_ne!(
        request["additionalProperties"],
        serde_json::json!(false),
        "unknown keys must be tolerated on this input or the honeypot cannot be read: {request}"
    );
}

/// Every `{...}` placeholder in a published path names an `in: path`
/// parameter declared on that path item or one of its operations, and every
/// `in: path` parameter has a placeholder in the template.
///
/// A trailing `...` on a placeholder is impresspress's matcher syntax for a
/// rest segment (`{key...}` binds the remainder of the path; see
/// `endpoint_match`), and the parameter it binds is named without the marker.
/// wafer-run's OpenAPI projection strips the marker when it publishes the row
/// (upstream #331): OpenAPI has no multi-segment parameter, so `{key...}` is
/// published as `{key}` naming the same parameter. So a published document
/// carries no marker at all, and the second half of this test pins that the
/// stripping happened rather than the row vanishing — see the anchor below.
#[tokio::test]
async fn path_placeholders_and_path_parameters_agree() {
    let ctx = impresspress_core::test_support::TestContext::new().await;
    let doc = impresspress_core::test_support::openapi_document(&ctx).await;
    let paths = doc["paths"].as_object().expect("openapi paths object");

    fn path_param_names(node: &serde_json::Value, out: &mut Vec<String>) {
        let Some(params) = node.get("parameters").and_then(|p| p.as_array()) else {
            return;
        };
        for p in params {
            if p["in"] == "path" {
                if let Some(name) = p["name"].as_str() {
                    out.push(name.to_string());
                }
            }
        }
    }

    let mut mismatches = Vec::new();
    for (path, item) in paths {
        let mut placeholders: Vec<String> = path
            .split('/')
            .filter_map(|seg| seg.strip_prefix('{')?.strip_suffix('}'))
            .map(str::to_string)
            .collect();
        assert!(
            placeholders.iter().all(|name| !name.ends_with("...")),
            "{path}: a published path key carries impresspress's rest marker, which no OpenAPI \
             client can fill — the projection must strip it"
        );
        placeholders.sort();
        placeholders.dedup();

        let mut declared = Vec::new();
        path_param_names(item, &mut declared);
        for (key, operation) in item.as_object().expect("path item object") {
            if key != "parameters" {
                path_param_names(operation, &mut declared);
            }
        }
        declared.sort();
        declared.dedup();

        if placeholders == declared {
            continue;
        }
        mismatches.push(format!(
            "{path}: template placeholders {placeholders:?} vs declared `in: path` parameters \
             {declared:?}"
        ));
    }

    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));

    // Anti-vacuity anchor.
    //
    // The old anchor demanded a published path key still containing a raw
    // `...`, which is exactly what a conformant document may no longer have:
    // it could only ever be satisfied by the bug #331 fixed. The invariant
    // worth pinning is the other half — that a rest-parameter route is
    // *published*, under its marker-stripped key, naming the same plain
    // parameter it declares. So the rest routes are read from the block
    // declarations (which still spell the marker, since that is impresspress's
    // own matcher syntax) and each is looked up in the document.
    //
    // This bites three ways: an empty `rest_routes` means the fixture no
    // longer contains a rest route and the check below proves nothing; a
    // missing path key means the projection stopped stripping (or dropped the
    // row); a missing parameter means it stripped the name as well as the key.
    let mut rest_routes: Vec<(String, String)> = Vec::new();
    for info in impresspress_core::test_support::real_block_infos() {
        for endpoint in &info.endpoints {
            // `generate_openapi` publishes only schema-carrying endpoints, so
            // a rest row without one (files' `DELETE …/objects/{key...}`) is
            // legitimately absent from the document.
            if !endpoint.has_schema() {
                continue;
            }
            let published: String = endpoint
                .path
                .split('/')
                .map(|seg| {
                    match seg
                        .strip_prefix('{')
                        .and_then(|n| n.strip_suffix('}'))
                        .and_then(|n| n.strip_suffix("..."))
                    {
                        Some(rest) => format!("{{{rest}}}"),
                        None => seg.to_string(),
                    }
                })
                .collect::<Vec<_>>()
                .join("/");
            for seg in endpoint.path.split('/') {
                let Some(name) = seg.strip_prefix('{').and_then(|n| n.strip_suffix('}')) else {
                    continue;
                };
                if let Some(rest) = name.strip_suffix("...") {
                    rest_routes.push((published.clone(), rest.to_string()));
                }
            }
        }
    }
    rest_routes.sort();
    rest_routes.dedup();

    assert!(
        !rest_routes.is_empty(),
        "expected at least one schema-carrying route declared with a rest segment (files' \
         `GET /b/storage/api/buckets/{{name}}/objects/{{key...}}`), or the check below proves \
         nothing about how a rest parameter is published"
    );

    for (published, param) in &rest_routes {
        let item = paths.get(published).unwrap_or_else(|| {
            panic!(
                "a rest-segment route declared with `{{{param}...}}` must be published under its \
                 marker-stripped key `{published}`; published keys: {:?}",
                paths.keys().collect::<Vec<_>>()
            )
        });
        let mut declared = Vec::new();
        path_param_names(item, &mut declared);
        for (key, operation) in item.as_object().expect("path item object") {
            if key != "parameters" {
                path_param_names(operation, &mut declared);
            }
        }
        assert!(
            declared.iter().any(|name| name == param),
            "`{published}` must declare its rest segment as the plain `in: path` parameter \
             `{param}`; declared: {declared:?}"
        );
    }
}
