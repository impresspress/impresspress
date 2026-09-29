//! Every platform table has exactly one door.
//!
//! `src/platform_state/<module>.rs` owns one `impresspress__admin__*` table
//! each, and `src/blocks/<block>/repo/<module>.rs` owns its block's tables
//! the same way: the name, the column names and the row shape. Every other
//! module reaches the table through that module's functions, so a column is
//! spelled in one Rust file and a read cannot skip whatever the door
//! enforces (decoding, the seed hash gate, the single `assign` writer, the
//! one decode of `files.public`, the three writers of
//! `legalpages.documents.status`). The gate is a source scan because the
//! table name is necessarily reachable — a block's `collections(..)` and
//! `grants(..)` registrations name it — so nothing but a test can catch a
//! call site that names it directly. It generalises
//! `blocks/products/tests/repo_door_test.rs`; the block repos join it one PR
//! at a time.
//!
//! Scope: every `.rs` file under this crate's `src/`, with full-line
//! comments removed first — prose naming a table is not a query, and a
//! dozen doc comments describe these tables by name. Trailing comments on
//! code lines are kept, so nothing hides behind a `//` on the same line as
//! code. What this gate still does NOT cover, stated so it is not mistaken
//! for more than it is: other workspace crates (the CLI's `boot_lifecycle` and
//! `native_wrap_grants` tests seed fixtures through `DatabaseService`, and
//! the Cloudflare adapter's `D1ConfigSource` reads the variables table on
//! its production config path; all of them name `platform_state::*::TABLE`
//! and decode through the row types, so they are consumers of this module's
//! public surface rather than bypasses of it, but no test in this crate can
//! see them), non-Rust sources (the migrations under
//! `blocks/admin/migrations/` define the tables), and the files on the
//! allowlists below — each listed individually with its reason, so a NEW
//! file naming a table fails the gate and has to justify itself here.

use impresspress_core::test_support::source_scan::{strip_line_comments, SourceWalk};

/// `(door, table, const)` for every door this gate covers.
///
/// `door` names the door in the failure message and keys the two allowlists.
/// It is the owning module's name wherever a module owns one table; where a
/// module owns two (`files::repo::shares` owns the share rows and their
/// child access log, because a log row is meaningless without its share)
/// each table is its own door, so an exemption for one is not an exemption
/// for the other.
///
/// `const` is the constant's full path from the crate root, which is what the
/// second scan compares every path a file names against once it has been
/// resolved to the same form ([`Names`]). A full path is what tells two
/// same-named constants apart — products' `repo::variables::TABLE` is not
/// the platform's `platform_state::variables::TABLE` — without asking what
/// else the file happens to mention. The file that defines the constant is
/// the door itself and is never an offender.
const TABLES: &[(&str, &str, &str)] = &[
    (
        "variables",
        "impresspress__admin__variables",
        "crate::platform_state::variables::TABLE",
    ),
    (
        "block_settings",
        "impresspress__admin__block_settings",
        "crate::platform_state::block_settings::TABLE",
    ),
    (
        "wrap_grants",
        "impresspress__admin__wrap_grants",
        "crate::platform_state::wrap_grants::TABLE",
    ),
    (
        "request_logs",
        "impresspress__admin__request_logs",
        "crate::platform_state::request_logs::TABLE",
    ),
    (
        "user_roles",
        "impresspress__admin__user_roles",
        "crate::platform_state::user_roles::TABLE",
    ),
    (
        "users",
        "wafer_run__auth__users",
        "crate::blocks::auth::repo::users::TABLE",
    ),
    // The three auth doors this PR adds. `sessions` and `tokens` are the
    // pair B12 re-keyed and wired retention for; `maintenance` is the
    // sweeper's singleton, new in migration 012.
    (
        "sessions",
        "wafer_run__auth__sessions",
        "crate::blocks::auth::repo::sessions::TABLE",
    ),
    (
        "refresh_tokens",
        "wafer_run__auth__tokens",
        "crate::blocks::auth::repo::tokens::TABLE",
    ),
    (
        "auth_maintenance",
        "wafer_run__auth__maintenance",
        "crate::blocks::auth::repo::maintenance::TABLE",
    ),
    (
        "buckets",
        "impresspress__files__buckets",
        "crate::blocks::files::repo::buckets::TABLE",
    ),
    (
        "objects",
        "impresspress__files__objects",
        "crate::blocks::files::repo::objects::TABLE",
    ),
    (
        "shares",
        "impresspress__files__cloud_shares",
        "crate::blocks::files::repo::shares::TABLE",
    ),
    (
        "share_access_logs",
        "impresspress__files__cloud_access_logs",
        "crate::blocks::files::repo::shares::ACCESS_LOGS_TABLE",
    ),
    (
        "quota",
        "impresspress__files__cloud_quotas",
        "crate::blocks::files::repo::quota::TABLE",
    ),
    (
        "views",
        "impresspress__files__views",
        "crate::blocks::files::repo::views::TABLE",
    ),
    (
        "documents",
        "impresspress__legalpages__documents",
        "crate::blocks::legalpages::repo::documents::TABLE",
    ),
    // The products doors. Every table the block declares, each owned by its
    // own `repo/<module>.rs`. `blocks/products/mod.rs` re-exports most of
    // them under a `<NAME>_TABLE` alias for `blocks::dev::data_snapshot`'s
    // closed-list bookkeeping; the resolver follows a re-export to the
    // constant it names, so an alias needs no row of its own.
    (
        "products",
        "impresspress__products__products",
        "crate::blocks::products::repo::products::TABLE",
    ),
    (
        "product_versions",
        "impresspress__products__product_versions",
        "crate::blocks::products::repo::product_versions::TABLE",
    ),
    (
        "offers",
        "impresspress__products__offers",
        "crate::blocks::products::repo::offers::TABLE",
    ),
    (
        "offer_components",
        "impresspress__products__offer_components",
        "crate::blocks::products::repo::offer_components::TABLE",
    ),
    (
        "payment_links",
        "impresspress__products__payment_links",
        "crate::blocks::products::repo::payment_links::TABLE",
    ),
    (
        "checkout_presets",
        "impresspress__products__checkout_presets",
        "crate::blocks::products::repo::checkout_presets::TABLE",
    ),
    (
        "purchases",
        "impresspress__products__purchases",
        "crate::blocks::products::repo::purchases::PURCHASES_TABLE",
    ),
    (
        "line_items",
        "impresspress__products__line_items",
        "crate::blocks::products::repo::purchases::LINE_ITEMS_TABLE",
    ),
    (
        "refunds",
        "impresspress__products__refunds",
        "crate::blocks::products::repo::refunds::TABLE",
    ),
    (
        "disputes",
        "impresspress__products__disputes",
        "crate::blocks::products::repo::disputes::TABLE",
    ),
    (
        "entitlements",
        "impresspress__products__entitlements",
        "crate::blocks::products::repo::entitlements::TABLE",
    ),
    (
        "subscriptions",
        "impresspress__products__subscriptions",
        "crate::blocks::products::repo::subscriptions::SUBSCRIPTIONS_TABLE",
    ),
    (
        "subscription_items",
        "impresspress__products__subscription_items",
        "crate::blocks::products::repo::subscription_items::TABLE",
    ),
    (
        "seller_accounts",
        "impresspress__products__seller_accounts",
        "crate::blocks::products::repo::seller_accounts::TABLE",
    ),
    (
        "provider_operations",
        "impresspress__products__provider_operations",
        "crate::blocks::products::repo::provider_operations::TABLE",
    ),
    (
        "stripe_events",
        "impresspress__products__stripe_events",
        "crate::blocks::products::repo::stripe_events::TABLE",
    ),
    (
        "products_variables",
        "impresspress__products__variables",
        "crate::blocks::products::repo::variables::TABLE",
    ),
    (
        "groups",
        "impresspress__products__groups",
        "crate::blocks::products::repo::groups::TABLE",
    ),
    (
        "types",
        "impresspress__products__types",
        "crate::blocks::products::repo::types::TABLE",
    ),
    (
        "group_templates",
        "impresspress__products__group_templates",
        "crate::blocks::products::repo::group_templates::TABLE",
    ),
    (
        "product_templates",
        "impresspress__products__product_templates",
        "crate::blocks::products::repo::product_templates::TABLE",
    ),
    (
        "llm_settings",
        "impresspress__llm__settings",
        "crate::blocks::llm::repo::settings::TABLE",
    ),
];

/// The walk this gate runs over: every `.rs` file in the crate, with a floor
/// so an empty scan cannot pass as a clean one.
fn scan() -> SourceWalk {
    SourceWalk::crate_src().least(100)
}

/// Every file the walk reaches, as `(path, code)` — the source with its
/// full-line comments dropped, which is what every scan below matches on.
fn sources(walk: &SourceWalk) -> Vec<(String, String)> {
    walk.collect()
        .into_iter()
        .map(|file| (file.rel, strip_line_comments(&file.text)))
        .collect()
}

/// Every file the walk reaches, as `(path, code, names)`: [`sources`]'s pair
/// plus what the raw text names once its paths are resolved to full paths
/// from the crate root, re-exports followed ([`Names`], [`Crate`]). Parsed
/// from the raw text, not the comment-stripped code, so the parse sees the
/// file the compiler sees, and parsed once per test binary: every test that
/// reads it shares the one walk.
fn parsed() -> &'static [(String, String, Names)] {
    static PARSED: std::sync::OnceLock<Vec<(String, String, Names)>> = std::sync::OnceLock::new();
    PARSED.get_or_init(|| {
        let files = scan().collect();
        // `syn` in a debug test build takes seconds over the whole crate on
        // one thread; the files are independent, so parse them across all.
        let threads = std::thread::available_parallelism().map_or(1, usize::from);
        let chunk = files.len().div_ceil(threads).max(1);
        std::thread::scope(|scope| {
            // Spawn every worker before joining any, or they run one by one.
            let mut workers = Vec::new();
            for batch in files.chunks(chunk) {
                workers.push(scope.spawn(move || {
                    batch
                        .iter()
                        .map(|file| {
                            let names = Names::parse(&file.rel, &file.text);
                            let code = strip_line_comments(&file.text);
                            (file.rel.clone(), code, names)
                        })
                        .collect::<Vec<_>>()
                }));
            }
            let mut parsed: Vec<(String, String, Names)> = workers
                .into_iter()
                .flat_map(|w| w.join().expect("a parse worker panicked"))
                .collect();
            Crate::resolve(
                &mut parsed
                    .iter_mut()
                    .map(|(_, _, names)| names)
                    .collect::<Vec<_>>(),
            );
            parsed
        })
    })
}

/// Whether `path` (relative to `src`) is one of `allowlist`'s entries.
/// Exact matches only — no directory prefixes, so an allowlist can never
/// exempt a file that does not exist yet.
fn matches_allowlist(path: &str, allowlist: &[&str]) -> bool {
    allowlist.contains(&path)
}

/// The files, outside `allowed`, whose code satisfies `names` — every scan
/// below is this shape, and stating it once is what lets the walk's own
/// self-test drive the same filter over a planted tree.
fn offenders<'a>(
    sources: &'a [(String, String)],
    allowed: &[&str],
    names: impl Fn(&str) -> bool,
) -> Vec<&'a String> {
    sources
        .iter()
        .filter(|(path, _)| !matches_allowlist(path, allowed))
        .filter(|(_, src)| names(src))
        .map(|(path, _)| path)
        .collect()
}

/// Files allowed to spell a table's literal name, per table. Each entry is
/// a place the name is *defined* or a test fixture that must pin the wire
/// name rather than read it back from the constant it is testing.
const LITERAL_ALLOWED: &[(&str, &[&str])] = &[
    (
        "variables",
        &[
            // the door itself
            "platform_state/variables.rs",
            // the migration runner's tests assert that the embedded DDL
            // carries the index names, which are derived from the table
            // name; the `.sql` files next to it define the table
            "blocks/admin/migrations/mod.rs",
            // the KV row cache's tests pin the wire names its cache keys
            // are derived from — reading them back from the constant would
            // make the test tautological
            "cache_key.rs",
            // a Postgres error-message fixture (`column "block" of relation
            // "…" already exists`) for the duplicate-column detector
            "migration_helper.rs",
            // a guest capability fixture: a sandbox block declaring a
            // foreign table must be refused, and this is the foreign table
            "blocks/dev/validation.rs",
        ],
    ),
    (
        "block_settings",
        &["platform_state/block_settings.rs", "cache_key.rs"],
    ),
    (
        "wrap_grants",
        &["platform_state/wrap_grants.rs", "cache_key.rs"],
    ),
    ("request_logs", &["platform_state/request_logs.rs"]),
    ("user_roles", &["platform_state/user_roles.rs"]),
    // The files doors. Each is its own `repo/<module>.rs` and nothing else,
    // with one exception on the objects table: the WRAP-grant loader's
    // fixture seeds a grant whose target IS a table name on the wire, and
    // resolving it back through `files::repo::objects::TABLE` would make the
    // platform-state test depend on the files block to say what it is
    // testing.
    ("buckets", &["blocks/files/repo/buckets.rs"]),
    (
        "objects",
        &[
            "blocks/files/repo/objects.rs",
            "platform_state/wrap_grants.rs",
        ],
    ),
    (
        "shares",
        &[
            "blocks/files/repo/shares.rs",
            // the admin SQL explorer's refusal list names these two as
            // literals because their owning module is behind a block
            // feature and the table outlives the build that created it;
            // `secret_tables.rs` pins each literal to this door's own
            // constant in a `#[cfg(feature = ..)]` test, and issues no
            // query against either
            "secret_tables.rs",
        ],
    ),
    ("share_access_logs", &["blocks/files/repo/shares.rs"]),
    ("quota", &["blocks/files/repo/quota.rs"]),
    ("views", &["blocks/files/repo/views.rs"]),
    // The legalpages door. Nothing but the door itself: the block declares
    // no `collections(..)` and no `grants(..)` (it owns the one table it
    // touches, so WRAP has nothing to cross-check), which is what leaves this
    // list at one entry.
    ("documents", &["blocks/legalpages/repo/documents.rs"]),
    (
        "users",
        &[
            // the door itself
            "blocks/auth/repo/users.rs",
            // `auth_grants()` spells its grant targets as literals on
            // purpose: the WRAP audit script's const-resolver follows
            // top-level `super::NAME` paths only, not `repo::users::TABLE`
            // (the reason is written out above `auth_grants`)
            "blocks/auth/service.rs",
            // the migration-runner tests assert against the DDL the `.sql`
            // files define; reading the name back from the constant they are
            // testing would make them tautological
            "blocks/auth/migrations/mod.rs",
            // `seed_auth_user` — the ONE raw-SQL users fixture in the crate
            // (a test needs a user under a caller-chosen id that its own
            // authenticated `Message` names; `users::insert` mints a UUID) —
            // plus the WRAP tests whose grant target IS the wire name
            "test_support.rs",
            // the KV row cache classifies tables by wire name; its tests pin
            // the name rather than read it back from the constant
            "cache_key.rs",
            // the WRAP test of the router's auth_version read names, in its
            // failure message, the grant an operator has to go and add
            "crypto.rs",
        ],
    ),
    // The auth session / refresh-token / maintenance doors. Same two
    // categories the `users` door above is exempted under, and nothing else:
    // `auth_grants()` spells its grant targets as literals so the WRAP audit
    // script's const-resolver can follow them, and the migration runner's own
    // tests assert against the DDL the `.sql` files next to them define.
    (
        "sessions",
        &[
            "blocks/auth/repo/sessions.rs",
            "blocks/auth/service.rs",
            "blocks/auth/migrations/mod.rs",
        ],
    ),
    (
        "refresh_tokens",
        &["blocks/auth/repo/tokens.rs", "blocks/auth/service.rs"],
    ),
    // Nothing but the door: the sweeper's singleton is granted through
    // auth-ui's existing `wafer_run__auth__*` wildcard, so no grant literal
    // names it, and the migration tests do not assert on its DDL.
    ("auth_maintenance", &["blocks/auth/repo/maintenance.rs"]),
    // The products doors. Three categories, and nothing else:
    //
    // 1. `blocks/products/repo/<module>.rs` — the door itself, where the
    //    name is defined.
    // 2. `blocks/products/migrations/mod.rs` — the migration runner's own
    //    tests, which necessarily work below the repo layer (migration 020
    //    repairs a row the repo layer can no longer produce), and which
    //    assert against the DDL the `.sql` files next to them define.
    // 3. `blocks/products/tests/*.rs` — fixture setup that seeds rows the
    //    repo layer would not write (soft-deleted products, a pre-migration
    //    stripe event) and asserts on the raw stored row.
    //
    // `blocks/products/stripe.rs` is the one production file on this list,
    // for the reason its door already documents: `repo/stripe_events.rs`
    // owns the name only, and the webhook pipeline that is the table's sole
    // reader and writer predates the convention. Moving that pipeline behind
    // the door is a separate change; the entry says so rather than hiding it.
    (
        "products",
        &[
            // the htmx guard's products fixture: the block's repository is
            // private to it, so the rows no API writes without Stripe are
            // test-fixture rows written straight to the table
            "htmx_guard/products.rs",
            "blocks/products/repo/products.rs",
            "blocks/products/migrations/mod.rs",
            "blocks/products/tests/handler_tests.rs",
            "blocks/products/tests/page_link_tests.rs",
            // category 3: a catalog seeded past the unpaged read ceiling, in
            // one `INSERT … SELECT` over a recursive CTE. Ten thousand rows
            // through `db::create` would take a minute of service dispatch,
            // and the size is the whole point of the test.
            "blocks/products/tests/bounded_read_tests.rs",
        ],
    ),
    (
        "product_versions",
        &[
            "blocks/products/repo/product_versions.rs",
            "blocks/products/migrations/mod.rs",
        ],
    ),
    (
        "offers",
        &[
            "blocks/products/repo/offers.rs",
            "blocks/products/migrations/mod.rs",
            "blocks/products/tests/offer_management_tests.rs",
            "blocks/products/tests/repo_tests.rs",
        ],
    ),
    (
        "offer_components",
        &[
            "blocks/products/repo/offer_components.rs",
            "blocks/products/migrations/mod.rs",
        ],
    ),
    (
        "payment_links",
        &[
            // the htmx guard's products fixture: the block's repository is
            // private to it, so the rows no API writes without Stripe are
            // test-fixture rows written straight to the table
            "htmx_guard/products.rs",
            "blocks/products/repo/payment_links.rs",
            "blocks/products/migrations/mod.rs",
        ],
    ),
    (
        "checkout_presets",
        &[
            "blocks/products/repo/checkout_presets.rs",
            "blocks/products/migrations/mod.rs",
        ],
    ),
    (
        "purchases",
        &[
            // the htmx guard's products fixture: the block's repository is
            // private to it, so the rows no API writes without Stripe are
            // test-fixture rows written straight to the table
            "htmx_guard/products.rs",
            // the admin SQL explorer's refusal list names these two as
            // literals because their owning module is behind a block
            // feature and the table outlives the build that created it;
            // `secret_tables.rs` pins each literal to this door's own
            // constant in a `#[cfg(feature = ..)]` test, and issues no
            // query against either
            "secret_tables.rs",
            "blocks/products/repo/purchases.rs",
            "blocks/products/tests/handler_tests.rs",
            "blocks/products/tests/purchase_tests.rs",
            "blocks/products/tests/repo_tests.rs",
            "blocks/products/tests/seller_governance_tests.rs",
            "blocks/products/tests/stripe_tests.rs",
            // seeds orders past the unpaged read ceiling in one
            // `INSERT … SELECT`; see the note on the products door above
            "blocks/products/tests/bounded_read_tests.rs",
        ],
    ),
    (
        "line_items",
        &[
            "blocks/products/repo/purchases.rs",
            "blocks/products/tests/handler_tests.rs",
            "blocks/products/tests/purchase_tests.rs",
            "blocks/products/tests/stripe_tests.rs",
            // seeds a line per order past the unpaged read ceiling; see the
            // note on the products door above
            "blocks/products/tests/bounded_read_tests.rs",
        ],
    ),
    (
        "refunds",
        &[
            "blocks/products/repo/refunds.rs",
            "blocks/products/migrations/mod.rs",
        ],
    ),
    (
        "disputes",
        &[
            "blocks/products/repo/disputes.rs",
            "blocks/products/migrations/mod.rs",
        ],
    ),
    (
        "entitlements",
        &[
            "blocks/products/repo/entitlements.rs",
            "blocks/products/migrations/mod.rs",
        ],
    ),
    (
        "subscriptions",
        &[
            "blocks/products/repo/subscriptions.rs",
            "blocks/products/tests/repo_tests.rs",
            "blocks/products/migrations/mod.rs",
        ],
    ),
    (
        "subscription_items",
        &[
            "blocks/products/repo/subscription_items.rs",
            "blocks/products/migrations/mod.rs",
        ],
    ),
    (
        "seller_accounts",
        &[
            // the htmx guard's products fixture: the block's repository is
            // private to it, so the rows no API writes without Stripe are
            // test-fixture rows written straight to the table
            "htmx_guard/products.rs",
            "blocks/products/repo/seller_accounts.rs",
            "blocks/products/migrations/mod.rs",
            // seeds a seller population past the unpaged read ceiling; see
            // the note on the products door above
            "blocks/products/tests/bounded_read_tests.rs",
        ],
    ),
    (
        "provider_operations",
        &[
            "blocks/products/repo/provider_operations.rs",
            "blocks/products/migrations/mod.rs",
        ],
    ),
    (
        "stripe_events",
        &[
            "blocks/products/repo/stripe_events.rs",
            "blocks/products/migrations/mod.rs",
            // category 4: the webhook pipeline that predates the convention
            "blocks/products/stripe.rs",
            "blocks/products/tests/handler_tests.rs",
            "blocks/products/tests/stripe_tests.rs",
            // the admin SQL explorer's refusal list names this one as a
            // literal for the same reason as `shares` and `purchases`: its
            // owning module is behind a block feature and the table outlives
            // the build that created it. `secret_tables.rs` pins the literal
            // to this door's own constant in a `#[cfg(feature = ..)]` test,
            // and issues no query against it
            "secret_tables.rs",
        ],
    ),
    ("products_variables", &["blocks/products/repo/variables.rs"]),
    (
        "groups",
        &[
            "blocks/products/repo/groups.rs",
            // the htmx guard's products fixture: the block's repository is
            // private to it, so the rows no API writes without Stripe are
            // test-fixture rows written straight to the table
            "htmx_guard/products.rs",
        ],
    ),
    ("types", &["blocks/products/repo/types.rs"]),
    (
        "group_templates",
        &["blocks/products/repo/group_templates.rs"],
    ),
    (
        "product_templates",
        &[
            "blocks/products/repo/product_templates.rs",
            "blocks/products/migrations/mod.rs",
        ],
    ),
    // The llm settings door. The door itself plus the migration runner's
    // own tests, which assert that the embedded DDL creates the table and
    // its index — reading the name back from the constant they are testing
    // would make them tautological. The block declares no `collections(..)`
    // (its schema is materialised by its migrations, and `mod.rs` says so),
    // so no other non-test file has to name the table.
    (
        "llm_settings",
        &[
            "blocks/llm/repo/settings.rs",
            "blocks/llm/migrations/mod.rs",
        ],
    ),
];

#[test]
fn only_the_door_names_a_platform_table() {
    let sources = sources(&scan());
    for (door, literal, _const) in TABLES {
        let allowed = LITERAL_ALLOWED
            .iter()
            .find(|(m, _)| m == door)
            .map(|(_, files)| *files)
            .unwrap_or(&[]);
        let offenders = offenders(&sources, allowed, |src| src.contains(literal));
        assert!(
            offenders.is_empty(),
            "these files name `{literal}` directly and so bypass \
             the `{door}` door; route them through its functions: {offenders:?}"
        );
    }
}

/// The literal scan catches a call site that spells the name by hand. The
/// likelier mistake is naming the table through the constant — handing
/// `platform_state::variables::TABLE` to `db::list_all` — which compiles
/// cleanly because the constant is `pub` for `blocks/admin`'s
/// `collections(..)` registration. This scan closes that gap: a file that
/// names a door's constant — by its path, relative or absolute, or through
/// an import or re-export that leaves some other spelling at the call site
/// (a grouped `TABLE`, a module alias, a glob, `products::OFFERS_TABLE`;
/// [`names_const`]) — must be on the list below, each entry justified on why
/// it is not a query around the door.
///
/// Attribution is by the constant's full path, so a block's own
/// `repo::variables::TABLE` (products has one) is never the platform's. The
/// doors themselves are not listed: the file that defines a constant is
/// never an offender for it ([`is_door`]).
const IDENT_ALLOWED: &[(&str, &[&str])] = &[
    (
        "variables",
        &[
            // the KV row cache classifies tables by name; it never queries
            "cache_key.rs",
            // the config-snapshot invalidation predicate compares names
            "config_generation.rs",
            // the admin SQL explorer's refusal list: it compares the table
            // name against the text of a submitted query and never issues
            // one. Naming the door's constant is the point — a re-typed
            // literal here would be a second spelling of the table that
            // could drift out of the refusal silently.
            "secret_tables.rs",
            // `BlockInfo::collections(..)` / `grants(..)` are advisory
            // declarations for WRAP and the admin database explorer
            "blocks/admin/mod.rs",
            // the export allowlist/exclusion bookkeeping; its reads go
            // through a generic `db::list_all(ctx, table, ..)` over the
            // allowlist and its import through `seed::import`, and the dev
            // block grants itself those tables (see the audit pragma there)
            "blocks/dev/data_snapshot.rs",
            // A fault injector: the seed's tests fail its one metadata
            // refresh (`database.update`) and its bulk read (`database.list`)
            // on this table, to prove neither stamps the seed hash gate.
            "blocks/admin/settings.rs",
            // Fault injectors: each admin route's WRAP-denial test refuses
            // the table its database site reads or writes, so the denial
            // lands on the query under test.
            "blocks/admin/error_mapping_tests.rs",
        ],
    ),
    (
        "block_settings",
        &[
            "cache_key.rs",
            "config_generation.rs",
            "blocks/admin/mod.rs",
            // names the table only to aim the fault injector
            // (`FailingDbOpContext`) at it in the toggle handler's tests
            "blocks/admin/pages/blocks.rs",
            "blocks/dev/data_snapshot.rs",
            // Fault injectors: each admin route's WRAP-denial test refuses
            // the table its database site reads or writes, so the denial
            // lands on the query under test.
            "blocks/admin/error_mapping_tests.rs",
        ],
    ),
    (
        "wrap_grants",
        &[
            "cache_key.rs",
            "blocks/admin/mod.rs",
            "blocks/dev/data_snapshot.rs",
            // Fault injectors: each admin route's WRAP-denial test refuses
            // the table its database site reads or writes, so the denial
            // lands on the query under test.
            "blocks/admin/error_mapping_tests.rs",
        ],
    ),
    (
        "request_logs",
        &[
            // the queued audit row carries the table name for
            // `after_response::persist_audit_row` (`create_many`) to write off the
            // response path; the inline path calls `request_logs::insert`
            "pipeline.rs",
            "blocks/admin/mod.rs",
            "blocks/dev/data_snapshot.rs",
            // Fault injectors: each admin route's WRAP-denial test refuses
            // the table its database site reads or writes, so the denial
            // lands on the query under test.
            "blocks/admin/error_mapping_tests.rs",
        ],
    ),
    (
        "user_roles",
        &[
            "blocks/admin/mod.rs",
            "blocks/dev/data_snapshot.rs",
            // A fault injector, the same category as `blocks/admin/pages/blocks.rs`:
            // the race test aims `RendezvousDbOpContext` at the grants read
            // `assign` makes, so the two concurrent assigns both pass it
            // before either inserts. It then drives the revoke through
            // `handle_remove_role`, where the reported bug lived. The role
            // delete test aims `FailingDbOpContext` at the grants read of
            // the revocation pass that runs after the role row is deleted.
            "blocks/admin/iam.rs",
            // A fault injector, the same one: the roles tab's delete with
            // that late revocation pass failing.
            "blocks/admin/pages/users.rs",
            // A test fixture that must write past the door: migration 004's
            // test plants twin grants for the repair to collapse, and the
            // door's only writer (`assign`) refuses to make a twin.
            "blocks/admin/migrations/mod.rs",
            // Fault injectors: each admin route's WRAP-denial test refuses
            // the table its database site reads or writes, so the denial
            // lands on the query under test.
            "blocks/admin/error_mapping_tests.rs",
            // A fault injector: the API-key credential check reads the key,
            // its user and THEN the user's grants, and
            // `pipeline::credential_check_tests` fails exactly the grants
            // read. `break_reads` would fail the key lookup first.
            "pipeline.rs",
            // Fault injectors: the token-mint race tests aim
            // `AfterDbOpContext` at the grants read a sign-in and a refresh
            // rotation make after the account row, so a role removal commits
            // between the two; the auth routes' WRAP-denial test refuses the
            // same read.
            "blocks/auth_ui/api/login.rs",
            "blocks/auth_ui/api/refresh.rs",
            "blocks/auth_ui/tests/error_mapping_tests.rs",
        ],
    ),
    (
        "users",
        &[
            // the admin SQL explorer's refusal list; see the note on
            // the `variables` door above
            "secret_tables.rs",
            // the export allowlist/exclusion bookkeeping; its reads go
            // through a generic `db::list_all(ctx, table, ..)` over the
            // allowlist and its import through `seed::import`
            "blocks/dev/data_snapshot.rs",
            // A fault injector, the established second category: refresh
            // reads the tokens table and THEN the users table, and the branch
            // under test is the second read, so `TestContext::break_reads`
            // cannot reach it — it fails the token lookup first and the
            // handler returns before the users read happens. Only
            // `FailingDbOpContext` can fail one table, and it has to be
            // named. The other seven handlers in the same sweep reach their
            // branch on their first read and use `break_reads`, which names
            // no table at all.
            "blocks/auth_ui/api/refresh.rs",
            // A fault injector: a role delete's first write is the
            // auth-version bump of each holder, and the test fails exactly
            // that increment to prove a failed invalidation revokes nothing.
            // `break_reads` cannot reach it — the delete's reads succeed.
            "blocks/admin/iam.rs",
            // Fault injectors: a password change and a reset end sessions by
            // bumping auth_version after the credential write, and the tests
            // fail exactly that increment to prove the failure is reported;
            // the reset's tests also fail its reset-token clear (the users
            // table's `database.update`) to prove sessions end regardless.
            // Both are writes after every read succeeded, where
            // `break_reads` cannot reach.
            "blocks/auth_ui/api/change_password.rs",
            "blocks/auth_ui/api/reset_password.rs",
            // A fault injector: the security page reads the provider links
            // and THEN the user's `email_verified` flag, and the branch under
            // test is the flag read. `break_reads` fails the link list first;
            // only `FailingDbOpContext` aimed at this table reaches it.
            "blocks/userportal/pages/security.rs",
            // Fault injectors: each auth route's WRAP-denial test refuses the
            // one `(action, table)` its site calls — the first users read, or
            // the second one (`generate_tokens`' auth_version read) — so the
            // denial lands on the query under test.
            "blocks/auth_ui/tests/error_mapping_tests.rs",
            // Fault injectors: each admin route's WRAP-denial test refuses
            // the table its database site reads or writes, so the denial
            // lands on the query under test.
            "blocks/admin/error_mapping_tests.rs",
            // Fault injectors: each user-portal route's WRAP-denial test
            // refuses the table its database site reads or writes.
            "blocks/userportal/error_mapping_tests.rs",
            // Fault injectors: a request-time credential check reads the JWT
            // blocklist and the users table (`auth_version`), and
            // `pipeline::credential_check_tests` and `AuthServiceImpl`'s
            // require_user test fail one of those reads per case to prove a
            // failed check refuses the request instead of signing the caller
            // out. `break_reads` fails every read and cannot tell the two
            // apart.
            "pipeline.rs",
            "blocks/auth/service.rs",
        ],
    ),
    // The auth doors B12 adds. Two categories, both already established
    // above: the export allowlist's closed-list bookkeeping, and tests naming
    // a table only to aim `FailingDbOpContext` at it so the injected fault
    // lands on the query under test.
    (
        "sessions",
        &[
            "blocks/dev/data_snapshot.rs",
            // `("database.delete_where_count", sessions::TABLE)` — logout's
            // "a failed session-row delete is not a successful logout" test
            "blocks/auth_ui/api/logout.rs",
            // Fault injectors: each user-portal route's WRAP-denial test
            // refuses the table its database site reads or writes.
            "blocks/userportal/error_mapping_tests.rs",
        ],
    ),
    (
        "refresh_tokens",
        &[
            // the admin SQL explorer's refusal list; see the note on
            // the `variables` door above
            "secret_tables.rs",
            "blocks/dev/data_snapshot.rs",
            // Five `FailingDbOpContext` fixtures across the flows that revoke
            // refresh rows: logout, password change, password reset, refresh
            // rotation, and the userportal per-device revoke.
            "blocks/auth_ui/api/logout.rs",
            "blocks/auth_ui/api/change_password.rs",
            "blocks/auth_ui/api/reset_password.rs",
            "blocks/auth_ui/api/refresh.rs",
            "blocks/userportal/pages/sessions.rs",
            // The WRAP-denial tests of login's refresh-row insert, refresh's
            // token lookup and logout's revocation.
            "blocks/auth_ui/tests/error_mapping_tests.rs",
            // Fault injectors: each user-portal route's WRAP-denial test
            // refuses the table its database site reads or writes.
            "blocks/userportal/error_mapping_tests.rs",
            // `("database.delete_where_count", tokens::TABLE)` — the sweep's
            // "one failing table is named and the others still run" test
            "blocks/auth/maintenance.rs",
        ],
    ),
    (
        "auth_maintenance",
        &[
            // The export decision: the sweep's throttle stamp is scoped to
            // the instance that wrote it, so `TABLE_EXCLUDED` names it. The
            // list is closed, so every table has to be named somewhere in it.
            "blocks/dev/data_snapshot.rs",
            // `("database.get", maintenance::TABLE)` — the throttle's
            // "an unreadable stamp skips rather than sweeps" test
            "blocks/auth/maintenance.rs",
        ],
    ),
    // The files block's two categories, both of which the admin doors above
    // are already exempted under:
    //
    // 1. `blocks/files/mod.rs` — `BlockInfo::collections(..)`. Advisory
    //    declarations for WRAP and the admin database explorer, the same
    //    reason `blocks/admin/mod.rs` is listed for the platform tables.
    //    Every files door needs it; there is no way to declare a collection
    //    without naming it.
    // 2. A test naming the table only to aim `FailingDbOpContext` at it, so
    //    the fault lands on the query under test and not on some other
    //    table's. The same reason `blocks/admin/pages/blocks.rs` is listed.
    //    These are not queries around the door; the door is what runs.
    (
        "buckets",
        &[
            "blocks/files/mod.rs",
            // `("database.delete_where", buckets::TABLE)` — the bucket-delete
            // handler's two compensating-failure tests
            "blocks/files/storage/buckets.rs",
            // category 2: `files/error_mapping_tests.rs` aims each WRAP
            // denial at the one read or write its route site makes
            "blocks/files/error_mapping_tests.rs",
        ],
    ),
    (
        "objects",
        &[
            "blocks/files/mod.rs",
            // `("database.delete_where"/"delete_where_count", objects::TABLE)`
            // — the object-delete metadata-cleanup failure test; and
            // `("database.insert_guarded", objects::TABLE)` — the upload's
            // fail-closed test for the reservation that enforces the quota
            "blocks/files/storage/objects.rs",
            // `("database.sum", objects::TABLE)` — the same, through the
            // `/b/cloudstorage/quota` handler
            "blocks/files/cloud.rs",
            // `("database.aggregate", objects::TABLE)` — the bucket-list
            // page's SECOND read. `break_reads` cannot reach it (the bucket
            // listing fails first), so the outage test names the table to put
            // the fault on the object-count aggregate alone.
            "blocks/files/pages_user/buckets.rs",
            // `("database.list", objects::TABLE)` — the object-list page's
            // "bucket found, listing failed" shape: the ownership check reads
            // the buckets table and must still land.
            "blocks/files/pages_user/objects.rs",
            // category 2: `files/error_mapping_tests.rs` aims each WRAP
            // denial at the one read or write its route site makes
            "blocks/files/error_mapping_tests.rs",
        ],
    ),
    (
        "shares",
        &[
            // the admin SQL explorer's refusal list; see the note on
            // the `variables` door above
            "secret_tables.rs",
            "blocks/files/mod.rs",
            // `("database.get", shares::TABLE)` — the share-delete
            // authorization test: a failed ownership read must stop the
            // request rather than skip the check
            "blocks/files/cloud.rs",
            // `("database.list", shares::TABLE)` — the cloudstorage page's
            // outage test, scoped so the quota reads beside it still land
            "blocks/files/pages_user/cloudstorage.rs",
            // `("database.list"/"database.increment_field_where",
            // shares::TABLE)` — the public link's outage tests: a lookup
            // that failed must not read as a revoked link, and an access
            // the counter could not record must not be served
            "blocks/files/share.rs",
            // category 2: `files/error_mapping_tests.rs` aims each WRAP
            // denial at the one read or write its route site makes
            "blocks/files/error_mapping_tests.rs",
        ],
    ),
    (
        "documents",
        // Category 2 only. The block declares no `collections(..)`, so there
        // is no non-test file that has to name the table at all; this entry
        // is the four `FailingDbOpContext` fixtures in the block's
        // `write_loss_tests`, which name the table so the injected fault
        // lands on the query under test. Same reason
        // `blocks/admin/pages/blocks.rs` is listed above.
        &["blocks/legalpages/mod.rs"],
    ),
    (
        "share_access_logs",
        &[
            "blocks/files/mod.rs",
            // category 2: `files/error_mapping_tests.rs` aims each WRAP
            // denial at the one read or write its route site makes
            "blocks/files/error_mapping_tests.rs",
        ],
    ),
    (
        "quota",
        &[
            "blocks/files/mod.rs",
            // `("database.list", quota::TABLE)` — the upload's fail-closed
            // test for the quota override lookup
            "blocks/files/storage/objects.rs",
            // category 2: `files/error_mapping_tests.rs` aims each WRAP
            // denial at the one read or write its route site makes
            "blocks/files/error_mapping_tests.rs",
        ],
    ),
    (
        "views",
        &[
            "blocks/files/mod.rs",
            // category 2: `files/error_mapping_tests.rs` aims each WRAP
            // denial at the one read or write its route site makes
            "blocks/files/error_mapping_tests.rs",
        ],
    ),
    // The products doors. Four categories:
    //
    // 1. `blocks/products/mod.rs` — `BlockInfo::collections(..)` plus the
    //    curated `block-dev`-gated re-export list that lets
    //    `blocks::dev::data_snapshot` name every collection this block
    //    declares without retyping a literal. Advisory declarations, not
    //    queries; the same reason `blocks/admin/mod.rs` and
    //    `blocks/files/mod.rs` are listed above. Every products door needs
    //    it.
    // 2. `blocks/dev/data_snapshot.rs` — the export allowlist/exclusion
    //    bookkeeping and the `DataSnapshot` JSON keys. Its reads go through
    //    a generic `db::list_all(ctx, table, ..)` over the allowlist and its
    //    writes through `seed::import`; already listed for the platform
    //    doors above for exactly this.
    // 3. `blocks/products/tests/*.rs` — fixtures that seed or assert on raw
    //    rows, and fault injectors (`FailingDbOpContext`) that name the
    //    table so the injected failure lands on the query under test.
    // 4. Two production files that pass the constant to a shared helper
    //    rather than building a query on it: `handlers/group.rs` and
    //    `handlers/types.rs` hand `repo::{groups,types}::TABLE` to
    //    `blocks/crud.rs`'s generic `list_page` / `create_record` /
    //    `update_record` / `delete_record` / `verify_owner` /
    //    `{get,update,delete}_owned`, whose table name always comes from the
    //    caller (the same property that made `crud.rs` carry an
    //    `// audit-allow-file:` pragma for the WRAP audit). Folding those
    //    into per-table repo functions moves the HTTP error mapping `crud`
    //    encapsulates and is a separate change. `blocks/products/stripe.rs`
    //    is the fifth, for the reason `repo/stripe_events.rs` documents.
    //
    // `repo/purchases.rs` and `repo/subscriptions.rs` are on their own
    // lists: their constants are named `PURCHASES_TABLE`,
    // `LINE_ITEMS_TABLE` and `SUBSCRIPTIONS_TABLE` rather than `TABLE`, so
    // the door's own uses match the scan.
    (
        "products",
        &[
            "blocks/products/mod.rs",
            "blocks/dev/data_snapshot.rs",
            "blocks/products/tests/handler_tests.rs",
            "blocks/products/tests/offer_management_tests.rs",
            "blocks/products/tests/offer_pricing_tests.rs",
            "blocks/products/tests/repo_tests.rs",
            "blocks/products/tests/seller_governance_tests.rs",
            "blocks/products/tests/stripe_tests.rs",
            // seeds a storefront product and names the table to refuse one
            // database op on it through `FailingDbOpContext`.
            "blocks/products/tests/error_mapping_tests.rs",
            // names `products::TABLE` for the witness assertion that a
            // one-shot read of the seeded catalog stops at the ceiling — the
            // fact the exhaustive read exists to defeat.
            "blocks/products/tests/bounded_read_tests.rs",
        ],
    ),
    (
        "product_versions",
        &["blocks/products/mod.rs", "blocks/dev/data_snapshot.rs"],
    ),
    (
        "offers",
        &[
            "blocks/products/mod.rs",
            "blocks/dev/data_snapshot.rs",
            "blocks/products/tests/error_mapping_tests.rs",
            "blocks/products/tests/handler_tests.rs",
            "blocks/products/tests/offer_pricing_tests.rs",
            "blocks/products/tests/stripe_tests.rs",
        ],
    ),
    (
        "offer_components",
        &[
            "blocks/products/mod.rs",
            "blocks/dev/data_snapshot.rs",
            "blocks/products/tests/offer_pricing_tests.rs",
        ],
    ),
    (
        "payment_links",
        &[
            "blocks/products/mod.rs",
            "blocks/dev/data_snapshot.rs",
            // a fault injector: `FailingDbOpContext` fails the update that
            // records a link Stripe already created, and nothing else
            "blocks/products/tests/stripe_tests.rs",
        ],
    ),
    (
        "checkout_presets",
        &["blocks/products/mod.rs", "blocks/dev/data_snapshot.rs"],
    ),
    (
        "purchases",
        &[
            // the admin SQL explorer's refusal list; see the note on
            // the `variables` door above
            "secret_tables.rs",
            "blocks/products/mod.rs",
            "blocks/dev/data_snapshot.rs",
            "blocks/products/tests/page_link_tests.rs",
            "blocks/products/tests/provider_tests.rs",
            "blocks/products/tests/storefront_tests.rs",
            "blocks/products/tests/stripe_tests.rs",
            // names `PURCHASES_TABLE`/`LINE_ITEMS_TABLE` for the witness
            // assertion that a one-shot read of the seeded tables stops at
            // the ceiling.
            "blocks/products/tests/bounded_read_tests.rs",
            // Fault injector: the list pages' WRAP-denial test refuses the
            // order read so the denial lands on the page's own query.
            "blocks/products/tests/error_mapping_tests.rs",
        ],
    ),
    (
        "line_items",
        &[
            "blocks/products/mod.rs",
            "blocks/dev/data_snapshot.rs",
            // names `LINE_ITEMS_TABLE` for the witness assertion that a
            // one-shot read of the seeded table stops at the ceiling.
            "blocks/products/tests/bounded_read_tests.rs",
        ],
    ),
    (
        "refunds",
        &[
            "blocks/products/mod.rs",
            "blocks/dev/data_snapshot.rs",
            "blocks/products/tests/purchase_tests.rs",
            // Test-fixture setup: `refund_reconciliation_keeps_the_provider_response_summary`
            // stages the `provider_succeeded` state an interrupted reconcile
            // leaves behind. No product path parks a row there across requests,
            // and `record_provider_response` refuses once the reconcile has
            // stamped `stripe_event_created`. Also a fault injector:
            // `refund_status_when_the_ledger_read_answers` aims
            // `FailingDbOpContext` at the refund-ledger read.
            "blocks/products/tests/provider_tests.rs",
            // A fault injector: `refund_webhook_status_when_the_ledger_read_answers`
            // aims `FailingDbOpContext` at the webhook's refund-ledger read.
            "blocks/products/tests/stripe_tests.rs",
        ],
    ),
    (
        "disputes",
        &[
            "blocks/products/mod.rs",
            "blocks/dev/data_snapshot.rs",
            "blocks/products/tests/handler_tests.rs",
            "blocks/products/tests/purchase_tests.rs",
        ],
    ),
    (
        "entitlements",
        &["blocks/products/mod.rs", "blocks/dev/data_snapshot.rs"],
    ),
    (
        "subscriptions",
        &[
            "blocks/products/mod.rs",
            "blocks/dev/data_snapshot.rs",
            "blocks/products/tests/handler_tests.rs",
            "blocks/products/tests/status_enum_tests.rs",
            "blocks/products/tests/stripe_tests.rs",
        ],
    ),
    (
        "subscription_items",
        &[
            "blocks/products/mod.rs",
            "blocks/dev/data_snapshot.rs",
            "blocks/products/tests/stripe_tests.rs",
        ],
    ),
    (
        "seller_accounts",
        &[
            "blocks/products/mod.rs",
            "blocks/dev/data_snapshot.rs",
            "blocks/products/tests/error_mapping_tests.rs",
            "blocks/products/tests/handler_tests.rs",
            "blocks/products/tests/page_link_tests.rs",
            "blocks/products/tests/provider_tests.rs",
            "blocks/products/tests/repo_tests.rs",
            "blocks/products/tests/seller_governance_tests.rs",
            "blocks/products/tests/status_enum_tests.rs",
            "blocks/products/tests/stripe_tests.rs",
        ],
    ),
    (
        "provider_operations",
        &[
            "blocks/products/mod.rs",
            "blocks/dev/data_snapshot.rs",
            "blocks/products/tests/provider_tests.rs",
        ],
    ),
    (
        "stripe_events",
        &[
            "blocks/products/mod.rs",
            "blocks/dev/data_snapshot.rs",
            // category 4: the webhook pipeline that predates the convention
            "blocks/products/stripe.rs",
            // the admin SQL explorer's refusal list, which names the constant
            // only to assert its own literal still matches it, and issues no
            // query — see the note on the `variables` door above
            "secret_tables.rs",
        ],
    ),
    (
        "products_variables",
        &[
            "blocks/products/mod.rs",
            "blocks/dev/data_snapshot.rs",
            "blocks/products/tests/offer_pricing_tests.rs",
        ],
    ),
    (
        "groups",
        &[
            "blocks/products/mod.rs",
            "blocks/dev/data_snapshot.rs",
            // category 4: `crud::{list_page, create_record, update_record,
            // delete_record, verify_owner, *_owned}` take the table from the
            // caller
            "blocks/products/handlers/group.rs",
            "blocks/products/tests/handler_tests.rs",
            "blocks/products/tests/page_link_tests.rs",
            "blocks/products/tests/repo_tests.rs",
            // Fault injector: the groups page's WRAP-denial test refuses the
            // group read so the denial lands on the page's own query.
            "blocks/products/tests/error_mapping_tests.rs",
        ],
    ),
    (
        "types",
        &[
            "blocks/products/mod.rs",
            "blocks/dev/data_snapshot.rs",
            // category 4, same as groups
            "blocks/products/handlers/types.rs",
        ],
    ),
    // The two template doors carry a fault injector each, the same category
    // as the `llm_settings` entry below: `handler_tests` names the table so
    // `FailingDbOpContext` lands on the default-template lookup a create makes
    // and on nothing else — the point of that test is that the *other* reads
    // in the same create still work.
    (
        "group_templates",
        &[
            "blocks/products/mod.rs",
            "blocks/dev/data_snapshot.rs",
            "blocks/products/tests/handler_tests.rs",
        ],
    ),
    (
        "product_templates",
        &[
            "blocks/products/mod.rs",
            "blocks/dev/data_snapshot.rs",
            "blocks/products/tests/handler_tests.rs",
        ],
    ),
    // The llm settings door. Both entries are fault injectors: the block's
    // `config_tests` and its WRAP-denial tests name the table so
    // `FailingDbOpContext` lands on the settings read or write under test
    // rather than on some other table's. Same category as
    // `blocks/admin/pages/blocks.rs` and `blocks/files/storage/objects.rs`
    // above.
    (
        "llm_settings",
        &["blocks/llm/mod.rs", "blocks/llm/error_mapping_tests.rs"],
    ),
];

/// What one source file's code names, as full paths from the crate root.
///
/// Parsed, not pattern-matched: `syn` reads the file into its scopes — the
/// file itself, each inline `mod`, each block — and records in each one the
/// module it belongs to, the names its `use` items bind, the globs they open,
/// the `const`/`static`/`mod` items it defines, and every path its code
/// spells. Paths inside macro arguments come from the macro's token stream;
/// comments and string literals are not tokens, so neither can name
/// anything — a block id like `"impresspress/products"` in a fixture names no
/// table.
///
/// Every spelled or imported path is then resolved the way the compiler
/// would read it ([`Names::absolute`]): `crate::…` as written, `self::` and
/// `super::` against the scope's module, and a path starting with any other
/// name through what that name is in scope — a `use` binding
/// (`user_roles::{self as ur}` binds `ur`; `use ur::{TABLE}` then binds
/// `TABLE` through `ur`), an item the scope defines (`repo::offers::TABLE`
/// in `blocks/products/mod.rs` is that module's child `repo`), or a glob
/// (`use super::*` in a test module). [`Crate`] then follows re-exports
/// across files, so `blocks::products::OFFERS_TABLE` lands on
/// `blocks::products::repo::offers::TABLE`.
///
/// A name is looked up in its own scope and every enclosing one, and a name
/// bound more than once is resolved through every binding. That
/// over-approximates (an inline `mod` does not really see its parent's
/// imports), and deliberately: the gate is a ban, so a spelling it cannot
/// place must count against the file rather than slip past it. What it never
/// does is attribute a constant to a door by what ELSE the file mentions.
struct Names {
    scopes: Vec<Scope>,
    /// Every path the file spells or imports, resolved to its full path from
    /// the crate root in every form it can take ([`Names::absolute`]) —
    /// before [`Crate`] follows re-exports.
    local: std::collections::HashSet<Vec<String>>,
    /// `local` with every re-export followed ([`Crate::expand`]): what the
    /// file names. Filled in once the whole crate has been parsed.
    named: std::collections::HashSet<Vec<String>>,
}

#[derive(Default)]
struct Scope {
    parent: Option<usize>,
    /// The module this scope's code is in, from the crate root
    /// (`["crate", "blocks", "products"]`). A block shares its module's.
    module: Vec<String>,
    /// Whether this scope IS a module (the file, or an inline `mod`), so its
    /// `use` bindings are items of `module` another file can reach.
    is_module: bool,
    /// Name bound by a `use` → every path it is bound to, as written.
    bindings: std::collections::HashMap<String, Vec<Vec<String>>>,
    /// The module paths opened by a `use …::*`, as written.
    globs: Vec<Vec<String>>,
    /// `globs`, each resolved to its full paths once, so a lookup that
    /// passes a glob does not resolve the glob's own path all over again.
    resolved_globs: Vec<Vec<String>>,
    /// `const` / `static` / `mod` items this scope defines: an explicit item
    /// beats a glob of the same name in the same scope and every scope
    /// inside it.
    own_items: std::collections::HashSet<String>,
    /// Every path the scope's code spells, and whether it is qualified by
    /// something that is not a path segment (`::TABLE`, `<T>::TABLE`), which
    /// makes it no path of this crate's.
    paths: Vec<(Vec<String>, bool)>,
}

/// How many aliases deep a path is followed before the resolver gives up —
/// far more than any real chain, and a stop for a cyclic one.
const RESOLVE_DEPTH: u8 = 8;

impl Names {
    /// `rel` is the file's path under `src/`, which is its module path: the
    /// crate has no `#[path]` attributes, so `blocks/products/mod.rs` is
    /// `crate::blocks::products` and `blocks/products/pages.rs` is
    /// `crate::blocks::products::pages`.
    fn parse(rel: &str, text: &str) -> Self {
        use syn::visit::Visit;

        struct Builder {
            scopes: Vec<Scope>,
            current: usize,
        }
        impl Builder {
            fn scoped(&mut self, module: Option<String>, visit: impl FnOnce(&mut Self)) {
                let outer = self.current;
                let mut path = self.scopes[outer].module.clone();
                path.extend(module.clone());
                self.scopes.push(Scope {
                    parent: Some(outer),
                    module: path,
                    is_module: module.is_some(),
                    ..Scope::default()
                });
                self.current = self.scopes.len() - 1;
                visit(self);
                self.current = outer;
            }
            fn scope(&mut self) -> &mut Scope {
                &mut self.scopes[self.current]
            }
            fn tree(&mut self, prefix: &mut Vec<String>, tree: &syn::UseTree) {
                match tree {
                    syn::UseTree::Path(p) => {
                        prefix.push(p.ident.to_string());
                        self.tree(prefix, &p.tree);
                        prefix.pop();
                    }
                    syn::UseTree::Name(n) => self.bind(prefix, &n.ident, None),
                    syn::UseTree::Rename(r) => self.bind(prefix, &r.ident, Some(&r.rename)),
                    syn::UseTree::Glob(_) => {
                        let glob = prefix.clone();
                        self.scope().globs.push(glob);
                    }
                    syn::UseTree::Group(g) => {
                        for item in &g.items {
                            self.tree(prefix, item);
                        }
                    }
                }
            }
            fn bind(&mut self, prefix: &[String], ident: &syn::Ident, rename: Option<&syn::Ident>) {
                let mut path = prefix.to_vec();
                if ident != "self" {
                    path.push(ident.to_string());
                }
                let Some(last) = path.last().cloned() else {
                    return;
                };
                let name = rename.map_or(last, ToString::to_string);
                if name != "_" {
                    self.scope().bindings.entry(name).or_default().push(path);
                }
            }
            fn record(&mut self, path: &syn::Path, from: usize, pathed: bool) {
                let segments = path
                    .segments
                    .iter()
                    .skip(from)
                    .map(|s| s.ident.to_string())
                    .collect();
                self.scope().paths.push((segments, pathed));
            }
        }
        impl<'ast> Visit<'ast> for Builder {
            fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
                self.scope().own_items.insert(item.ident.to_string());
                if item.content.is_some() {
                    let name = item.ident.to_string();
                    self.scoped(Some(name), |b| syn::visit::visit_item_mod(b, item));
                } else {
                    syn::visit::visit_item_mod(self, item);
                }
            }
            fn visit_block(&mut self, block: &'ast syn::Block) {
                self.scoped(None, |b| syn::visit::visit_block(b, block));
            }
            fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
                self.tree(&mut Vec::new(), &item.tree);
            }
            fn visit_item_const(&mut self, item: &'ast syn::ItemConst) {
                self.scope().own_items.insert(item.ident.to_string());
                syn::visit::visit_item_const(self, item);
            }
            fn visit_item_static(&mut self, item: &'ast syn::ItemStatic) {
                self.scope().own_items.insert(item.ident.to_string());
                syn::visit::visit_item_static(self, item);
            }
            fn visit_path(&mut self, path: &'ast syn::Path) {
                self.record(path, 0, path.leading_colon.is_some());
                syn::visit::visit_path(self, path);
            }
            fn visit_expr_path(&mut self, expr: &'ast syn::ExprPath) {
                match &expr.qself {
                    // `<T as Trait>::NAME`: the segments after the qualified
                    // self are the trait's, never a path in scope.
                    Some(q) => {
                        self.visit_type(&q.ty);
                        self.record(&expr.path, q.position, true);
                    }
                    None => syn::visit::visit_expr_path(self, expr),
                }
            }
            fn visit_type_path(&mut self, ty: &'ast syn::TypePath) {
                match &ty.qself {
                    Some(q) => {
                        self.visit_type(&q.ty);
                        self.record(&ty.path, q.position, true);
                    }
                    None => syn::visit::visit_type_path(self, ty),
                }
            }
            fn visit_macro(&mut self, mac: &'ast syn::Macro) {
                let mut paths = Vec::new();
                collect_paths(mac.tokens.clone(), &mut paths);
                self.scope().paths.extend(paths);
                syn::visit::visit_macro(self, mac);
            }
        }

        let file = syn::parse_file(text)
            .unwrap_or_else(|e| panic!("{rel}: the gate cannot read what it cannot parse: {e}"));
        let mut builder = Builder {
            scopes: vec![Scope {
                module: module_of(rel),
                is_module: true,
                ..Scope::default()
            }],
            current: 0,
        };
        builder.visit_file(&file);
        let mut names = Names {
            scopes: builder.scopes,
            local: std::collections::HashSet::new(),
            named: std::collections::HashSet::new(),
        };
        // Parents come before their children, so each scope's globs resolve
        // against the enclosing scopes' already-resolved ones.
        for at in 0..names.scopes.len() {
            let resolved = names.scopes[at]
                .globs
                .iter()
                .flat_map(|glob| names.absolute(at, glob))
                .collect();
            names.scopes[at].resolved_globs = resolved;
        }
        let mut local = std::collections::HashSet::new();
        for at in 0..names.scopes.len() {
            let scope = &names.scopes[at];
            // A `::`-led or `<T>::`-qualified path is no path of this crate's.
            let spelled = scope
                .paths
                .iter()
                .filter(|(_, pathed)| !pathed)
                .map(|(path, _)| path);
            let imported = scope.bindings.values().flatten();
            for path in spelled.chain(imported) {
                local.extend(names.absolute(at, path));
            }
            local.extend(scope.resolved_globs.iter().cloned());
        }
        names.local = local;
        names
    }

    /// `scope` and every scope enclosing it, innermost first, as indices.
    fn chain(&self, scope: usize) -> impl Iterator<Item = usize> + '_ {
        std::iter::successors(Some(scope), |s| self.scopes[*s].parent)
    }

    /// Every full path (from `crate`) that `path`, spelled in `scope`, can
    /// stand for.
    fn absolute(&self, scope: usize, path: &[String]) -> Vec<Vec<String>> {
        let mut out = Vec::new();
        self.absolute_into(scope, path, RESOLVE_DEPTH, &mut out);
        out
    }

    fn absolute_into(&self, scope: usize, path: &[String], depth: u8, out: &mut Vec<Vec<String>>) {
        let Some(head) = path.first() else {
            return;
        };
        let module = &self.scopes[scope].module;
        let rest = &path[1..];
        match head.as_str() {
            "crate" | "impresspress_core" => {
                out.push(
                    std::iter::once("crate".to_string())
                        .chain(rest.iter().cloned())
                        .collect(),
                );
                return;
            }
            "self" => {
                out.push(module.iter().chain(rest).cloned().collect());
                return;
            }
            "super" => {
                let ups = path.iter().take_while(|s| *s == "super").count();
                // `module` always starts with `crate`, which no `super` climbs past.
                let keep = module.len().saturating_sub(ups).max(1);
                out.push(module[..keep].iter().chain(&path[ups..]).cloned().collect());
                return;
            }
            _ => {}
        }
        if depth == 0 {
            return;
        }
        // Walking outwards: the first scope that binds or defines `head`
        // decides what it is; a glob on the way may also supply it.
        for at in self.chain(scope) {
            let here = &self.scopes[at];
            let bound: Vec<&Vec<String>> = here
                .bindings
                .get(head)
                .into_iter()
                .flatten()
                // `use user_roles;` binds the name to itself — an extern
                // crate or an item already in scope, not an alias.
                .filter(|target| target.as_slice() != [head.clone()])
                .collect();
            if !bound.is_empty() {
                for target in bound {
                    let mut resolved = Vec::new();
                    self.absolute_into(at, target, depth - 1, &mut resolved);
                    out.extend(
                        resolved
                            .into_iter()
                            .map(|r| r.into_iter().chain(rest.iter().cloned()).collect()),
                    );
                }
                return;
            }
            if here.own_items.contains(head) {
                out.push(here.module.iter().chain(path).cloned().collect());
                return;
            }
            for glob in &here.resolved_globs {
                out.push(glob.iter().chain(path).cloned().collect());
            }
        }
        // Nothing in the file binds it: an item of the scope's module this
        // parse does not track (a `fn`, a type), or an extern crate. Either
        // way it is read as the module's own, which names no door.
        out.push(module.iter().chain(path).cloned().collect());
    }
}

/// The module path of the file at `rel` (relative to `src/`).
fn module_of(rel: &str) -> Vec<String> {
    let stem = rel.strip_suffix(".rs").unwrap_or(rel);
    let mut module = vec!["crate".to_string()];
    module.extend(stem.split('/').map(str::to_string));
    if matches!(
        module.last().map(String::as_str),
        Some("mod" | "lib" | "main")
    ) {
        module.pop();
    }
    module
}

/// Every maximal `ident (:: ident)*` run in `tokens`, descending into groups
/// (nested macro arguments included), each with whether a `::` preceded it.
fn collect_paths(tokens: proc_macro2::TokenStream, out: &mut Vec<(Vec<String>, bool)>) {
    use proc_macro2::{Spacing, TokenTree};
    let trees: Vec<TokenTree> = tokens.into_iter().collect();
    let colons = |i: usize| {
        matches!((trees.get(i), trees.get(i + 1)),
            (Some(TokenTree::Punct(a)), Some(TokenTree::Punct(b)))
                if a.as_char() == ':' && a.spacing() == Spacing::Joint && b.as_char() == ':')
    };
    let mut i = 0;
    while i < trees.len() {
        match &trees[i] {
            TokenTree::Group(g) => {
                collect_paths(g.stream(), out);
                i += 1;
            }
            TokenTree::Ident(first) => {
                let pathed = i >= 2 && colons(i - 2);
                let mut path = vec![first.to_string()];
                i += 1;
                while colons(i) {
                    match trees.get(i + 2) {
                        Some(TokenTree::Ident(next)) => {
                            path.push(next.to_string());
                            i += 3;
                        }
                        _ => break,
                    }
                }
                out.push((path, pathed));
            }
            _ => i += 1,
        }
    }
}

/// What the whole crate re-exports: every module-level `use` is an item of
/// its module that another file can name (`crate::blocks::products::
/// OFFERS_TABLE` is `pub(crate) use repo::offers::TABLE as OFFERS_TABLE` in
/// `blocks/products/mod.rs`), and every module-level glob opens its target
/// under the module's name.
#[derive(Default)]
struct Crate {
    /// `module::name` → the full paths the `use` binding it points at.
    aliases: std::collections::HashMap<Vec<String>, Vec<Vec<String>>>,
    /// `module` → the full paths of the modules its globs open.
    globs: std::collections::HashMap<Vec<String>, Vec<Vec<String>>>,
    /// Every item path the crate is known to define (see [`Crate::resolve`]).
    items: std::collections::HashSet<Vec<String>>,
}

impl Crate {
    /// Fill in [`Names::named`] for every file of one crate, re-exports
    /// followed across all of them.
    fn resolve(files: &mut [&mut Names]) {
        let mut krate = Crate::default();
        for names in files.iter().map(|n| &**n) {
            for (at, scope) in names.scopes.iter().enumerate() {
                if !scope.is_module {
                    continue;
                }
                for (name, targets) in &scope.bindings {
                    let key: Vec<String> = scope.module.iter().chain([name]).cloned().collect();
                    let entry = krate.aliases.entry(key).or_default();
                    for target in targets {
                        entry.extend(names.absolute(at, target));
                    }
                }
                for glob in &scope.resolved_globs {
                    krate
                        .globs
                        .entry(scope.module.clone())
                        .or_default()
                        .push(glob.clone());
                }
            }
        }
        // What a glob can supply: the items this crate is known to define —
        // its modules, their `const`/`static`/`mod` items and their `use`
        // bindings. A glob that resolved to anything else
        // (`use wafer_run::prelude::*`) opens nothing here.
        for names in files.iter().map(|n| &**n) {
            for scope in names.scopes.iter().filter(|scope| scope.is_module) {
                krate.items.insert(scope.module.clone());
                for item in &scope.own_items {
                    krate
                        .items
                        .insert(scope.module.iter().chain([item]).cloned().collect());
                }
            }
        }
        krate.items.extend(krate.aliases.keys().cloned());
        for names in files.iter_mut() {
            let mut named = std::collections::HashSet::new();
            for path in &names.local {
                krate.expand(path, RESOLVE_DEPTH, &mut named);
            }
            names.named = named;
        }
    }

    /// `path` and every path it reaches through a re-export: wherever a
    /// prefix of it is a module-level `use`, the binding's target with the
    /// rest appended, and wherever a prefix is a module with a glob, the
    /// glob's target with the rest appended when the target defines the next
    /// segment.
    fn expand(&self, path: &[String], depth: u8, out: &mut std::collections::HashSet<Vec<String>>) {
        if !out.insert(path.to_vec()) || depth == 0 {
            return;
        }
        for k in 1..=path.len() {
            let (prefix, rest) = path.split_at(k);
            for target in self.aliases.get(prefix).into_iter().flatten() {
                let next: Vec<String> = target.iter().chain(rest).cloned().collect();
                self.expand(&next, depth - 1, out);
            }
            // `module::name` through a glob in `module` is `target::name` —
            // when `target` defines `name`, which is all a glob brings in.
            let Some(name) = rest.first() else {
                continue;
            };
            for target in self.globs.get(prefix).into_iter().flatten() {
                let item: Vec<String> = target.iter().chain([name]).cloned().collect();
                if self.items.contains(&item) {
                    let next: Vec<String> =
                        item.into_iter().chain(rest[1..].iter().cloned()).collect();
                    self.expand(&next, depth - 1, out);
                }
            }
        }
    }
}

/// Whether the file `names` describes names the constant at `path` (a full
/// path, `crate::platform_state::user_roles::TABLE`) — spelled out, through
/// an import that binds the constant or its module under any name
/// (`user_roles::{self, TABLE}`, `user_roles::{self as ur}` then
/// `ur::TABLE`, `use ur::{TABLE}` then a bare `TABLE`), through a glob
/// (`user_roles::*`, then a bare `TABLE` that no `const`/`static` between the
/// use and the glob's scope shadows), relative to the file's module
/// (`super::repo::offers::TABLE`), or through a re-export elsewhere in the
/// crate. An import names the constant even before a call site uses it.
fn names_const(names: &Names, path: &str) -> bool {
    let path: Vec<String> = path.split("::").map(str::to_string).collect();
    names.named.contains(&path)
}

/// Whether the file at `rel` is the module that defines the constant at
/// `path` — the door itself, which names its own constant freely.
fn is_door(rel: &str, path: &str) -> bool {
    let module = module_of(rel);
    let owner: Vec<&str> = path
        .rsplit_once("::")
        .map_or(vec![], |(m, _)| m.split("::").collect());
    module.iter().map(String::as_str).eq(owner)
}

/// [`Names`] for a set of fixture files, re-exports resolved across them as
/// the real crate's are.
fn fixture_crate(files: &[(&str, &str)]) -> Vec<Names> {
    let mut parsed: Vec<Names> = files
        .iter()
        .map(|(rel, src)| Names::parse(rel, src))
        .collect();
    Crate::resolve(&mut parsed.iter_mut().collect::<Vec<_>>());
    parsed
}

#[test]
fn a_grouped_import_of_the_const_is_naming_it() {
    for (src, named) in [
        ("use crate::platform_state::user_roles::TABLE;", true),
        (
            "use crate::platform_state::user_roles::{self, UserRoleRow, TABLE};",
            true,
        ),
        (
            "use crate::platform_state::{user_roles::{TABLE as T}, variables};",
            true,
        ),
        (
            "use crate::platform_state::user_roles::{\n    self,\n    TABLE,\n};",
            true,
        ),
        (
            "use crate::platform_state::user_roles::{self, UserRoleRow};",
            false,
        ),
        (
            "use crate::platform_state::user_roles::{self, OTHER_TABLE};",
            false,
        ),
        ("use crate::blocks::x::not_user_roles::{TABLE};", false),
        // another module's constant inside the group
        (
            "use crate::platform_state::user_roles::{self, other::TABLE};",
            false,
        ),
        (
            "use crate::platform_state::user_roles::{self, other::{TABLE}};",
            false,
        ),
        // a module alias, at the top level and inside a group
        (
            "use crate::platform_state::user_roles as ur;\nfn f() { ur::TABLE; }",
            true,
        ),
        (
            "use crate::platform_state::{user_roles as ur, variables};\nfn f() { ur::TABLE; }",
            true,
        ),
        // `self as` inside a group: the crate's rustfmt shape
        (
            "use crate::platform_state::user_roles::{self as ur, UserRoleRow};\nfn f() { ur::TABLE; }",
            true,
        ),
        // an import through an alias, then the bare name
        (
            "use crate::platform_state::user_roles as ur;\nuse ur::{TABLE};\nfn f() { TABLE; }",
            true,
        ),
        // inside a macro's arguments
        (
            "use crate::platform_state::user_roles as ur;\nfn f() { let _ = vec![ur::TABLE]; }",
            true,
        ),
        (
            "use crate::platform_state::user_roles as ur;\nfn f() { ur::UserRoleRow; }",
            false,
        ),
        (
            "use crate::x::not_user_roles as ur;\nfn f() { ur::TABLE; }",
            false,
        ),
        // a glob import, bare or grouped, then the bare name
        (
            "use crate::platform_state::user_roles::*;\nfn f() { db::list(ctx, TABLE); }",
            true,
        ),
        (
            "use crate::platform_state::user_roles::{*};\nfn f() { db::list(ctx, TABLE); }",
            true,
        ),
        (
            "use crate::platform_state::user_roles::*;\nfn f() { db::list(ctx, OTHER_TABLE); }",
            false,
        ),
        (
            "use crate::platform_state::user_roles::*;\nfn f() { other::TABLE; }",
            false,
        ),
        (
            "use crate::x::not_user_roles::*;\nfn f() { db::list(ctx, TABLE); }",
            false,
        ),
        // a glob import next to the file's own `TABLE`: the bare name is the
        // file's
        (
            "use crate::platform_state::user_roles::*;\nconst TABLE: &str = \"t\";\nfn f() { db::list(ctx, TABLE); }",
            false,
        ),
        // a later alias of the same name in another scope does not hide
        // the first one
        (
            "use crate::platform_state::user_roles as ur;\nfn f() { ur::TABLE; }\nmod tests { use crate::other as ur; }",
            true,
        ),
        // a `const TABLE` in one module does not shadow a glob in another
        (
            "mod a { const TABLE: &str = \"t\"; }\nmod b { use crate::platform_state::user_roles::*; fn f() { db::list(ctx, TABLE); } }",
            true,
        ),
        // ... but the module that defines its own `TABLE` uses its own
        (
            "mod a { const TABLE: &str = \"t\"; fn f() { db::list(ctx, TABLE); } }\nmod b { use crate::platform_state::user_roles::*; }",
            false,
        ),
        // a glob inside a fn body reaches a bare name in that body
        (
            "fn f() { use crate::platform_state::user_roles::*; db::list(ctx, TABLE); }",
            true,
        ),
        // `self::` in front of an alias
        (
            "use crate::platform_state::user_roles as ur;\nfn f() { self::ur::TABLE; }",
            true,
        ),
        // comments and strings name nothing
        (
            "/* use crate::platform_state::user_roles::*; */\nfn f() { db::list(ctx, TABLE); }",
            false,
        ),
        (
            "// use crate::platform_state::user_roles::*;\nfn f() { db::list(ctx, TABLE); }",
            false,
        ),
        (
            "fn f() { db::list(ctx, TABLE); } // use crate::platform_state::user_roles::*;",
            false,
        ),
        (
            "fn f() { let _ = \"user_roles::TABLE\"; }",
            false,
        ),
    ] {
        let names = &fixture_crate(&[("case.rs", src)])[0];
        assert_eq!(
            names_const(names, "crate::platform_state::user_roles::TABLE"),
            named,
            "{src}"
        );
    }
}

/// A constant is attributed to a door by the full path it resolves to, and
/// by nothing else the file mentions.
///
/// Each case is a small crate — `(file under src/, source)` pairs — and asks
/// whether its LAST file names one constant. A file that mentions a door's
/// block elsewhere — in a re-export, in a string — does not name the door's
/// constant by doing so, and a file that names the constant through a
/// relative path (`super::repo::variables`) names it without spelling the
/// block at all.
#[test]
fn a_constant_is_attributed_by_its_full_path() {
    const PLATFORM_VARIABLES: &str = "crate::platform_state::variables::TABLE";
    const PRODUCTS_VARIABLES: &str = "crate::blocks::products::repo::variables::TABLE";
    const OFFERS: &str = "crate::blocks::products::repo::offers::TABLE";
    const PRODUCTS_MOD: &str = "pub(crate) use repo::{offers::TABLE as OFFERS_TABLE, purchases::PURCHASES_TABLE};\nmod repo;";
    /// `(file under src/, source)` pairs: one small crate.
    type Files<'a> = &'a [(&'a str, &'a str)];
    let cases: &[(Files, &str, bool)] = &[
        // must not catch: the platform config store's constant next to a
        // products re-export is not products' variables table
        (
            &[
                ("blocks/products/mod.rs", PRODUCTS_MOD),
                (
                    "secret_tables.rs",
                    "use crate::{blocks::products::PURCHASES_TABLE, platform_state::variables};\nfn f() { let _ = [variables::TABLE, PURCHASES_TABLE]; }",
                ),
            ],
            PRODUCTS_VARIABLES,
            false,
        ),
        // ... though it is the platform's
        (
            &[
                ("blocks/products/mod.rs", PRODUCTS_MOD),
                (
                    "secret_tables.rs",
                    "use crate::{blocks::products::PURCHASES_TABLE, platform_state::variables};\nfn f() { let _ = [variables::TABLE, PURCHASES_TABLE]; }",
                ),
            ],
            PLATFORM_VARIABLES,
            true,
        ),
        // must not catch: a block id in a string names no table
        (
            &[(
                "blocks/admin/mod.rs",
                "use crate::platform_state::variables;\nfn f() { let _ = (variables::TABLE, \"impresspress/products\"); }",
            )],
            PRODUCTS_VARIABLES,
            false,
        ),
        // must not catch: products' own `variables` repo is not the platform's
        (
            &[(
                "blocks/products/pages.rs",
                "use super::repo::variables;\nfn f() { let _ = variables::TABLE; }",
            )],
            PLATFORM_VARIABLES,
            false,
        ),
        // must catch: the same file names products' table, with no
        // "products" anywhere in its text
        (
            &[(
                "blocks/products/pages.rs",
                "use super::repo::variables;\nfn f() { let _ = variables::TABLE; }",
            )],
            PRODUCTS_VARIABLES,
            true,
        ),
        // must catch: `super::super::` from a nested module
        (
            &[(
                "blocks/products/handlers/offer.rs",
                "fn f() { let _ = super::super::repo::offers::TABLE; }",
            )],
            OFFERS,
            true,
        ),
        // must catch: a child module named relative to the file's own module
        (
            &[(
                "blocks/products/mod.rs",
                "mod repo;\nfn f() { let _ = repo::offers::TABLE; }",
            )],
            OFFERS,
            true,
        ),
        // must catch: a test module reaching the parent's child through
        // `use super::*`
        (
            &[(
                "blocks/products/pages.rs",
                "mod tests { use super::*; fn f() { let _ = repo::offers::TABLE; } }\nuse super::repo;",
            )],
            OFFERS,
            true,
        ),
        // must catch: a re-exported alias in another file
        (
            &[
                ("blocks/products/mod.rs", PRODUCTS_MOD),
                (
                    "blocks/dev/data_snapshot.rs",
                    "use crate::blocks::products::OFFERS_TABLE;\nfn f() { let _ = OFFERS_TABLE; }",
                ),
            ],
            OFFERS,
            true,
        ),
        // must catch: a module re-exported through a glob
        (
            &[
                ("blocks/products/mod.rs", "pub use repo::*;\nmod repo;"),
                ("blocks/products/repo/mod.rs", "pub mod offers;"),
                (
                    "blocks/dev/data_snapshot.rs",
                    "fn f() { let _ = crate::blocks::products::offers::TABLE; }",
                ),
            ],
            OFFERS,
            true,
        ),
        // must catch: a module alias
        (
            &[(
                "blocks/dev/data_snapshot.rs",
                "use crate::blocks::products::repo::offers as o;\nfn f() { let _ = o::TABLE; }",
            )],
            OFFERS,
            true,
        ),
        // must not catch: another module's `offers`
        (
            &[(
                "blocks/dev/data_snapshot.rs",
                "use crate::blocks::files::repo::offers;\nfn f() { let _ = offers::TABLE; }",
            )],
            OFFERS,
            false,
        ),
        // must not catch: an alias chain that loops names nothing
        (
            &[(
                "blocks/dev/data_snapshot.rs",
                "use a as b;\nuse b as a;\nfn f() { let _ = a::TABLE; }",
            )],
            OFFERS,
            false,
        ),
    ];
    for (files, constant, named) in cases {
        let crate_names = fixture_crate(files);
        let names = crate_names.last().expect("a case has a file");
        assert_eq!(
            names_const(names, constant),
            *named,
            "{constant} in {:?}",
            files.last()
        );
    }
    assert!(is_door("blocks/products/repo/offers.rs", OFFERS));
    assert!(!is_door("blocks/products/repo/mod.rs", OFFERS));
    assert!(is_door("platform_state/variables.rs", PLATFORM_VARIABLES));
}

#[test]
fn only_the_allowlist_names_a_platform_table_via_the_const() {
    let parsed = parsed();
    for (door, _, constant) in TABLES {
        let allowed = IDENT_ALLOWED
            .iter()
            .find(|(m, _)| m == door)
            .map(|(_, files)| *files)
            .unwrap_or(&[]);
        let offenders: Vec<&String> = parsed
            .iter()
            .filter(|(path, _, _)| !matches_allowlist(path, allowed) && !is_door(path, constant))
            .filter(|(_, _, names)| names_const(names, constant))
            .map(|(path, _, _)| path)
            .collect();
        assert!(
            offenders.is_empty(),
            "these files name the table via `{constant}` instead of calling a \
             `{door}` repo function: {offenders:?}"
        );
    }
}

/// An allowlist entry naming a file that no longer names the table is a
/// dead exemption: it silently pre-approves whatever that file does next.
#[test]
fn no_allowlist_entry_is_dead() {
    let parsed = parsed();
    let sources: Vec<(String, String)> = parsed
        .iter()
        .map(|(path, src, _)| (path.clone(), src.clone()))
        .collect();
    let mut dead = Vec::new();
    for (door, literal, constant) in TABLES {
        for (m, files) in LITERAL_ALLOWED {
            if m != door {
                continue;
            }
            for entry in *files {
                assert!(
                    sources
                        .iter()
                        .any(|(path, src)| path == entry && src.contains(literal)),
                    "`{entry}` is allowlisted for the `{literal}` literal but no longer \
                     names it; drop the entry rather than leaving a standing exemption"
                );
            }
        }
        for (m, files) in IDENT_ALLOWED {
            if m != door {
                continue;
            }
            for entry in *files {
                if is_door(entry, constant) {
                    dead.push(format!(
                        "`{entry}` is the `{door}` door itself, which is never an offender"
                    ));
                } else if !parsed
                    .iter()
                    .any(|(path, _, names)| path == entry && names_const(names, constant))
                {
                    dead.push(format!(
                        "`{entry}` is allowlisted for `{constant}` but no longer names it"
                    ));
                }
            }
        }
    }
    assert!(
        dead.is_empty(),
        "drop these entries rather than leaving a standing exemption:\n{}",
        dead.join("\n")
    );
}

/// The old names are gone: `admin_schema.rs` and the `blocks::admin`
/// re-exports (`BLOCK_SETTINGS_TABLE`, `WRAP_GRANTS_TABLE`,
/// `REQUEST_LOGS_TABLE`, `USER_ROLES_TABLE`, `admin::VARIABLES_TABLE`),
/// `messages_schema.rs` (the module that existed so `blocks/llm` could read
/// the messages block's tables by name), and `PRODUCTS_TABLE` (the products
/// table's pre-`repo` constant, previously guarded by the block's own door
/// test). A file that still imports one would compile only by redefining it,
/// which is the same bypass wearing the old name. (`VARIABLES_TABLE` on its
/// own is not banned: products aliases its own `repo::variables::TABLE` to
/// it.)
#[test]
fn the_old_table_name_shims_are_gone() {
    let sources = sources(&scan());
    for old in [
        "admin_schema::",
        "mod admin_schema",
        "BLOCK_SETTINGS_TABLE",
        "WRAP_GRANTS_TABLE",
        "REQUEST_LOGS_TABLE",
        "USER_ROLES_TABLE",
        "admin::VARIABLES_TABLE",
        "messages_schema::",
        "mod messages_schema",
        "PRODUCTS_TABLE",
    ] {
        let offenders = offenders(&sources, &[], |src| src.contains(old));
        assert!(
            offenders.is_empty(),
            "`{old}` still referenced in {offenders:?}"
        );
    }
}

/// The messages block's two tables are named only inside the messages block.
///
/// This is the cross-block half of the same rule, and it is stated as a
/// boundary rather than as a door because `messages/rest.rs` genuinely hands
/// `service::{CONTEXTS_TABLE, ENTRIES_TABLE}` to shared helpers
/// (`crud::verify_owner`, `crud::delete_record`) whose table comes from the
/// caller — allowlisting that file would buy a standing exemption for
/// nothing, since the risk this test exists for was never inside the block.
/// It was `blocks/llm/pages.rs`, which listed both tables with `db::list`
/// while the same block wrote through `ctx.call_block`. That direct read is
/// the whole reason `messages_schema.rs` existed and the reason
/// `messages/mod.rs` had to grant `impresspress/llm` read access to two
/// tables it does not own.
#[test]
fn the_messages_tables_are_named_only_inside_the_messages_block() {
    let sources = sources(&scan());
    for name in [
        "impresspress__messages__contexts",
        "impresspress__messages__entries",
        "CONTEXTS_TABLE",
        "ENTRIES_TABLE",
    ] {
        let offenders: Vec<&String> = sources
            .iter()
            .filter(|(path, _)| !path.starts_with("blocks/messages/"))
            .filter(|(_, src)| src.contains(name))
            .map(|(path, _)| path)
            .collect();
        assert!(
            offenders.is_empty(),
            "`{name}` belongs to `impresspress/messages`; these files outside \
             `blocks/messages/` name it instead of calling the block through \
             `ctx.call_block(\"impresspress/messages\", ..)`: {offenders:?}"
        );
    }
}

/// The *walk* reaches a planted offender, honours an allowlist entry, and
/// reads only Rust — over the same `sources` pipeline every scan above runs.
///
/// Each scan above proves what it matches; none of them proves the walk ever
/// opened a file. A root that moved or an extension filter that broke would
/// leave every assertion here passing on an empty list of sources, which is
/// the failure mode that makes most source gates worthless. The floor on
/// [`scan`] is the other half: it fails when the real tree comes back short.
#[test]
fn the_walk_reaches_the_files_it_claims_to_scan() {
    const LITERAL: &str = "impresspress__admin__variables";

    let root = std::env::temp_dir().join(format!("repo-door-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("nested")).expect("temp tree");
    std::fs::write(
        root.join("nested/offender.rs"),
        format!("let t = \"{LITERAL}\";\n"),
    )
    .expect("offender");
    std::fs::write(
        root.join("door.rs"),
        format!("pub const T: &str = \"{LITERAL}\";\n"),
    )
    .expect("the allowlisted door");
    std::fs::write(
        root.join("prose.rs"),
        format!("// {LITERAL} named in a comment, not queried\n"),
    )
    .expect("prose only");
    std::fs::write(root.join("notes.txt"), LITERAL).expect("non-rust");

    let sources = sources(&SourceWalk::new(&root));
    std::fs::remove_dir_all(&root).expect("clean up");

    let found = offenders(&sources, &["door.rs"], |src| src.contains(LITERAL));
    assert_eq!(
        found,
        vec![&"nested/offender.rs".to_string()],
        "expected exactly the planted offender"
    );
}
