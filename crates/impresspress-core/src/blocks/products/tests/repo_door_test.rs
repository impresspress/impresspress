//! Which `repo::products` function a call site picks.
//!
//! *That* the products table is named only inside `repo::products` is now
//! asserted by the crate-level `tests/repo_door.rs`, which owns one
//! allowlist per table for every door in the crate — the three scans that
//! used to live here moved there when the products tables joined it, because
//! two allowlists for one table is precisely the drift a door exists to
//! prevent. What stays here is what that gate cannot express: `repo::products`
//! deliberately exposes functions that read and write *past* the soft-delete
//! filter, and picking the wrong one is a bug no table-name scan can see. A
//! read that goes around the `deleted_at` filter makes a soft-deleted product
//! visible again — which for the catalog means purchasable.
//!
//! Scope: every `.rs` file in this crate, not just `src/blocks/products`.
//! `repo` is `pub(crate)`, so a call site can be anywhere in it.
//!
//! What this gate still does NOT cover, stated so it is not mistaken for
//! more than it is:
//!
//!   * other workspace crates (`impresspress-web`, `-browser`,
//!     `-cloudflare`, the CLI). `CARGO_MANIFEST_DIR` is this crate, and the
//!     products block lives entirely here; a cross-crate read would need
//!     `repo` to be public, which it is not.
//!   * non-Rust sources. The migrations under `blocks/products/migrations/`
//!     name the table by design — that is where it is defined.
//!   * the files on the allowlists below. Each is listed individually, so a
//!     NEW file reading the table raw fails the gate and has to justify
//!     itself here; but nothing re-checks that a listed file's reads are
//!     still fixture setup rather than production reads.

use crate::test_support::source_scan::SourceWalk;

/// The walk this gate runs over: every `.rs` file in the crate, with a floor
/// so an empty scan cannot pass as a clean one.
fn scan() -> SourceWalk {
    SourceWalk::crate_src().least(100)
}

/// Whether `path` (relative to `src`) matches one of `allowlist`'s entries.
/// Every entry must match `path` exactly — no directory prefixes, so an
/// allowlist can never exempt a file that does not exist yet. A suffix match
/// would additionally let `"mod.rs"` wrongly allow `repo/mod.rs`,
/// `handlers/mod.rs`, etc.
fn matches_allowlist(path: &str, allowlist: &[&str]) -> bool {
    allowlist.contains(&path)
}

/// The files, outside `allowlist`, that name `ident` — the shape both scans
/// below run, stated once so the walk's own self-test drives the same filter
/// over a planted tree.
fn callers(walk: &SourceWalk, ident: &str, allowlist: &[&str]) -> Vec<String> {
    walk.collect()
        .into_iter()
        .filter(|file| !matches_allowlist(&file.rel, allowlist))
        .filter(|file| file.text.contains(ident))
        .map(|file| file.rel)
        .collect()
}

/// The crate-level door test proves that nothing *names the products table*
/// outside `repo::products`. It says nothing about which of that module's
/// own functions a call site picks — and on the write side the choice is the
/// whole correctness question.
///
/// `update_live`, `soft_delete` and `restore` each carry their soft-delete
/// predicate in the write's own `WHERE`, so they cannot land on a row whose
/// state changed since the caller last looked. Two functions deliberately do
/// not: `update_including_deleted` writes with no liveness filter at all, and
/// `purge` hard-deletes the row. Both are `pub(crate)`, both compile cleanly
/// from anywhere in the block, and nothing in the type system distinguishes
/// the handful of deliberate uses from an accidental one — a new handler
/// reaching for `update_including_deleted` writes straight through a
/// soft-deleted product and every other gate in this file still passes.
///
/// So they are distinguished by name, and each call site is listed here with
/// its justification. This is the write-side twin of the crate-level
/// `only_the_allowlist_names_a_platform_table_via_the_const`.
///
/// What it does NOT cover, for the same reasons stated at the top of this
/// file: other crates, non-Rust sources, and whether a listed file's uses are
/// still the ones justified below. It also cannot see a caller that reaches
/// these functions through an alias or a re-export — `use
/// repo::products::purge as p;` defeats a source scan, as it defeats the two
/// above it.
const WRITE_ESCAPE_HATCHES: &[(&str, &[&str])] = &[
    (
        // No liveness filter: writes to a soft-deleted row and reports
        // success.
        "products::update_including_deleted",
        &[
            // this file: needs the identifier in order to scan for it
            "blocks/products/tests/repo_door_test.rs",
            // seller suspension. `seller_every_product` reads through
            // `list_all_including_deleted` on purpose — suspension is a fraud
            // control and has to cover every row the seller owns, because
            // soft delete takes nothing down in Stripe — so filtering the
            // write on liveness would silently exempt exactly those rows.
            "blocks/products/handlers/sellers.rs",
            // the `stripe_product_id` write-back after the Stripe Product has
            // already been created. Refusing to record it because the product
            // was deleted mid-sync leaves that Stripe object orphaned, and it
            // is precisely the id `archive_offer_catalog` later needs to take
            // it down.
            "blocks/products/stripe.rs",
        ],
    ),
    (
        // Hard delete. Orphans every `product_id` reference to the row, which
        // is the bug soft delete exists to fix.
        "products::purge",
        &[
            // this file: needs the identifier in order to scan for it
            "blocks/products/tests/repo_door_test.rs",
            // rolling back a product that failed part-way through creation
            // and was never visible to anyone, so it has no references to
            // orphan. Not the delete a user's action reaches — that is
            // `soft_delete`.
            "blocks/products/handlers/product.rs",
        ],
    ),
];

#[test]
fn write_side_escape_hatches_are_allowlisted() {
    let walk = scan();
    for (ident, allowlist) in WRITE_ESCAPE_HATCHES {
        let offenders = callers(&walk, ident, allowlist);
        assert!(
            offenders.is_empty(),
            "these files call `{ident}`, which writes past the soft-delete \
             filter, without being justified in WRITE_ESCAPE_HATCHES. Use the \
             filtered write (`update_live` / `soft_delete`) unless the \
             unfiltered one is genuinely what the operation means, and say why \
             here if it is: {offenders:?}"
        );
    }
}

/// An allowlist entry naming a file that no longer calls the function is a
/// dead exemption: it silently pre-approves whatever that file does next.
/// Every entry must still be a real call site (this file excepted — it holds
/// the identifiers only in order to scan for them).
#[test]
fn no_write_escape_hatch_allowlist_entry_is_dead() {
    let sources = scan().collect();
    for (ident, allowlist) in WRITE_ESCAPE_HATCHES {
        for entry in *allowlist {
            if *entry == "blocks/products/tests/repo_door_test.rs" {
                continue;
            }
            let uses = sources
                .iter()
                .any(|file| &file.rel == entry && file.text.contains(ident));
            assert!(
                uses,
                "`{entry}` is allowlisted for `{ident}` but no longer calls it; \
                 drop the entry rather than leaving a standing exemption"
            );
        }
    }
}

/// The read-side twin of [`WRITE_ESCAPE_HATCHES`], and the one that matters
/// most for what a customer can see.
///
/// The crate-level door test proves nothing *names the products table*
/// outside `repo::products`, and the write list proves no new call site
/// writes past the soft-delete filter. Neither says anything about the four functions in
/// that module that deliberately *read* past it.
///
/// Two of them span both sets, handing back a soft-deleted product's row as
/// if it were live: `get_including_deleted` and `list_all_including_deleted`.
/// Two read the deleted set and only it: `get_deleted` and `list_deleted`.
/// All four are `pub(crate)` and compile cleanly from anywhere in the block,
/// and for this gate the two families are one hazard — a public catalog
/// handler that reaches for `list_deleted` where it meant `list_page` serves
/// a page of soft-deleted products just as surely as one reaching for
/// `list_all_including_deleted` serves them mixed in with the live ones, and
/// the two differ by a word. That is the exact argument the write-side list
/// makes for itself; it applies at least as strongly here, because a leaked
/// read is visible to a buyer whereas a stray write is visible to an admin.
///
/// This list is worth something only while it grows with the module. An
/// escape hatch added to `repo::products` and left unnamed here leaves a gate
/// that READS as coverage while covering nothing, which is worse than no gate
/// at all — `get_deleted` and `list_deleted` arrived after the first two and
/// went unlisted for exactly that stretch, during which the public catalog
/// could have been pointed at the deleted set with every test in this file
/// still green.
///
/// Each call site is listed with its justification, and the same limits
/// apply as above: other crates, non-Rust sources, aliases and re-exports are
/// out of reach, and nothing re-checks that a listed file's uses are still
/// the ones justified here.
const READ_ESCAPE_HATCHES: &[(&str, &[&str])] = &[
    (
        // Reads one row whatever its soft-delete state.
        "products::get_including_deleted",
        &[
            // this file: needs the identifier in order to scan for it
            "blocks/products/tests/repo_door_test.rs",
            // two callers, both of which read a DELETED product on purpose
            // and neither of which shows it to anyone:
            // `archive_offer_catalog` needs `owner_kind`/`owner_id` and
            // `stripe_product_id` to take a deleted product's Prices out of
            // the live Stripe catalog, which is precisely when that most
            // needs doing; and `reconcile_payment_link_session` needs the
            // product name for the buyer's own receipt after Stripe has
            // already captured the money through a Payment Link that soft
            // delete never took down.
            "blocks/products/stripe.rs",
            // `offers::verify_product` under `ProductState::LiveOrDeleted`,
            // which is how an admin or owner reaches the *close* half of a
            // deleted product's money surface: list its offers, archive
            // them, list its Payment Links, deactivate them. Soft delete
            // touches nothing in Stripe, so refusing that read would leave a
            // deleted product's Prices and Links live with no off switch
            // short of restoring the listing to the public catalog. It is
            // never a customer-facing read: every caller is Admin- or
            // Owner-gated, and `ProductState::Live` (the default for
            // everything that creates, edits, publishes, syncs or opens a
            // new way to charge) still goes through `products::get`.
            "blocks/products/handlers/offers.rs",
        ],
    ),
    (
        // Lists rows whatever their soft-delete state.
        "products::list_all_including_deleted",
        &[
            // this file: needs the identifier in order to scan for it
            "blocks/products/tests/repo_door_test.rs",
            // asserts that suspension reached the deleted rows too, which it
            // can only do by reading the set that spans both.
            "blocks/products/tests/seller_governance_tests.rs",
        ],
    ),
    (
        // One owner's rows whatever their soft-delete state — the named
        // shape of the read above, and the one an accidental caller is most
        // likely to reach for when it wanted `list_owned_by`.
        "products::list_owned_by_including_deleted",
        &[
            // this file: needs the identifier in order to scan for it
            "blocks/products/tests/repo_door_test.rs",
            // seller suspension, which is a fraud control and so has to cover
            // every row the seller owns — soft delete takes nothing down in
            // Stripe, so exempting the deleted rows would leave exactly them
            // still taking money.
            "blocks/products/handlers/sellers.rs",
            // pins that this read and `list_owned_by` differ in exactly the
            // deleted rows — it has to name both to say so.
            "blocks/products/tests/repo_tests.rs",
            // pins that suspension's read is exhaustive: it seeds a catalog
            // past the unpaged ceiling and asserts every row comes back,
            // which is the same "covers every row the seller owns" property
            // the `sellers.rs` entry above exists for.
            "blocks/products/tests/bounded_read_tests.rs",
        ],
    ),
    (
        // Reads one row and answers `NotFound` unless it is soft-deleted —
        // `get`'s mirror image, so it never hands back a live row by
        // accident, only a deleted one on purpose.
        "products::get_deleted",
        &[
            // this file: needs the identifier in order to scan for it
            "blocks/products/tests/repo_door_test.rs",
            // `deleted_product_close`, the close-only admin page for a
            // deleted product's money surface. It exists only for a deleted
            // product — that is what this read decides — and the whole page
            // archives offers and deactivates payment links, which soft
            // delete leaves live in Stripe. Reading it through `get` would
            // 404 the one page that can shut a deleted product's charging
            // off. Admin tier is enforced centrally from the declared
            // `/b/products/admin/*` endpoints, so no customer reaches it.
            "blocks/products/pages.rs",
            // `restore_slug_conflict`: after a restore write has already
            // failed on migration 005's partial unique index, this reads the
            // still-deleted row's own slug so the response can name the
            // collision instead of being an opaque 500. Nothing about the row
            // leaves the handler but that slug, and only to the admin who
            // asked for the restore.
            "blocks/products/handlers/product.rs",
        ],
    ),
    (
        // Lists the deleted set and only it — `list_page`'s mirror image,
        // and a one-word slip away from it at every call site.
        "products::list_deleted",
        &[
            // this file: needs the identifier in order to scan for it
            "blocks/products/tests/repo_door_test.rs",
            // `manage_products`' Deleted tab, and the only read in the crate
            // that can find a deleted product at all: every other door
            // refuses one by design, so without this the restore endpoint
            // would exist with nothing able to reach it and soft delete would
            // be permanent in practice. Same central Admin gate as the close
            // page above.
            "blocks/products/pages.rs",
        ],
    ),
];

#[test]
fn read_side_escape_hatches_are_allowlisted() {
    let walk = scan();
    for (ident, allowlist) in READ_ESCAPE_HATCHES {
        let offenders = callers(&walk, ident, allowlist);
        assert!(
            offenders.is_empty(),
            "these files call `{ident}`, which reads past the soft-delete \
             filter and so can hand a soft-deleted product to a caller, \
             without being justified in READ_ESCAPE_HATCHES. Use `get` / \
             `list_capped_live` unless reading a deleted row is genuinely \
             what the operation means, and say why here if it is: \
             {offenders:?}"
        );
    }
}

/// [`no_write_escape_hatch_allowlist_entry_is_dead`] for the read list: an
/// entry naming a file that no longer calls the function silently
/// pre-approves whatever that file does next.
#[test]
fn no_read_escape_hatch_allowlist_entry_is_dead() {
    let sources = scan().collect();
    for (ident, allowlist) in READ_ESCAPE_HATCHES {
        for entry in *allowlist {
            if *entry == "blocks/products/tests/repo_door_test.rs" {
                continue;
            }
            let uses = sources
                .iter()
                .any(|file| &file.rel == entry && file.text.contains(ident));
            assert!(
                uses,
                "`{entry}` is allowlisted for `{ident}` but no longer calls it; \
                 drop the entry rather than leaving a standing exemption"
            );
        }
    }
}

/// The *walk* reaches a planted caller, honours an allowlist entry, and reads
/// only Rust.
///
/// Both scans above prove what they match; neither proves the walk ever
/// opened a file. A root that moved or an extension filter that broke would
/// leave them passing on an empty list — green, and blind to the call site
/// they exist to catch. The floor on [`scan`] is the other half: it fails
/// when the real tree comes back short.
#[test]
fn the_walk_reaches_the_files_it_claims_to_scan() {
    const IDENT: &str = "products::purge";

    let root = std::env::temp_dir().join(format!("products-repo-door-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("handlers")).expect("temp tree");
    std::fs::write(
        root.join("handlers/caller.rs"),
        format!("async fn f() {{ repo::{IDENT}(ctx, id).await }}\n"),
    )
    .expect("caller");
    std::fs::write(
        root.join("allowlisted.rs"),
        format!("async fn f() {{ repo::{IDENT}(ctx, id).await }}\n"),
    )
    .expect("allowlisted caller");
    std::fs::write(root.join("notes.txt"), IDENT).expect("non-rust");

    let found = callers(&SourceWalk::new(&root), IDENT, &["allowlisted.rs"]);
    std::fs::remove_dir_all(&root).expect("clean up");

    assert_eq!(
        found,
        vec!["handlers/caller.rs".to_string()],
        "expected exactly the planted caller"
    );
}
