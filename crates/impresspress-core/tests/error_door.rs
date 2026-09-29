//! A failed database call has exactly one mapping.
//!
//! `blocks::crud::db_error` decides what a `WaferError` from the database
//! client turns into: `NotFound` is the caller's 404, `PermissionDenied` is a
//! **403**, `ResourceExhausted` keeps its 429, everything else is the
//! sanitized 500 with the cause logged. The shape it replaces —
//!
//! ```ignore
//! Err(e) if e.code == ErrorCode::NotFound => err_not_found("X not found"),
//! Err(e) => err_internal("Database error", e),
//! ```
//!
//! — appeared at 62 sites across 27 files, and **not one of them re-checked
//! `PermissionDenied`**. So a block deployed without the `ResourceGrant` its
//! handler needs answered `500 Internal server error (ref: …)`, which an
//! operator cannot tell from a corrupt row, and a caller cannot tell from an
//! outage. That is the regression this gate exists to stop: the shape is easy
//! to write, reads as careful, and silently loses the one code that matters.
//!
//! The gate is a source scan because there is nothing else it could be. The
//! ingredients (`ErrorCode::NotFound`, `err_not_found`, `err_internal`) are
//! all legitimately public, so no type system or lint can see the
//! combination; only reading the source can.
//!
//! Scope: every `.rs` file under `src/blocks/`, with full-line comments
//! removed first (prose describing the shape is not the shape — this file's
//! own doc comment would otherwise fail it) and with every `#[cfg(test)]`
//! item removed (a test asserting on the old behaviour is not a handler
//! producing it). Trailing comments on code lines are kept, so nothing hides
//! behind a `//` on the same line as code.
//!
//! What the gate does NOT see, stated so it is not mistaken for more than it
//! is: a handler that writes the `NotFound` arm and the `err_internal` tail
//! more than six lines apart; a handler whose tail is something other than
//! `err_internal` (`ui::server_error_response`, say); and a handler with no
//! `NotFound` arm at all, whose bare `err_internal` tail turns a refusal
//! into a 500 just as quietly — PR 2 found four of those in
//! `tickets/rest.rs` and two in `vector/pages.rs` only by reading the files
//! the allowlist sent it to.
//!
//! That last blind spot is closed for every block in `GATED_BLOCKS` — the
//! whole of `admin`, `auth`, `auth_ui`, `dev`, `files`, `legalpages`, `llm`,
//! `messages`, `products`, `signal`, `tickets`, `userportal` and `vector`,
//! and the top-level `email.rs` and `fastembed.rs`: every block that calls
//! `err_internal` at all. Whether an
//! `err_internal(label, cause)` there wraps a database call is a reading job
//! per site, so the second gate below does not guess: every `err_internal`
//! tail left in a gated file is inventoried by its label, with the reason it
//! is not a database failure, and a tail that is not on the inventory fails.
//! A third gate stops the inventory being walked around: in a gated file
//! `err_internal` may only be called by name, never renamed, stored, wrapped
//! in a macro, or wrapped in a function, trait default method or bound
//! closure that forwards its caller's error. A block that is not gated may
//! not call `err_internal` at all: `NOT_YET_GATED` is empty, and a block that
//! starts to must be gated or listed there.
//!
//! What the inventory does NOT see, in a gated file or anywhere: a database
//! failure answered through `err_internal_no_cause` or
//! `ui::server_error_response`, which take no cause — its code is gone before
//! the call. The inventory covers `err_internal(label, cause)` only; those two
//! are counted per block in `CAUSE_DROPPED` instead, exactly, with the plan
//! item that reads them, so a new one is at least an edit a reviewer sees.
//!
//! A failure printed into a page as its own text (`"Failed to load …: " (e)`)
//! is a third shape that neither sees: it answers 200, and a WRAP denial's
//! grant and table names reach the page. `ERROR_TEXT_RENDERED` counts it per
//! block, exactly, and it is empty.
//!
//! `auth::repo::RepoError` used to be named here as a site the gate could
//! not help: it was `NotFound | Db(String)`, so the wafer code was gone
//! before a handler ever saw it. PR 2 folded it into `WaferError`, and those
//! sites classify like every other one now.

use impresspress_core::test_support::source_scan::{
    strip_line_comments, strip_test_modules, SourceWalk,
};

/// Files still carrying the shape. **Empty**, and the history of how it got
/// there, because each entry was a place a WRAP refusal shipped as a 500 and
/// the order they came off in is the argument for keeping it at zero.
///
/// PR 1 converted the seven sites inside `blocks/crud.rs` — which is what
/// makes the fix reach every block that reads through the CRUD primitives —
/// plus `products/handlers/sellers.rs`, `admin/settings.rs` and
/// `legalpages/mod.rs`'s two `Result<Option<_>>` handlers.
///
/// PR 2 folded `auth::repo::RepoError` into `WaferError` and took the seven
/// entries it had marked for itself off this list: `admin/{ops,mod,iam}.rs`,
/// `vector/pages.rs`, `tickets/{rest,pages}.rs` and
/// `dev/generations_api.rs`, the last through the `dev::no_store_db_error`
/// its entry called for.
///
/// PR 3 took its ten: `messages/{rest,pages}.rs`, `legalpages/{mod,pages}.rs`,
/// `files/{share,cloud}.rs`, `files/storage/{objects,buckets}.rs`,
/// `llm/mod.rs` and `llm/routes/providers.rs`. Four of those are STORAGE
/// calls rather than database ones (`files/share.rs` and both
/// `files/storage/*`); the codes are the same set and the mapping is the
/// same sentence, so they go through the same door. Only the eight products
/// entries were left after it.
///
/// PR 4 took **none** of them, deliberately. The entries were written on the
/// assumption that the enum work would open these files anyway, and it did —
/// but it opened them to move two published snapshots, the SDK's order and
/// seller types and every products status column at once. Folding a second,
/// unrelated behaviour change (a WRAP denial stops answering 500) into that
/// review would have hidden it. The eight went as their own PR, which is a
/// mechanical diff with a behavioural test and no snapshot movement at all.
///
/// PR 5 (`StripeEventType`) opened two of the eight — `products/stripe.rs`
/// and `products/pages.rs` — and took none either, for the same reason.
///
/// That PR has now landed and the list is **empty**. All 29 `NotFound`
/// classifications across the eight products files classify through
/// `crud::db_error` / `crud::db_error_internal`, or through one of the three
/// block-private helpers (`handlers::product::write_error`,
/// `handlers::offers::domain_error`, `handlers::provider::provider_error`)
/// whose tails now delegate to it while keeping their own domain arms.
/// `products/tests/error_mapping_tests.rs` is the behavioural half: a real
/// `wrap::check_access` denial per file, each paired with the 404 a granted
/// read of a missing row still gives.
///
/// An empty list is the invariant, not a milestone: a file that hand-maps a
/// database error fails this test, and re-listing one takes an edit here and
/// the review that comes with it.
const STILL_HAND_MAPPED: &[(&str, &str)] = &[];

/// The one file allowed to contain the mapping, because it IS the mapping.
const THE_DOOR: &str = "crud.rs";

/// The walk this gate runs over: every block source, with a floor so an empty
/// scan cannot pass as a clean one.
fn scan() -> SourceWalk {
    SourceWalk::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src/blocks")).least(100)
}

/// `src` as production code: every `#[cfg(test)]` item dropped, and full-line
/// comments with it.
fn production_code(src: &str) -> Vec<String> {
    strip_line_comments(&strip_test_modules(src))
        .lines()
        .map(str::to_string)
        .collect()
}

/// Whether `line` CLASSIFIES an error as `NotFound`, rather than merely
/// naming the variant.
///
/// The two spellings a handler uses are the match guard
/// (`Err(e) if e.code == ErrorCode::NotFound =>`) and the bare arm of a
/// `match error.code` (`ErrorCode::NotFound =>`). Passing the variant as an
/// argument — `no_store_error(ErrorCode::NotFound, …)`, which constructs a
/// refusal rather than classifying one — is not the shape, and
/// `blocks/dev/files.rs` is why this distinction is drawn: it answers a
/// missing manifest entry with a constructed `NotFound` and, five lines
/// later, `err_internal`s an unrelated blob read.
fn classifies_as_not_found(line: &str) -> bool {
    let trimmed = line.trim();
    (trimmed.contains(".code ==") && trimmed.contains("ErrorCode::NotFound"))
        || trimmed.starts_with("ErrorCode::NotFound =>")
}

/// Whether `lines` pairs a `NotFound` classification with an `err_internal`
/// tail within six lines — the window the shape occupies wherever it appears.
fn hand_maps_a_database_error(lines: &[String]) -> bool {
    lines.iter().enumerate().any(|(i, line)| {
        classifies_as_not_found(line)
            && lines[i..(i + 7).min(lines.len())]
                .iter()
                .any(|window| window.contains("err_internal"))
    })
}

/// The files that carry the shape and are not allowed to, and the entries on
/// the list that no longer carry it — the gate's whole verdict, over whatever
/// tree `walk` reaches.
fn verdict(walk: &SourceWalk) -> (Vec<String>, Vec<String>) {
    let allowed: std::collections::HashMap<&str, &str> =
        STILL_HAND_MAPPED.iter().copied().collect();

    let mut unexpected = Vec::new();
    let mut clean_but_listed = Vec::new();

    for file in walk.collect() {
        if file.rel == THE_DOOR {
            continue;
        }
        let hits = hand_maps_a_database_error(&production_code(&file.text));
        match (hits, allowed.contains_key(file.rel.as_str())) {
            (true, false) => unexpected.push(file.rel),
            (false, true) => clean_but_listed.push(file.rel),
            _ => {}
        }
    }

    unexpected.sort();
    clean_but_listed.sort();
    (unexpected, clean_but_listed)
}

#[test]
fn only_crud_maps_a_database_error_by_hand() {
    let (unexpected, clean_but_listed) = verdict(&scan());

    assert!(
        unexpected.is_empty(),
        "these files hand-map a database error instead of calling \
         `crud::db_error`, so a WRAP `PermissionDenied` ships from them as a \
         500: {unexpected:?}\n\
         Use `crud::db_error(error, \"X not found\", \"Database error\")`. If \
         the site genuinely cannot — a block whose responses all carry a \
         header, the way `blocks::dev` does — classify through \
         `crud::classify_db_error` and seal it yourself, as \
         `dev::no_store_db_error` does. Otherwise add it to \
         STILL_HAND_MAPPED with the PR that converts it."
    );

    assert!(
        clean_but_listed.is_empty(),
        "these files are on STILL_HAND_MAPPED but no longer hand-map \
         anything; take them off the list so it stays a worklist: \
         {clean_but_listed:?}"
    );
}

/// The gate can actually fail. A test that only ever passes proves nothing,
/// and this one's whole value is the day someone re-introduces the shape.
#[test]
fn the_gate_catches_the_shape_it_is_looking_for() {
    let offending = production_code(
        r#"
        match db::get(ctx, TABLE, id).await {
            Ok(row) => ok_json(&row),
            Err(e) if e.code == ErrorCode::NotFound => err_not_found("Thing not found"),
            Err(e) => err_internal("Database error", e),
        }
        "#,
    );
    assert!(hand_maps_a_database_error(&offending));

    let converted = production_code(
        r#"
        match db::get(ctx, TABLE, id).await {
            Ok(row) => ok_json(&row),
            Err(e) => crud::db_error(e, "Thing not found", "Database error"),
        }
        "#,
    );
    assert!(!hand_maps_a_database_error(&converted));

    // Prose describing the shape is not the shape.
    let prose = production_code(
        r#"
        // Err(e) if e.code == ErrorCode::NotFound => ...
        // Err(e) => err_internal("Database error", e),
        "#,
    );
    assert!(!hand_maps_a_database_error(&prose));

    // Neither is a test asserting on it.
    let in_a_test = production_code(
        r#"
        pub fn handler() {}

        #[cfg(test)]
        mod tests {
            Err(e) if e.code == ErrorCode::NotFound => err_not_found("x"),
            Err(e) => err_internal("Database error", e),
        }
        "#,
    );
    assert!(!hand_maps_a_database_error(&in_a_test));

    // …but a handler BELOW one still is. `#[cfg(test)]` is not only the
    // trailing `mod tests`: seventeen files in this crate carry it on an early
    // `mod test_support;`, a `use` or a fixture `fn`, and a scope that ran to
    // the first attribute and stopped saw 15 of `blocks/products/mod.rs`'s 422
    // lines. Everything past it was un-gated, and this is the case that says
    // so.
    let after_a_test_module = production_code(
        r#"
        #[cfg(test)]
        mod tests {
            fn nothing() {}
        }

        pub fn handler() {
            Err(e) if e.code == ErrorCode::NotFound => err_not_found("x"),
            Err(e) => err_internal("Database error", e),
        }
        "#,
    );
    assert!(hand_maps_a_database_error(&after_a_test_module));
}

/// The *walk* reaches a planted offender, and the door it exempts is the only
/// thing it lets through.
///
/// `the_gate_catches_the_shape_it_is_looking_for` proves the predicate works;
/// it says nothing about whether the walk ever opens a file. A root that moved
/// or an extension filter that broke would leave the gate above passing on an
/// empty scan — green, and blind to the one thing it exists to catch.
#[test]
fn the_walk_reaches_the_files_it_claims_to_scan() {
    const SHAPE: &str = "match db::get(ctx, TABLE, id).await {\n\
         Err(e) if e.code == ErrorCode::NotFound => err_not_found(\"x\"),\n\
         Err(e) => err_internal(\"Database error\", e),\n\
         }\n";

    let root = std::env::temp_dir().join(format!("error-door-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("nested")).expect("temp tree");
    std::fs::write(root.join("nested/offender.rs"), SHAPE).expect("offender");
    std::fs::write(root.join(THE_DOOR), SHAPE).expect("the door");
    std::fs::write(root.join("notes.txt"), SHAPE).expect("non-rust");

    let (unexpected, clean_but_listed) = verdict(&SourceWalk::new(&root));
    std::fs::remove_dir_all(&root).expect("clean up");

    assert_eq!(
        unexpected,
        vec!["nested/offender.rs".to_string()],
        "expected exactly the planted offender"
    );
    assert!(clean_but_listed.is_empty(), "{clean_but_listed:?}");
}

// ---------------------------------------------------------------------------
// Inventoried `err_internal` tails
// ---------------------------------------------------------------------------

/// The blocks whose every file is held to [`INVENTORIED_TAILS`] and to
/// [`evasions`]: a file under one of these that is not on the inventory may
/// have no `err_internal` tail at all. A top-level file under `src/blocks/`
/// (`email.rs`) is its own entry, as in [`NOT_YET_GATED`]. Test code
/// (`tests/` directories and `#[cfg(test)]` items) is not gated.
const GATED_BLOCKS: &[&str] = &[
    "admin",
    "auth",
    "auth_ui",
    "dev",
    "email.rs",
    "fastembed.rs",
    "files",
    "legalpages",
    "llm",
    "messages",
    "products",
    "signal",
    "tickets",
    "userportal",
    "vector",
];

/// The walk the gated-file checks run over: every block source outside a
/// `tests/` directory, floored like [`scan`].
fn gated_scan() -> SourceWalk {
    SourceWalk::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src/blocks"))
        .skip_dir("tests")
        .least(100)
}

/// The gated block `rel` belongs to, if any: a file under a gated block's
/// directory, or a gated top-level file itself.
fn gated_block(rel: &str) -> Option<&'static str> {
    GATED_BLOCKS.iter().copied().find(|block| {
        rel == *block
            || rel
                .strip_prefix(block)
                .is_some_and(|rest| rest.starts_with('/'))
    })
}

/// Every `err_internal(label, cause)` left in a gated file, by label, with how
/// many times it appears and why it is not a database failure. A gated file
/// that is not listed here has an empty inventory.
///
/// A database or storage failure in these files goes through
/// `crud::db_error_internal` (or `crud::db_error` where a `NotFound` is the
/// caller's row, `crud::db_error_page` for a full page, or
/// `dev::no_store_db_error_internal` in the dev block), so a WRAP denial is 403
/// and a quota is 429. What is left here is one of five things:
///
/// - **Stripe**: the cause is a Stripe API call. `stripe_client::classify`
///   gives it `Internal` or `FailedPrecondition`, never a WRAP code, and a
///   Stripe 429 is Stripe's rate limit, not the database's.
/// - **Provider**: the cause is an outside party's answer — an OAuth
///   provider's token or userinfo endpoint, an LLM provider behind the
///   router (an `LlmError`, which carries no database code), or an embedding
///   block's reply that broke its own contract.
/// - **Crypto**: the cause is the `wafer-run/crypto` service hashing a
///   password or drawing random bytes, which reads no table.
/// - **Invariant**: the cause is this process, not a service — a row outside
///   its contract, a setting read successfully but outside its range (a
///   failed config read goes through the door like a database one), a
///   serialization or the OS RNG.
/// - **Classified**: the cause already went through
///   `crud::classify_db_error` and came back `Internal` — its WRAP denials,
///   quotas and duplicate keys were answered before this call.
///   `dev::seal_no_store` is the one: it seals the dev block's failures with
///   `Cache-Control: no-store`, which `crud`'s own sealer cannot add.
///
/// The gate compares counts both ways. A label that is not listed, or that
/// appears more often than listed, is a new tail: route it through the door
/// or add it here with its reason. A label that appears less often than
/// listed is a stale entry: lower the count or delete the line, so the list
/// never grants more than the files use.
///
/// The behavioural half is fault injection through `FailingDbOpContext`:
/// `webhook_database_denial_is_403_and_the_delivery_is_retried` and its
/// siblings in `products/tests/stripe_tests.rs`,
/// `refund_ledger_denial_is_403` and its siblings in
/// `products/tests/provider_tests.rs`, one real route per products file in
/// `products/tests/error_mapping_tests.rs`, one real route per auth handler
/// family in `auth_ui/tests/error_mapping_tests.rs`, the OAuth callback's
/// `state_redemption_denial_is_403_not_500`, and the admin and user-portal
/// routes in `admin/error_mapping_tests.rs` and
/// `userportal/error_mapping_tests.rs`, and one real route or page per
/// converted site in `llm/error_mapping_tests.rs`,
/// `vector/error_mapping_tests.rs`, `messages/error_mapping_tests.rs` and
/// `legalpages/error_mapping_tests.rs`, `signal`'s
/// `a_refused_room_store_is_403` in `signal/mod.rs`, one real route per
/// converted site in `files/error_mapping_tests.rs` and
/// `dev/error_mapping_tests.rs` (storage refusals through
/// `FailingServiceOpContext`), and `tickets`'
/// `refused_list_pages_are_the_403_page` in `tickets/pages.rs`.
const INVENTORIED_TAILS: &[(&str, &[Tail])] = &[
    (
        "products/stripe.rs",
        &[
            Tail {
                label: "Stripe API error",
                count: 1,
                why: "Stripe: the Checkout Session create",
            },
            Tail {
                label: "Platform application fee is misconfigured",
                count: 1,
                why: "invariant: the fee setting is outside 0..=10000",
            },
            Tail {
                label: "Platform country is misconfigured",
                count: 1,
                why: "invariant: the country setting is not a two-letter code",
            },
            Tail {
                label: "Could not snapshot checkout inputs",
                count: 1,
                why: "invariant: serializing the evaluated inputs",
            },
            Tail {
                label: "Could not snapshot checkout condition",
                count: 1,
                why: "invariant: serializing a component condition",
            },
            Tail {
                label: "Could not create checkout receipt",
                count: 1,
                why: "invariant: the OS random source",
            },
            Tail {
                label: "Purchase row is outside the contract",
                count: 1,
                why: "invariant: an undecodable order status",
            },
        ],
    ),
    (
        "products/purchase.rs",
        &[
            Tail {
                label: "Stripe refund could not be completed",
                count: 1,
                why: "Stripe: the refund create",
            },
            Tail {
                label: "Order row is outside the contract",
                count: 4,
                why: "invariant: an undecodable order row",
            },
            Tail {
                label: "Refund row is outside the contract",
                count: 1,
                why: "invariant: an undecodable refund status",
            },
            Tail {
                label: "&format!(\"{entity} row is outside the contract\")",
                count: 1,
                why: "invariant: an undecodable child row",
            },
            Tail {
                label: "Purchase has invalid refund accounting",
                count: 1,
                why: "invariant: the order's own totals disagree",
            },
        ],
    ),
    (
        "products/pages.rs",
        &[
            Tail {
                label: "Platform application fee is misconfigured",
                count: 1,
                why: "invariant: the fee setting is outside 0..=10000",
            },
            Tail {
                label: "Platform country is misconfigured",
                count: 1,
                why: "invariant: the country setting is not a two-letter code",
            },
            Tail {
                label: "Seller account error",
                count: 2,
                why: "invariant: an undecodable seller account row",
            },
        ],
    ),
    (
        "products/handlers/commerce.rs",
        &[
            Tail {
                label: "Could not encode storefront config",
                count: 1,
                why: "invariant: serializing the storefront config",
            },
            Tail {
                label: "Order has invalid currency",
                count: 1,
                why: "invariant: a stored order currency that is not a currency",
            },
            Tail {
                label: "Order row is outside the contract",
                count: 1,
                why: "invariant: an undecodable order or reconciliation status",
            },
            Tail {
                label: "Could not encode order status",
                count: 1,
                why: "invariant: serializing the guest order status",
            },
            Tail {
                label: "Could not decode product",
                count: 2,
                why: "invariant: undecodable product tags or fulfillment kind",
            },
        ],
    ),
    (
        "products/handlers/sellers.rs",
        &[
            Tail {
                label: "&format!(\"{outcome}{SELLER_APPLICATION_FEE_BPS} cannot be read\")",
                count: 1,
                why: "invariant: the fee setting is outside 0..=10000",
            },
            Tail {
                label: "Product row is outside the contract",
                count: 3,
                why: "invariant: an undecodable product row",
            },
        ],
    ),
    (
        "products/handlers/product.rs",
        &[Tail {
            label: "Product row is outside the contract",
            count: 3,
            why: "invariant: an undecodable product row",
        }],
    ),
    (
        "products/handlers/catalog.rs",
        &[Tail {
            label: "Product row is outside the contract",
            count: 1,
            why: "invariant: an undecodable product row",
        }],
    ),
    (
        "products/handlers/provider.rs",
        &[Tail {
            label: "Provider operation row is outside the contract",
            count: 1,
            why: "invariant: an undecodable provider operation row",
        }],
    ),
    (
        "auth_ui/api/signup.rs",
        &[Tail {
            label: "Failed to generate verification token",
            count: 1,
            why: "crypto: drawing the verification token",
        }],
    ),
    (
        "auth_ui/api/api_keys.rs",
        &[Tail {
            label: "Failed to generate key",
            count: 1,
            why: "crypto: drawing the key's random bytes",
        }],
    ),
    (
        "auth_ui/api/change_password.rs",
        &[Tail {
            label: "Stored password hash could not be checked",
            count: 1,
            why: "crypto: the Internal a stored hash it cannot check gets; every \
                      other compare_hash failure is classified by check_password",
        }],
    ),
    (
        "auth/mod.rs",
        &[Tail {
            label: "Failed to hash password",
            count: 1,
            why: "crypto: `hash_new_password` hashing a password being set; an \
                  unreachable hasher is a 503 first",
        }],
    ),
    (
        "auth_ui/oauth/start.rs",
        &[
            Tail {
                label: "Failed to generate PKCE verifier",
                count: 1,
                why: "invariant: the OS random source",
            },
            Tail {
                label: "Failed to generate state",
                count: 1,
                why: "invariant: the OS random source",
            },
        ],
    ),
    (
        "auth_ui/oauth/callback.rs",
        &[
            Tail {
                label: "Token exchange failed",
                count: 1,
                why: "provider: the token endpoint request",
            },
            Tail {
                label: "User info request failed",
                count: 1,
                why: "provider: the userinfo endpoint request",
            },
            Tail {
                label: "Failed to parse OAuth user info",
                count: 1,
                why: "provider: an undecodable userinfo body",
            },
        ],
    ),
    (
        "llm/routes/providers.rs",
        &[
            Tail {
                label: "Stored provider row invalid",
                count: 2,
                why: "invariant: an undecodable provider row",
            },
            Tail {
                label: "context",
                count: 1,
                why: "provider: `llm_error_response`'s `LlmError` from the provider router",
            },
        ],
    ),
    (
        "vector/pages.rs",
        &[Tail {
            label: "embedding/chunk count mismatch",
            count: 1,
            why: "provider: the embedding block answered a vector count unlike its chunk count",
        }],
    ),
    (
        "messages/rest.rs",
        &[Tail {
            label: "add_entry card render failed",
            count: 1,
            why: "invariant: an undecodable entry row the insert just returned",
        }],
    ),
    (
        "fastembed.rs",
        &[Tail {
            label: "fastembed service unavailable",
            count: 1,
            why: "invariant: loading the embedding model into this process",
        }],
    ),
    (
        "files/share.rs",
        &[Tail {
            label: "Token generation failed",
            count: 1,
            why: "crypto: drawing the share token's random bytes",
        }],
    ),
    (
        "dev/export.rs",
        &[
            Tail {
                label: "dev export archive",
                count: 1,
                why: "invariant: writing the zip into memory",
            },
            Tail {
                label: "dev export shell",
                count: 1,
                why:
                    "provider: `ShellSource` answers the host's static shell as a `String` error, \
                      a type with no database code",
            },
        ],
    ),
    (
        "dev/mod.rs",
        &[Tail {
            label: "context",
            count: 1,
            why: "classified: `seal_no_store`'s `DbFailure::Internal` arm, after \
                  `crud::classify_db_error` answered WRAP denials, quotas and \
                  duplicate keys",
        }],
    ),
];

/// One inventoried tail: its label as [`err_internal_labels`] reads it, how
/// many times the file uses it, and why it is not a database failure —
/// `"Stripe: …"`, `"provider: …"`, `"crypto: …"`, `"invariant: …"` or
/// `"classified: …"`, the only reasons there are.
struct Tail {
    label: &'static str,
    count: usize,
    why: &'static str,
}

/// A label whose use disagrees with its inventory: `(label, used, listed)`.
type Miscount = (String, usize, usize);

/// The first argument of every `err_internal` call in `src`'s production
/// code: a string literal without its quotes, any other expression as
/// [`render`] spells it.
fn err_internal_labels(src: &str) -> Vec<String> {
    call_arguments(src, "err_internal")
}

/// The first argument of every call of the free function `name` in `src`'s
/// production code, in source order.
///
/// Read from the token stream, not the text. A call is the identifier `name`,
/// then an optional turbofish (`err_internal::<WaferError>`), then a
/// parenthesized argument list; `err_internal_no_cause` and
/// `crud::db_error_internal` are different identifiers, a comment is not a
/// token, and a string that mentions the name is one literal. Calls inside a
/// macro invocation (`fail_webhook!(err_internal(..))`, a `vec![..]`) are in
/// its tokens and count like any other. `#[cfg(test)]` items and statements
/// are dropped by [`production_tokens`] first.
fn call_arguments(src: &str, name: &str) -> Vec<String> {
    use proc_macro2::{Delimiter, TokenStream, TokenTree};

    fn is_punct(tree: Option<&TokenTree>, ch: char) -> bool {
        matches!(tree, Some(TokenTree::Punct(p)) if p.as_char() == ch)
    }

    fn scan(stream: TokenStream, name: &str, found: &mut Vec<String>) {
        let trees: Vec<TokenTree> = stream.into_iter().collect();
        for (i, tree) in trees.iter().enumerate() {
            match tree {
                TokenTree::Group(group) => scan(group.stream(), name, found),
                TokenTree::Ident(ident) if ident == name => {
                    // A definition, not a call.
                    if i > 0 && matches!(&trees[i - 1], TokenTree::Ident(kw) if kw == "fn") {
                        continue;
                    }
                    let mut next = i + 1;
                    if is_punct(trees.get(next), ':')
                        && is_punct(trees.get(next + 1), ':')
                        && is_punct(trees.get(next + 2), '<')
                    {
                        next += 3;
                        let mut depth = 1;
                        while depth > 0 && next < trees.len() {
                            match &trees[next] {
                                TokenTree::Punct(p) if p.as_char() == '<' => depth += 1,
                                // `->` inside the turbofish is an arrow, not a close.
                                TokenTree::Punct(p)
                                    if p.as_char() == '>'
                                        && !is_punct(trees.get(next - 1), '-') =>
                                {
                                    depth -= 1
                                }
                                _ => {}
                            }
                            next += 1;
                        }
                    }
                    if let Some(TokenTree::Group(args)) = trees.get(next) {
                        if args.delimiter() == Delimiter::Parenthesis {
                            found.push(first_argument(args.stream()));
                        }
                    }
                }
                _ => {}
            }
        }
    }

    let mut found = Vec::new();
    scan(production_tokens(src), name, &mut found);
    found
}

/// `src`'s tokens with every `#[cfg(test)]` item and statement removed, at
/// any depth.
///
/// Where the test item ends is decided by syn's own item and statement
/// parsers, not by a brace count: a `#[cfg(test)]` on an early
/// `mod test_support;`, a `use`, a fixture `fn` or a `let` drops exactly that
/// one thing, and the production code after it is still scanned. Comments are
/// not tokens, and doc comments become string literals, so neither can
/// contribute a call.
fn production_tokens(src: &str) -> proc_macro2::TokenStream {
    use proc_macro2::{Group, TokenStream, TokenTree};
    use syn::parse::{ParseStream, Parser};

    fn is_cfg_test(attr: &syn::Attribute) -> bool {
        attr.path().is_ident("cfg")
            && matches!(&attr.meta, syn::Meta::List(list) if list.tokens.to_string() == "test")
    }

    fn strip(input: ParseStream) -> syn::Result<TokenStream> {
        let mut out = TokenStream::new();
        while !input.is_empty() {
            if input.peek(syn::Token![#]) && !input.peek2(syn::Token![!]) {
                let is_test = input
                    .fork()
                    .call(syn::Attribute::parse_outer)
                    .is_ok_and(|attrs| attrs.iter().any(is_cfg_test));
                if is_test {
                    if input.fork().parse::<syn::Item>().is_ok() {
                        input.parse::<syn::Item>()?;
                        continue;
                    }
                    if input.fork().parse::<syn::Stmt>().is_ok() {
                        input.parse::<syn::Stmt>()?;
                        continue;
                    }
                }
            }
            match input.parse::<TokenTree>()? {
                TokenTree::Group(group) => {
                    let mut kept = Group::new(group.delimiter(), strip.parse2(group.stream())?);
                    kept.set_span(group.span());
                    out.extend([TokenTree::Group(kept)]);
                }
                other => out.extend([other]),
            }
        }
        Ok(out)
    }

    let tokens: TokenStream = src
        .parse()
        .unwrap_or_else(|error| panic!("untokenizable source: {error}"));
    strip
        .parse2(tokens)
        .unwrap_or_else(|error| panic!("unparseable source: {error}"))
}

/// A call's first argument: the tokens before the first top-level comma. A
/// lone string literal is its contents, without the quotes; anything else is
/// [`render`]ed.
fn first_argument(args: proc_macro2::TokenStream) -> String {
    use proc_macro2::TokenTree;

    let first: Vec<TokenTree> = args
        .into_iter()
        .take_while(|tree| !matches!(tree, TokenTree::Punct(p) if p.as_char() == ','))
        .collect();
    if let [TokenTree::Literal(literal)] = first.as_slice() {
        let text = literal.to_string();
        if let Some(inner) = text.strip_prefix('"').and_then(|t| t.strip_suffix('"')) {
            return inner.to_string();
        }
    }
    render(first)
}

/// Tokens as text, spaced only where two words would otherwise run together:
/// `&format!("{entity} row is outside the contract")` renders as written.
fn render(trees: impl IntoIterator<Item = proc_macro2::TokenTree>) -> String {
    use proc_macro2::{Delimiter, TokenTree};

    let mut out = String::new();
    let mut last_was_word = false;
    for tree in trees {
        let is_word = matches!(tree, TokenTree::Ident(_) | TokenTree::Literal(_));
        if is_word && last_was_word {
            out.push(' ');
        }
        match tree {
            TokenTree::Group(group) => {
                let (open, close) = match group.delimiter() {
                    Delimiter::Parenthesis => ("(", ")"),
                    Delimiter::Bracket => ("[", "]"),
                    Delimiter::Brace => ("{", "}"),
                    Delimiter::None => ("", ""),
                };
                out.push_str(open);
                out.push_str(&render(group.stream()));
                out.push_str(close);
            }
            other => out.push_str(&other.to_string()),
        }
        last_was_word = is_word;
    }
    out
}

/// `src` against its inventory: the labels used more often than listed (new
/// tails), and the labels listed more often than used (stale entries), each
/// with the two counts.
fn tails_verdict(src: &str, inventory: &[Tail]) -> (Vec<Miscount>, Vec<Miscount>) {
    let mut used: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for label in err_internal_labels(src) {
        *used.entry(label).or_default() += 1;
    }
    let listed: std::collections::BTreeMap<String, usize> = inventory
        .iter()
        .map(|tail| (tail.label.to_string(), tail.count))
        .collect();
    assert_eq!(
        listed.len(),
        inventory.len(),
        "a label is listed twice in its file's inventory"
    );

    let unlisted = used
        .iter()
        .filter_map(|(label, &n)| {
            let allowed = listed.get(label).copied().unwrap_or(0);
            (n > allowed).then(|| (label.clone(), n, allowed))
        })
        .collect();
    let stale = listed
        .iter()
        .filter_map(|(label, &allowed)| {
            let n = used.get(label).copied().unwrap_or(0);
            (n < allowed).then(|| (label.clone(), n, allowed))
        })
        .collect();
    (unlisted, stale)
}

#[test]
fn gated_files_tail_only_inventoried_non_database_failures() {
    let files = gated_scan().collect();
    for (rel, inventory) in INVENTORIED_TAILS {
        assert!(
            gated_block(rel).is_some(),
            "{rel} is inventoried but not under a GATED_BLOCKS block"
        );
        assert!(
            files.iter().any(|file| file.rel == *rel),
            "{rel} is inventoried but the gated walk does not reach it"
        );
        for tail in *inventory {
            assert!(
                [
                    "Stripe: ",
                    "provider: ",
                    "crypto: ",
                    "invariant: ",
                    "classified: "
                ]
                .iter()
                .any(|reason| tail.why.starts_with(reason)),
                "{rel}: `{}` must say which non-database cause it is",
                tail.label
            );
        }
    }

    let mut gated = 0;
    for file in files.iter().filter(|file| gated_block(&file.rel).is_some()) {
        gated += 1;
        let rel = file.rel.as_str();
        let inventory = INVENTORIED_TAILS
            .iter()
            .find(|(listed, _)| *listed == rel)
            .map_or(&[][..], |(_, inventory)| *inventory);
        let (unlisted, stale) = tails_verdict(&file.text, inventory);
        assert!(
            unlisted.is_empty(),
            "{rel} has `err_internal` tails that are not on its inventory \
             (label, used, listed): {unlisted:?}\n\
             If the cause is a database call, use \
             `crud::db_error_internal(error, \"<label>\")` so a WRAP denial \
             stays a 403 and a quota a 429. If it genuinely is not — a Stripe \
             or OAuth provider call, the crypto service, or a fault of this \
             process — list it in INVENTORIED_TAILS with that reason."
        );
        assert!(
            stale.is_empty(),
            "{rel}'s inventory lists more `err_internal` tails than the file \
             has (label, used, listed): {stale:?}\n\
             Lower the count or delete the entry, so the inventory never \
             grants a tail the file does not use."
        );
    }
    // `products` alone is 40-odd files; a prefix that stopped matching would
    // otherwise leave this loop green over nothing.
    assert!(
        gated >= 30,
        "the gated walk reached only {gated} gated files"
    );
}

/// The inventory gate can fail, on the real file: a planted tail that wraps a
/// database call under a new label, a second copy of a listed label, and a
/// listed tail that is gone are each reported.
#[test]
fn the_inventory_gate_catches_a_planted_tail() {
    let (rel, inventory) = INVENTORIED_TAILS[0];
    let src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/blocks/products/stripe.rs"
    ))
    .expect("stripe.rs");
    assert_eq!(rel, "products/stripe.rs");
    assert_eq!(tails_verdict(&src, inventory), (Vec::new(), Vec::new()));

    let planted = format!(
        "{src}\nasync fn planted(ctx: &dyn Context) -> OutputStream {{\n    \
         match repo::refunds::get_by_idempotency_key(ctx, \"k\").await {{\n        \
         Ok(_) => ok_json(&()),\n        \
         Err(error) => err_internal(\n            \"Could not load the thing\",\n            error,\n        ),\n    \
         }}\n}}\n"
    );
    assert_eq!(
        tails_verdict(&planted, inventory).0,
        vec![("Could not load the thing".to_string(), 1, 0)]
    );

    let reused = format!(
        "{src}\nfn reused(error: WaferError) -> OutputStream {{ err_internal(\"Stripe API error\", error) }}\n"
    );
    assert_eq!(
        tails_verdict(&reused, inventory).0,
        vec![("Stripe API error".to_string(), 2, 1)]
    );

    let removed = src.replacen(
        "err_internal(\"Stripe API error\", error)",
        "crud::db_error_internal(error, \"Stripe API error\")",
        1,
    );
    assert_ne!(removed, src, "the listed Stripe tail must be in the file");
    assert_eq!(
        tails_verdict(&removed, inventory).1,
        vec![("Stripe API error".to_string(), 0, 1)]
    );

    // A turbofish on the callee is still the call, and so is a call inside a
    // macro invocation's tokens.
    assert_eq!(
        err_internal_labels(
            "fn a() { err_internal::<WaferError>(\"turbofish\", e); fail!(err_internal(\"in a macro\", e)); }\n"
        ),
        vec!["turbofish".to_string(), "in a macro".to_string()]
    );
    // A `#[cfg(test)]` early in a file drops that one item: the call after it
    // is still production code.
    assert_eq!(
        err_internal_labels(
            "#[cfg(test)]\nmod test_support;\n#[cfg(test)]\nuse a::b;\nfn a() { err_internal(\"after\", e); }\n"
        ),
        vec!["after".to_string()]
    );

    // Neither prose, a test, nor a different function is a tail.
    let not_tails = "// err_internal(\"prose\", e)\n\
         fn a() { err_internal_no_cause(\"x\"); crud::db_error_internal(e, \"y\"); }\n\
         #[cfg(test)]\nmod tests { fn t() { err_internal(\"in a test\", e); } }\n";
    assert!(err_internal_labels(not_tails).is_empty());
}

// ---------------------------------------------------------------------------
// Ways around the inventory
// ---------------------------------------------------------------------------

/// One way around the tail inventory; see [`evasions`].
#[derive(Debug, PartialEq, Eq)]
enum Evasion {
    /// `use …::err_internal as <name>`.
    Renamed(String),
    /// `err_internal` named somewhere other than the callee of a call.
    AsValue,
    /// A `macro_rules!` with this name whose body mentions `err_internal`.
    Macro(String),
    /// A function, trait default method, or closure bound by `let`, `const`
    /// or `static`, with this name, that passes its caller's error to
    /// `err_internal`.
    Wrapper(String),
}

/// The wrappers a gated file may keep, each with why nothing its callers can
/// pass is a database failure — `"invariant: …"`, or `"provider: …"` for a
/// wrapper whose parameter's type cannot carry a database error at all.
/// Checked both ways, like [`INVENTORIED_TAILS`]: an unlisted wrapper fails,
/// and so does a listed one that is gone.
///
/// A wrapper's own `err_internal` call is on its file's tail inventory once,
/// however many callers it has; this list is what says the callers were read.
const LISTED_WRAPPERS: &[(&str, &str, &str)] = &[
    (
        "products/purchase.rs",
        "child_rows",
        "invariant: every caller passes `…View::from_record` decodes of rows it already holds",
    ),
    (
        "llm/routes/providers.rs",
        "llm_error_response",
        "provider: its cause is an `LlmError` from the provider router, a type with no database code",
    ),
];

/// Every way `src` could send a database failure through `err_internal`
/// without [`err_internal_labels`] seeing the call site, one per finding.
/// Empty for a gated file, apart from its [`LISTED_WRAPPERS`].
///
/// The inventory counts `err_internal(` calls by label, so it is only as good
/// as the assumption that every tail is such a call, written where the error
/// arises. Four shapes break that, and each is a finding:
///
/// - **a renamed import** — `use crate::http::err_internal as fail;` — whose
///   calls are `fail(`, invisible to a scan for `err_internal(`;
/// - **the function as a value** — `let fail = err_internal;`, or
///   `.map_err(err_internal)`-style passing — for the same reason;
/// - **a `macro_rules!`** whose expansion calls it: the inventory counts the
///   one call in the macro body however many sites expand it;
/// - **a wrapper**: a function, a trait's default method, or a closure bound
///   by `let` (typed or not), `const` or `static`, whose `err_internal` cause
///   comes from its caller. The inventory counts the one call in the
///   wrapper, and every call site of the wrapper — each of which can pass a
///   database error — is invisible. A cause comes from the caller when it
///   names a parameter of the closure, or a parameter of the function whose
///   type is an error (`…Error`), a `Result`, a `Display`/`Debug` bound or a
///   generic, or a binding destructured from one (`match result { Err(e) =>
///   … }`, `result.map_err(|e| …)`, `let e = …`).
///
/// Parsed, not grepped: `use` trees nest and rustfmt wraps them, and a wrapper
/// is a question about scopes. `#[cfg(test)]` items are skipped. A wrapper that
/// launders the error through something else — a struct field, a call — is not
/// followed; the bet is that nobody writes one by accident, and one written on
/// purpose is a review finding.
fn evasions(src: &str) -> Vec<Evasion> {
    use syn::visit::Visit;

    const NAME: &str = "err_internal";

    fn is_cfg_test(attrs: &[syn::Attribute]) -> bool {
        attrs.iter().any(|attr| {
            attr.path().is_ident("cfg")
                && matches!(&attr.meta, syn::Meta::List(list) if list.tokens.to_string() == "test")
        })
    }

    fn names_err_internal(path: &syn::Path) -> bool {
        path.segments.last().is_some_and(|seg| seg.ident == NAME)
    }

    /// Single-segment paths an expression mentions.
    fn mentions(expr: &syn::Expr) -> std::collections::HashSet<String> {
        struct Idents(std::collections::HashSet<String>);
        impl<'ast> Visit<'ast> for Idents {
            fn visit_path(&mut self, path: &'ast syn::Path) {
                if let Some(ident) = path.get_ident() {
                    self.0.insert(ident.to_string());
                }
                syn::visit::visit_path(self, path);
            }
        }
        let mut idents = Idents(Default::default());
        idents.visit_expr(expr);
        idents.0
    }

    /// Identifiers a pattern binds.
    fn binds(pat: &syn::Pat) -> Vec<String> {
        struct Bound(Vec<String>);
        impl<'ast> Visit<'ast> for Bound {
            fn visit_pat_ident(&mut self, pat: &'ast syn::PatIdent) {
                self.0.push(pat.ident.to_string());
                syn::visit::visit_pat_ident(self, pat);
            }
        }
        let mut bound = Bound(Vec::new());
        bound.visit_pat(pat);
        bound.0
    }

    /// Whether a parameter's type can carry a caller's error.
    fn carries_an_error(ty: &syn::Type, generics: &[String]) -> bool {
        struct Names(Vec<String>);
        impl<'ast> Visit<'ast> for Names {
            fn visit_ident(&mut self, ident: &'ast proc_macro2::Ident) {
                self.0.push(ident.to_string());
            }
        }
        let mut names = Names(Vec::new());
        names.visit_type(ty);
        names.0.iter().any(|name| {
            name.ends_with("Error")
                || matches!(name.as_str(), "Result" | "Display" | "Debug")
                || generics.contains(name)
        })
    }

    /// A function's body, or a closure's.
    #[derive(Clone, Copy)]
    enum Body<'a> {
        Block(&'a syn::Block),
        Expr(&'a syn::Expr),
    }

    impl<'a> Body<'a> {
        fn walk(self, visitor: &mut impl Visit<'a>) {
            match self {
                Body::Block(block) => visitor.visit_block(block),
                Body::Expr(expr) => visitor.visit_expr(expr),
            }
        }
    }

    /// The identifiers in `body` that hold the caller's error, grown from
    /// `sources` through every binding destructured from one. Nested `fn`
    /// items are their own scope and are not entered.
    fn tainted(body: Body<'_>, sources: Vec<String>) -> std::collections::HashSet<String> {
        struct Taint {
            held: std::collections::HashSet<String>,
            grew: bool,
        }
        impl Taint {
            fn from(&self, expr: &syn::Expr) -> bool {
                mentions(expr).iter().any(|ident| self.held.contains(ident))
            }
            fn hold(&mut self, pat: &syn::Pat) {
                for ident in binds(pat) {
                    self.grew |= self.held.insert(ident);
                }
            }
        }
        impl<'ast> Visit<'ast> for Taint {
            fn visit_item_fn(&mut self, _: &'ast syn::ItemFn) {}
            fn visit_local(&mut self, local: &'ast syn::Local) {
                if local
                    .init
                    .as_ref()
                    .is_some_and(|init| self.from(&init.expr))
                {
                    self.hold(&local.pat);
                }
                syn::visit::visit_local(self, local);
            }
            fn visit_expr_match(&mut self, expr: &'ast syn::ExprMatch) {
                if self.from(&expr.expr) {
                    for arm in &expr.arms {
                        self.hold(&arm.pat);
                    }
                }
                syn::visit::visit_expr_match(self, expr);
            }
            fn visit_expr_let(&mut self, expr: &'ast syn::ExprLet) {
                if self.from(&expr.expr) {
                    self.hold(&expr.pat);
                }
                syn::visit::visit_expr_let(self, expr);
            }
            fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
                if self.from(&call.receiver) {
                    for arg in &call.args {
                        if let syn::Expr::Closure(closure) = arg {
                            for input in &closure.inputs {
                                self.hold(input);
                            }
                        }
                    }
                }
                syn::visit::visit_expr_method_call(self, call);
            }
        }
        let mut taint = Taint {
            held: sources.into_iter().collect(),
            grew: true,
        };
        while taint.grew {
            taint.grew = false;
            body.walk(&mut taint);
        }
        taint.held
    }

    /// The `err_internal` calls in `body` whose cause is in `held`.
    fn forwarded(body: Body<'_>, held: &std::collections::HashSet<String>) -> usize {
        struct Calls<'h> {
            held: &'h std::collections::HashSet<String>,
            found: usize,
        }
        impl<'ast> Visit<'ast> for Calls<'_> {
            fn visit_item_fn(&mut self, _: &'ast syn::ItemFn) {}
            fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
                if let syn::Expr::Path(callee) = &*call.func {
                    if names_err_internal(&callee.path)
                        && call.args.iter().nth(1).is_some_and(|cause| {
                            mentions(cause).iter().any(|i| self.held.contains(i))
                        })
                    {
                        self.found += 1;
                    }
                }
                syn::visit::visit_expr_call(self, call);
            }
        }
        let mut calls = Calls { held, found: 0 };
        body.walk(&mut calls);
        calls.found
    }

    struct Finder(Vec<Evasion>);

    impl Finder {
        fn function(&mut self, name: String, sig: &syn::Signature, body: &syn::Block) {
            let generics: Vec<String> = sig
                .generics
                .type_params()
                .map(|param| param.ident.to_string())
                .collect();
            let sources = sig
                .inputs
                .iter()
                .filter_map(|input| match input {
                    syn::FnArg::Typed(arg) if carries_an_error(&arg.ty, &generics) => {
                        Some(binds(&arg.pat))
                    }
                    _ => None,
                })
                .flatten()
                .collect();
            let body = Body::Block(body);
            if forwarded(body, &tainted(body, sources)) > 0 {
                self.0.push(Evasion::Wrapper(name));
            }
        }

        /// `expr`, bound to `name`, when it is a closure that forwards one of
        /// its own parameters to `err_internal`.
        fn closure(&mut self, name: String, expr: &syn::Expr) {
            if let syn::Expr::Closure(closure) = expr {
                let sources = closure.inputs.iter().flat_map(binds).collect();
                let body = Body::Expr(&closure.body);
                if forwarded(body, &tainted(body, sources)) > 0 {
                    self.0.push(Evasion::Wrapper(name));
                }
            }
        }
    }

    impl<'ast> Visit<'ast> for Finder {
        fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
            if !is_cfg_test(&item.attrs) {
                syn::visit::visit_item_mod(self, item);
            }
        }
        fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
            if !is_cfg_test(&item.attrs) {
                syn::visit::visit_item_impl(self, item);
            }
        }
        fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
            if !is_cfg_test(&item.attrs) {
                syn::visit::visit_item_use(self, item);
            }
        }
        fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
            if !is_cfg_test(&item.attrs) {
                self.function(item.sig.ident.to_string(), &item.sig, &item.block);
                syn::visit::visit_item_fn(self, item);
            }
        }
        fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
            if !is_cfg_test(&item.attrs) {
                self.function(item.sig.ident.to_string(), &item.sig, &item.block);
                syn::visit::visit_impl_item_fn(self, item);
            }
        }
        fn visit_trait_item_fn(&mut self, item: &'ast syn::TraitItemFn) {
            if is_cfg_test(&item.attrs) {
                return;
            }
            // A default method is a wrapper every implementor inherits.
            if let Some(body) = &item.default {
                self.function(item.sig.ident.to_string(), &item.sig, body);
            }
            syn::visit::visit_trait_item_fn(self, item);
        }
        fn visit_item_trait(&mut self, item: &'ast syn::ItemTrait) {
            if !is_cfg_test(&item.attrs) {
                syn::visit::visit_item_trait(self, item);
            }
        }
        fn visit_local(&mut self, local: &'ast syn::Local) {
            // `let fail = |e| …` and `let fail: fn(_) -> _ = |e| …` alike.
            let name = match &local.pat {
                syn::Pat::Type(typed) => &*typed.pat,
                pat => pat,
            };
            if let (Some(init), syn::Pat::Ident(name)) = (&local.init, name) {
                self.closure(name.ident.to_string(), &init.expr);
            }
            syn::visit::visit_local(self, local);
        }
        fn visit_item_const(&mut self, item: &'ast syn::ItemConst) {
            if !is_cfg_test(&item.attrs) {
                self.closure(item.ident.to_string(), &item.expr);
                syn::visit::visit_item_const(self, item);
            }
        }
        fn visit_item_static(&mut self, item: &'ast syn::ItemStatic) {
            if !is_cfg_test(&item.attrs) {
                self.closure(item.ident.to_string(), &item.expr);
                syn::visit::visit_item_static(self, item);
            }
        }
        fn visit_use_rename(&mut self, rename: &'ast syn::UseRename) {
            if rename.ident == NAME {
                self.0.push(Evasion::Renamed(rename.rename.to_string()));
            }
        }
        fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
            match &*call.func {
                // The callee is the one place the name may appear.
                syn::Expr::Path(callee) if names_err_internal(&callee.path) => {
                    for arg in &call.args {
                        self.visit_expr(arg);
                    }
                }
                _ => syn::visit::visit_expr_call(self, call),
            }
        }
        fn visit_expr_path(&mut self, expr: &'ast syn::ExprPath) {
            if names_err_internal(&expr.path) {
                self.0.push(Evasion::AsValue);
            }
            syn::visit::visit_expr_path(self, expr);
        }
        fn visit_item_macro(&mut self, item: &'ast syn::ItemMacro) {
            if !is_cfg_test(&item.attrs)
                && item.mac.path.is_ident("macro_rules")
                && item.mac.tokens.to_string().contains(NAME)
            {
                self.0.push(Evasion::Macro(
                    item.ident
                        .as_ref()
                        .map_or_else(String::new, ToString::to_string),
                ));
            }
        }
    }

    let file = syn::parse_file(src).unwrap_or_else(|error| panic!("unparseable source: {error}"));
    let mut finder = Finder(Vec::new());
    finder.visit_file(&file);
    finder.0
}

#[test]
fn gated_files_call_err_internal_only_by_name() {
    let mut found = Vec::new();
    let mut listed_and_found = Vec::new();
    for file in gated_scan().collect() {
        if gated_block(&file.rel).is_none() {
            continue;
        }
        for evasion in evasions(&file.text) {
            let listed = LISTED_WRAPPERS.iter().find(|(rel, name, _)| {
                *rel == file.rel && evasion == Evasion::Wrapper(name.to_string())
            });
            match listed {
                Some(entry) => listed_and_found.push(entry),
                None => found.push((file.rel.clone(), evasion)),
            }
        }
    }
    assert!(
        found.is_empty(),
        "these gated files reach `err_internal` in a way the tail inventory \
         cannot count: {found:#?}\n\
         Call `err_internal(label, cause)` by name where the error arises, or \
         route a database error through `crud::db_error_internal`. A wrapper \
         whose callers can only pass a fault of this process may be added to \
         LISTED_WRAPPERS with that reason."
    );
    for entry in LISTED_WRAPPERS {
        assert!(
            entry.2.starts_with("invariant: ") || entry.2.starts_with("provider: "),
            "{entry:?} must say why it is an invariant, or which provider type it takes"
        );
        assert!(
            listed_and_found.contains(&entry),
            "{entry:?} is listed but is no longer a wrapper in that file; \
             delete the entry"
        );
    }
}

/// Each evasion is caught, and none of the shapes the gated files really use
/// is mistaken for one.
#[test]
fn the_evasion_gate_catches_each_way_around() {
    let caught = [
        "use crate::http::err_internal as x;\nfn f() {}\n",
        "use crate::http::{err_bad_request, err_internal as fail};\n",
        "fn f() { let fail = crate::http::err_internal; }\n",
        "fn f(r: Result<u8, E>) { r.map_err(err_internal); }\n",
        "macro_rules! fail { ($e:expr) => { err_internal(\"x\", $e) }; }\n",
        "fn fail(error: WaferError) -> OutputStream { err_internal(\"x\", error) }\n",
        "fn fail(e: impl std::fmt::Display) -> OutputStream { err_internal(\"x\", e) }\n",
        "fn fail<E: Display>(e: E) -> OutputStream { err_internal(\"x\", e) }\n",
        "fn respond(result: Result<u8, WaferError>) -> OutputStream {\n\
             match result { Ok(_) => ok(), Err(e) => err_internal(\"x\", e) }\n}\n",
        "fn respond(result: Result<u8, WaferError>) -> Result<u8, OutputStream> {\n\
             result.map_err(|e| err_internal(\"x\", e))\n}\n",
        "impl S { fn fail(&self, error: WaferError) -> OutputStream { err_internal(\"x\", error) } }\n",
        "fn f() { let fail = |e| err_internal(\"x\", e); }\n",
        // A type annotation on the binding does not hide the closure.
        "fn f() { let fail: fn(WaferError) -> OutputStream = |e| err_internal(\"x\", e); }\n",
        "const FAIL: fn(WaferError) -> OutputStream = |e| err_internal(\"x\", e);\n",
        "static FAIL: fn(WaferError) -> OutputStream = |e| err_internal(\"x\", e);\n",
        // Nor does a trait: every implementor inherits the default body.
        "trait Fail { fn fail(&self, error: WaferError) -> OutputStream { err_internal(\"x\", error) } }\n",
        // Nor a turbofish on the callee.
        "fn fail(error: WaferError) -> OutputStream { err_internal::<WaferError>(\"x\", error) }\n",
    ];
    for src in caught {
        assert_eq!(evasions(src).len(), 1, "not caught: {src}");
    }
    assert_eq!(
        evasions("use crate::http::err_internal as x;\n"),
        vec![Evasion::Renamed("x".to_string())]
    );

    let allowed = [
        // The shapes the products files use: a call where the error arises,
        // an inline `map_err` closure, a helper that decodes its argument.
        "use crate::http::{err_internal, ok_json};\n\
         fn a(ctx: &dyn Context) -> OutputStream {\n\
             match load(ctx) { Ok(v) => ok_json(&v), Err(e) => err_internal(\"x\", e) }\n}\n",
        "async fn b(ctx: &dyn Context, outcome: &str) -> Result<u16, OutputStream> {\n\
             fee(ctx).await.map_err(|error| err_internal(&format!(\"{outcome}\"), error))\n}\n",
        "fn product_json(record: &db::Record) -> OutputStream {\n\
             match View::from_record(record) { Ok(v) => ok_json(&v), Err(e) => crate::http::err_internal(\"x\", e) }\n}\n",
        // A wrapper in a test is not production code.
        "#[cfg(test)]\nmod tests { fn fail(error: WaferError) -> OutputStream { err_internal(\"x\", error) } }\n",
        // A helper forwarding to the door is the fix, not an evasion.
        "fn fail(error: WaferError) -> OutputStream { crud::db_error_internal(error, \"x\") }\n",
    ];
    for src in allowed {
        assert_eq!(evasions(src), Vec::new(), "wrongly caught: {src}");
    }
}

// ---------------------------------------------------------------------------
// Where the inventory does not run yet
// ---------------------------------------------------------------------------

/// Every block outside [`GATED_BLOCKS`] that still calls
/// `err_internal(label, cause)`: how many of its files do, and the review
/// plan item that gates it. Nothing there is read per site: any of those
/// calls may be a database failure answering 500 where a WRAP denial should
/// be a 403 and a quota a 429.
///
/// **Empty**: `dev`, `files` and `tickets` were the last, and are gated. A
/// block that starts calling `err_internal` joins `GATED_BLOCKS` with its
/// tails inventoried, or is listed here with the plan item that will. The
/// counts are exact both ways, so the backlog this states stays true. A
/// top-level file under `src/blocks/` is its own entry; `crud.rs` is the door
/// and is not listed. `"unplanned"` is a block no plan item has scheduled yet.
const NOT_YET_GATED: &[(&str, usize, &str)] = &[];

/// The block a `src/blocks`-relative path belongs to: its first component.
fn block_of(rel: &str) -> &str {
    rel.split('/').next().unwrap_or(rel)
}

#[test]
fn not_yet_gated_is_the_whole_backlog() {
    let mut files_with_tails: std::collections::BTreeMap<String, usize> = Default::default();
    for file in gated_scan().collect() {
        if file.rel == THE_DOOR || gated_block(&file.rel).is_some() {
            continue;
        }
        if !err_internal_labels(&file.text).is_empty() {
            *files_with_tails
                .entry(block_of(&file.rel).to_string())
                .or_default() += 1;
        }
    }
    let listed: std::collections::BTreeMap<String, usize> = NOT_YET_GATED
        .iter()
        .map(|(block, files, _)| (block.to_string(), *files))
        .collect();
    assert_eq!(
        files_with_tails, listed,
        "NOT_YET_GATED must state exactly the ungated blocks that still call \
         `err_internal(label, cause)`, and in how many files (left: the tree, \
         right: the list)"
    );
}

// ---------------------------------------------------------------------------
// Where the cause is dropped before anything could classify it
// ---------------------------------------------------------------------------

/// The helpers that answer a 500 without taking a cause:
/// `err_internal_no_cause(label)` and `ui::server_error_response(msg)`. A
/// database failure answered through either has lost its code before the
/// call, so no inventory of `err_internal` tails can see it — in a gated block
/// or anywhere else. A full page whose read failed answers through
/// `crud::db_error_page` instead, which classifies first.
const CAUSE_DROPPING: &[&str] = &["err_internal_no_cause", "server_error_response"];

/// Every block's [`CAUSE_DROPPING`] call sites, with the plan item that reads
/// them or what reading them found. The counts are exact both ways, like
/// [`NOT_YET_GATED`]: a new site raises its block's count here, and the edit
/// is where its reviewer asks whether the cause was a database call.
///
/// `"read: …"` is a block whose every site was read and none drops a database
/// error; `"unplanned"` is a block no plan item has scheduled yet.
const CAUSE_DROPPED: &[(&str, usize, &str)] = &[
    (
        "auth_ui",
        7,
        "read: OAuth provider responses, OAuth configuration, and the \
         admin row bootstrap has just inserted — none carries a cause",
    ),
    (
        "files",
        3,
        "read: a share row with no expiry, an unparseable one, or no bucket or \
         key — the lookup succeeded, so there is no cause to carry",
    ),
    (
        "products",
        32,
        "read: a Stripe payload that disagrees with the ledger or is missing a \
         field, a concurrent or already-scheduled webhook delivery, a checkout \
         claim or subscription lookup that succeeded and matched nothing, a \
         resolved checkout component its offer no longer has, and a setting \
         outside its range (read successfully; a failed read goes through the door) — \
         none carries a cause",
    ),
    (
        "userportal",
        1,
        "read: the profile page's signed-in user with no users row — the read \
         succeeded, so there is no cause to carry",
    ),
    (
        "vector",
        1,
        "read: the embedding block answered success with no vectors — the call \
         did not fail, so there is no cause to carry",
    ),
];

#[test]
fn cause_dropped_is_the_whole_backlog() {
    let mut sites: std::collections::BTreeMap<String, usize> = Default::default();
    for file in gated_scan().collect() {
        if file.rel == THE_DOOR {
            continue;
        }
        let n: usize = CAUSE_DROPPING
            .iter()
            .map(|name| call_arguments(&file.text, name).len())
            .sum();
        if n > 0 {
            *sites.entry(block_of(&file.rel).to_string()).or_default() += n;
        }
    }
    let listed: std::collections::BTreeMap<String, usize> = CAUSE_DROPPED
        .iter()
        .map(|(block, n, _)| (block.to_string(), *n))
        .collect();
    assert_eq!(
        sites, listed,
        "CAUSE_DROPPED must state exactly how many `err_internal_no_cause` / \
         `ui::server_error_response` calls each block makes (left: the tree, \
         right: the list). A database failure belongs in \
         `crud::db_error_internal`, or `crud::db_error_page` for a full page."
    );
    for (block, _, plan) in CAUSE_DROPPED.iter().chain(NOT_YET_GATED) {
        assert!(
            plan.starts_with('N') || plan.starts_with("read: ") || *plan == "unplanned",
            "{block}: `{plan}` must name a plan item, say what reading it found, \
             or say `unplanned`"
        );
    }
}

// ---------------------------------------------------------------------------
// Where a failure's own text is printed into a page
// ---------------------------------------------------------------------------

/// How many maud interpolations in `src`'s production code print a failed
/// call's own text.
///
/// That shape answers a failed read as a page: the status is 200, the empty
/// table's place is taken by the failure, and a WRAP denial's own text (the
/// grant and table names it refused) is shown to whoever loaded the page.
/// Nothing classified it on the way, so no inventory of `err_internal` tails
/// can see it either. A page answers a failed read through
/// `crud::db_error_page`, and a fragment through `crud::db_error_notice`.
///
/// Read from the token stream ([`production_tokens`]), so a group ends where
/// its brace does, however deeply the markup inside it nests. An
/// interpolation is a parenthesized group inside an `html!` invocation that
/// is not a call's argument list (it does not follow an identifier or a
/// macro's `!`). It prints the failure when its expression starts from a
/// name that holds an error (`(e)`, `(e.message)`, `(e.to_string())`,
/// `(&e.message.clone())`), is a `format!` naming one as an argument or
/// inline (`format!("…{e}")`), or is either of those wrapped in maud's
/// `PreEscaped(…)`. A name holds an error when it is bound:
///
/// - by an `Err(…)` anywhere in a pattern — every name it binds, so
///   `Err(WaferError { message, .. })` binds `message`, and `Some(Err(e))`
///   and `(Err(e), _) | (_, Err(e))` bind `e` — in a `match` arm
///   (`Err(e) => { … }`, `Err(e) if guard => …`, `Err(e) => html! { … }`,
///   in Rust or in maud's `@match`), for the arm's body; in an `if let` or
///   `while let` (maud's `@if let` too), for its block; or in a
///   `let Err(e) = r else { … };`, for the rest of the enclosing block; or
/// - as a parameter of a `fn` whose type names an `…Error`
///   (`fn notice(e: &WaferError)`), for the function's body — an error
///   handed to a helper, which no pattern scan sees.
///
/// Only names bound that way count: `(notice.message)` is a struct's field,
/// not an error's text, unless `notice` is one of them.
///
/// What it does NOT follow: the text laundered through something else first
/// (`@let text = e.to_string();` and then `(text)`, or a helper called with
/// `e` that returns its message), a closure's parameter, or a generic `E`
/// parameter. A classified reason — `(crud::db_error_notice(e, "…"))` —
/// starts from the door, not from `e`, and is not the shape.
fn error_text_renders(src: &str) -> usize {
    use proc_macro2::{Delimiter, TokenStream, TokenTree};

    fn is_punct(tree: Option<&TokenTree>, ch: char) -> bool {
        matches!(tree, Some(TokenTree::Punct(p)) if p.as_char() == ch)
    }

    fn is_ident(tree: Option<&TokenTree>, name: &str) -> bool {
        matches!(tree, Some(TokenTree::Ident(ident)) if ident == name)
    }

    fn is_group(tree: Option<&TokenTree>, delimiter: Delimiter) -> bool {
        matches!(tree, Some(TokenTree::Group(group)) if group.delimiter() == delimiter)
    }

    /// Every name `pattern` binds: `e` in `e`, `ref e` or `mut e`, `message`
    /// in `WaferError { message, .. }`. A capitalized identifier is a unit
    /// variant or a constant, not a binding; `_` binds nothing.
    fn bound_names(pattern: TokenStream) -> Vec<String> {
        use syn::{parse::Parser, visit::Visit};

        struct Bound(Vec<String>);
        impl<'ast> Visit<'ast> for Bound {
            fn visit_pat_ident(&mut self, pat: &'ast syn::PatIdent) {
                let name = pat.ident.to_string();
                if name.starts_with(|c: char| c.is_lowercase() || c == '_') {
                    self.0.push(name);
                }
                syn::visit::visit_pat_ident(self, pat);
            }
        }
        let Ok(pat) = syn::Pat::parse_single.parse2(pattern) else {
            return Vec::new();
        };
        let mut bound = Bound(Vec::new());
        bound.visit_pat(&pat);
        bound.0
    }

    /// The parameters of a `fn` signature whose type names an `…Error`.
    fn error_parameters(params: TokenStream) -> Vec<String> {
        use syn::{parse::Parser, punctuated::Punctuated, visit::Visit, FnArg, Token};

        struct Names(bool);
        impl<'ast> Visit<'ast> for Names {
            fn visit_ident(&mut self, ident: &'ast proc_macro2::Ident) {
                self.0 |= ident.to_string().ends_with("Error");
            }
        }
        let Ok(args) = Punctuated::<FnArg, Token![,]>::parse_terminated.parse2(params) else {
            return Vec::new();
        };
        args.iter()
            .filter_map(|arg| match arg {
                FnArg::Typed(typed) => {
                    let mut names = Names(false);
                    names.visit_type(&typed.ty);
                    names.0.then(|| bound_names(quote_pat(&typed.pat)))
                }
                FnArg::Receiver(_) => None,
            })
            .flatten()
            .collect()
    }

    /// A parameter pattern's own tokens, for [`bound_names`]. Only a plain
    /// or `mut` identifier is spelled back; a destructuring parameter binds
    /// nothing this scan follows.
    fn quote_pat(pat: &syn::Pat) -> TokenStream {
        match pat {
            syn::Pat::Ident(ident) => TokenTree::Ident(ident.ident.clone()).into(),
            _ => TokenStream::new(),
        }
    }

    /// Every name an `Err(…)` inside the pattern `trees` binds, at any
    /// depth: `e` in `Err(e)`, `Some(Err(e))` and `(Err(e), _) | (_, Err(e))`.
    fn err_names(trees: &[TokenTree]) -> Vec<String> {
        let mut names = Vec::new();
        for (i, tree) in trees.iter().enumerate() {
            match tree {
                TokenTree::Ident(ident) if ident == "Err" => {
                    if let Some(TokenTree::Group(pattern)) = trees.get(i + 1) {
                        if pattern.delimiter() == Delimiter::Parenthesis {
                            names.extend(bound_names(pattern.stream()));
                        }
                    }
                }
                // `Err`'s own group was read above; this reaches the nested
                // ones (`Some(Err(e))`), and re-reading `Err(e)`'s group
                // finds no further `Err` in it.
                TokenTree::Group(group) => {
                    let inner: Vec<TokenTree> = group.stream().into_iter().collect();
                    names.extend(err_names(&inner));
                }
                _ => {}
            }
        }
        names.sort();
        names.dedup();
        names
    }

    /// For each sibling index, the names bound as holding an error there: an
    /// `Err(…)` arm's body, an `if let`'s block, the rest of the block after
    /// a `let … else`, a `fn`'s body for its error-typed parameters.
    fn bindings(trees: &[TokenTree]) -> Vec<Vec<String>> {
        let mut held = vec![Vec::new(); trees.len()];
        for at in 0..trees.len() {
            if is_ident(trees.get(at), "fn") {
                let Some(params) =
                    (at + 1..trees.len()).find(|&i| is_group(trees.get(i), Delimiter::Parenthesis))
                else {
                    continue;
                };
                // The body, unless the signature ends at a `;` first.
                let body = (params + 1..trees.len()).find(|&i| {
                    is_group(trees.get(i), Delimiter::Brace) || is_punct(trees.get(i), ';')
                });
                if let (Some(TokenTree::Group(params)), Some(body)) = (trees.get(params), body) {
                    if is_group(trees.get(body), Delimiter::Brace) {
                        held[body].extend(error_parameters(params.stream()));
                    }
                }
                continue;
            }
            // An arm: `=>`, with its pattern reaching back to the previous
            // arm's comma or brace body. An `Err(…)` anywhere in it binds —
            // `Some(Err(e))`, `(Err(e), _) | (_, Err(e))` — and a guard
            // (`if …`) is not part of it.
            if is_punct(trees.get(at), '=') && is_punct(trees.get(at + 1), '>') {
                let from = (0..at)
                    .rev()
                    .find(|&i| {
                        is_punct(trees.get(i), ',') || is_group(trees.get(i), Delimiter::Brace)
                    })
                    .map_or(0, |i| i + 1);
                let to = (from..at)
                    .find(|&i| is_ident(trees.get(i), "if"))
                    .unwrap_or(at);
                let names = err_names(&trees[from..to]);
                if names.is_empty() {
                    continue;
                }
                // Its brace body, or everything up to its comma.
                let start = at + 2;
                let end = if is_group(trees.get(start), Delimiter::Brace) {
                    start + 1
                } else {
                    (start..trees.len())
                        .find(|&i| is_punct(trees.get(i), ','))
                        .unwrap_or(trees.len())
                };
                for slot in &mut held[start..end] {
                    slot.extend(names.iter().cloned());
                }
                continue;
            }
            // A `let`: its pattern runs to the `=`.
            if !is_ident(trees.get(at), "let") {
                continue;
            }
            let Some(eq) = (at + 1..trees.len()).find(|&i| is_punct(trees.get(i), '=')) else {
                continue;
            };
            let names = err_names(&trees[at + 1..eq]);
            if names.is_empty() {
                continue;
            }
            let conditional = at >= 1
                && (is_ident(trees.get(at - 1), "if") || is_ident(trees.get(at - 1), "while"));
            if conditional {
                // `if let Err(e) = … { body }`: the first block after `=`.
                if let Some(body) =
                    (eq + 1..trees.len()).find(|&i| is_group(trees.get(i), Delimiter::Brace))
                {
                    held[body].extend(names.iter().cloned());
                }
            } else {
                // `let Err(e) = r else { … };`: bound after the statement.
                let end_of_statement = (eq..trees.len())
                    .find(|&i| is_punct(trees.get(i), ';'))
                    .unwrap_or(trees.len());
                for slot in &mut held[end_of_statement..] {
                    slot.extend(names.iter().cloned());
                }
            }
        }
        held
    }

    /// Whether `tokens` mention `name` as an identifier, or as an inline
    /// format argument (`{name}`, `{name:?}`) in a string literal, at any
    /// depth.
    fn mentions(tokens: TokenStream, name: &str) -> bool {
        tokens.into_iter().any(|tree| match tree {
            TokenTree::Ident(ident) => ident == name,
            TokenTree::Literal(literal) => {
                let text = literal.to_string();
                text.contains(&format!("{{{name}}}")) || text.contains(&format!("{{{name}:"))
            }
            TokenTree::Group(group) => mentions(group.stream(), name),
            TokenTree::Punct(_) => false,
        })
    }

    /// Whether the interpolated expression `tokens` prints an error: one of
    /// `bound` as its root or as a `format!` argument, directly or inside
    /// `PreEscaped(…)`.
    fn prints_an_error(tokens: TokenStream, bound: &[String]) -> bool {
        let trees: Vec<TokenTree> = tokens.into_iter().collect();
        let start = trees
            .iter()
            .position(
                |tree| !matches!(tree, TokenTree::Punct(p) if matches!(p.as_char(), '&' | '*')),
            )
            .unwrap_or(trees.len());
        let expr = &trees[start..];
        // `PreEscaped(…)` or `maud::PreEscaped(…)`: the wrapper marks the
        // text as markup, which does not stop it being the error's.
        if let [path @ .., TokenTree::Ident(wrapper), TokenTree::Group(inner)] = expr {
            if wrapper == "PreEscaped"
                && inner.delimiter() == Delimiter::Parenthesis
                && path.iter().all(|tree| {
                    matches!(tree, TokenTree::Ident(_))
                        || matches!(tree, TokenTree::Punct(p) if p.as_char() == ':')
                })
            {
                return prints_an_error(inner.stream(), bound);
            }
        }
        match expr {
            [TokenTree::Ident(root), ..] if bound.iter().any(|name| root == name) => true,
            [TokenTree::Ident(mac), TokenTree::Punct(bang), TokenTree::Group(args), ..]
                if (mac == "format" || mac == "format_args") && bang.as_char() == '!' =>
            {
                bound.iter().any(|name| mentions(args.stream(), name))
            }
            _ => false,
        }
    }

    fn walk(trees: &[TokenTree], in_html: bool, bound: &[String], found: &mut usize) {
        let held = bindings(trees);
        for (at, tree) in trees.iter().enumerate() {
            let TokenTree::Group(group) = tree else {
                continue;
            };
            let mut scope = bound.to_vec();
            scope.extend(held[at].iter().cloned());
            let previous = at.checked_sub(1).and_then(|i| trees.get(i));
            let is_call = matches!(previous, Some(TokenTree::Ident(_))) || is_punct(previous, '!');
            if in_html
                && group.delimiter() == Delimiter::Parenthesis
                && !is_call
                && prints_an_error(group.stream(), &scope)
            {
                *found += 1;
            }
            let opens_html =
                is_punct(previous, '!') && at >= 2 && is_ident(trees.get(at - 2), "html");
            let inner: Vec<TokenTree> = group.stream().into_iter().collect();
            walk(&inner, in_html || opens_html, &scope, found);
        }
    }

    let trees: Vec<TokenTree> = production_tokens(src).into_iter().collect();
    let mut found = 0;
    walk(&trees, false, &[], &mut found);
    found
}

/// Every block that still prints a failure's own text into a page, how many
/// times, and the plan item that reads it — exact both ways, like
/// [`CAUSE_DROPPED`]. **Empty**: products' six list pages were the last, and
/// they answer `crud::db_error_page` now.
const ERROR_TEXT_RENDERED: &[(&str, usize, &str)] = &[];

#[test]
fn error_text_rendered_is_the_whole_backlog() {
    let mut sites: std::collections::BTreeMap<String, usize> = Default::default();
    for file in gated_scan().collect() {
        if file.rel == THE_DOOR {
            continue;
        }
        let n = error_text_renders(&file.text);
        if n > 0 {
            *sites.entry(block_of(&file.rel).to_string()).or_default() += n;
        }
    }
    let listed: std::collections::BTreeMap<String, usize> = ERROR_TEXT_RENDERED
        .iter()
        .map(|(block, n, _)| (block.to_string(), *n))
        .collect();
    assert_eq!(
        sites, listed,
        "ERROR_TEXT_RENDERED must state exactly how many times each block prints \
         a failure's own text into markup (left: the tree, right: the list). A \
         page answers a failed read through `crud::db_error_page`, a fragment \
         through `crud::db_error_notice`."
    );
    for (block, _, plan) in ERROR_TEXT_RENDERED {
        assert!(
            plan.starts_with('N') || *plan == "unplanned",
            "{block}: `{plan}` must name a plan item or say `unplanned`"
        );
    }
}

/// The scan can fail: each shape it exists for is found, and the shapes that
/// merely look like it are not.
#[test]
fn the_error_text_scan_catches_the_shapes() {
    for (src, n) in [
        // The admin users page's roles tab, as it was.
        (
            "fn a() { html! { @match r { Ok(l) => { (l) } Err(e) => {\n div .login-error { \"Failed to load roles: \" (e.message) }\n } } } }\n",
            1,
        ),
        // Display of the error itself.
        (
            "fn a() { html! { @match r { Err(e) => { div .login-error { \"Failed: \" (e) } } } } }\n",
            1,
        ),
        // A helper handed the error.
        (
            "fn a(e: &WaferError) -> Markup { html! { div { \"Failed to load variables: \" (e.message) } } }\n",
            1,
        ),
        // Two sites.
        (
            "fn a() { html! { Err(a) => { p { \"x\" (a) } } Err(b) => { p { \"y\" (b.message) } } } }\n",
            2,
        ),
        // The error's text by any spelling.
        (
            "fn a() { html! { @match r { Err(e) => { p { \"x\" (e.to_string()) } } } } }\n",
            1,
        ),
        (
            "fn a() { html! { @match r { Err(e) => { p { (format!(\"Failed: {e}\")) } } } } }\n",
            1,
        ),
        (
            "fn a() { html! { @match r { Err(e) => { p { (format!(\"Failed: {:?}\", e)) } } } } }\n",
            1,
        ),
        (
            "fn a() { html! { @match r { Err(e) => { p { (e.message.clone()) } } } } }\n",
            1,
        ),
        (
            "fn a() { html! { @match r { Err(e) => { p { (&e.message) } } } } }\n",
            1,
        ),
        // Bound by `@if let`, by `ref`, or in a Rust arm whose body is an
        // `html!` with no braces around it.
        (
            "fn a() { html! { @if let Err(e) = &r { p { \"x\" (e) } } } }\n",
            1,
        ),
        (
            "fn a() { html! { @match &r { Err(ref e) => { p { (e) } } } } }\n",
            1,
        ),
        (
            "fn a() -> Markup { match r { Ok(v) => html! { (v) }, Err(e) => html! { p { (e) } }, } }\n",
            1,
        ),
        (
            "fn a() -> Markup { if let Err(e) = r { return html! { p { (e) } }; } html! {} }\n",
            1,
        ),
        // Nested markup: the arm does not end at the first closing brace.
        (
            "fn a() { html! { @match r { Err(e) => { div { span { \"a\" } } div { p { (e) } } } } } }\n",
            1,
        ),
        // An arm with a guard, in maud and in Rust.
        (
            "fn a() { html! { @match r { Err(e) if e.code == ErrorCode::Internal => { p { (e) } } } } }\n",
            1,
        ),
        (
            "fn a() -> Markup { match r { Err(e) if e.code >= 3 => html! { p { (e.message) } }, _ => html! {} } }\n",
            1,
        ),
        // A destructured error: every name the pattern binds.
        (
            "fn a() { html! { @match r { Err(WaferError { message, .. }) => { p { (message) } } } } }\n",
            1,
        ),
        (
            "fn a() { html! { @match r { Err(WaferError { message: text, .. }) => { p { (text) } } } } }\n",
            1,
        ),
        // `let … else`: the name is bound for the rest of the block.
        (
            "fn a() -> Markup { let Err(e) = r else { return html! {} }; html! { p { (e) } } }\n",
            1,
        ),
        // An `Err` nested inside the arm's pattern, or either side of an
        // or-pattern.
        (
            "fn a() -> Markup { match (x, y) { (Ok(a), Ok(b)) => html! { (a) (b) }, (Err(e), _) | (_, Err(e)) => html! { p { (e) } }, } }\n",
            1,
        ),
        (
            "fn a() { html! { @match r { Some(Err(e)) => { p { (e.message) } } _ => {} } } }\n",
            1,
        ),
        (
            "fn a() -> Markup { if let Some(Err(e)) = r { return html! { (e) }; } html! {} }\n",
            1,
        ),
        // Marked as markup, which is worse, not better.
        (
            "fn a() { html! { @match r { Err(e) => { p { (PreEscaped(format!(\"<b>{e}</b>\"))) } } } } }\n",
            1,
        ),
        (
            "fn a() { html! { @match r { Err(e) => { p { (maud::PreEscaped(e.to_string())) } } } } }\n",
            1,
        ),
        // Not the shape: calls, a classified reason, an unbound name, the Ok
        // arm, markup outside the arm, a test, a struct's `.message` field, a
        // guard that reads the error while the body prints none of it, and
        // the `else` of a `let … else`, where the name is not bound.
        (
            "fn a(notice: &Notice) -> Markup { html! { p .notice { (notice.message) } } }\n",
            0,
        ),
        (
            "fn a() { html! { @for n in notices { p { (n.message) } } } }\n",
            0,
        ),
        (
            "fn a() { html! { @match r { Err(e) if e.code == ErrorCode::NotFound => { p { \"missing\" } } } } }\n",
            0,
        ),
        (
            "fn a() { html! { @match r { Err(e) => { p { (PreEscaped(crud::db_error_notice(e, \"x\"))) } } } } }\n",
            0,
        ),
        (
            "fn a() -> Markup { let Err(e) = r else { return html! { (e) } }; html! {} }\n",
            0,
        ),
        ("fn a() { match r { Err(e) => { return Err(e) } } }\n", 0),
        ("fn a() { match r { Err(e) => { return crud::db_error_internal(e, \"x\") } } }\n", 0),
        ("fn a() { match r { Err(e) => { RoomError::Db(e.message) } } }\n", 0),
        ("fn a() { html! { div { \"Failed: \" (reason) } } }\n", 0),
        (
            "fn a() { html! { @match r { Err(e) => { p { \"Failed: \" (crud::db_error_notice(e, \"x\")) } } } } }\n",
            0,
        ),
        (
            "fn a() { html! { @match r { Ok(e) => { (e) } Err(_) => { p { \"failed\" } } } } }\n",
            0,
        ),
        (
            "fn a() { match r { Err(e) => log(e), _ => {} } html! { (e) } }\n",
            0,
        ),
        (
            "#[cfg(test)]\nmod tests { fn t() { html! { Err(e) => { p { \"x\" (e) } } } } }\n",
            0,
        ),
    ] {
        assert_eq!(error_text_renders(src), n, "{src}");
    }
}
