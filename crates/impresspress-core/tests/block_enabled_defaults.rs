//! The guard for spec 2.1.4: switching the block-enablement seed from the
//! hand-maintained `features::ENABLED_DEFAULTS` table (deleted by this PR, and
//! reproduced below as [`SHIPPED_DEFAULTS_BEFORE_PR2`]) to the blocks' own
//! `BlockInfo` declarations must not change what any existing deployment ends
//! up with.
//!
//! Why this needs a test at all: `plan_seed_decisions` re-seeds every row whose
//! `seed_defaults_hash` is a *stale* `seed:<hex>` (the #222 hash gate). A row
//! written at `seed_hash_for(true)` becomes stale the moment the derived
//! default for that block is `false`, and the next cold start rewrites it to
//! `false`. So a divergence between the constant and a `.default_enabled(..)`
//! declaration was not a cosmetic inconsistency — it was a silent,
//! deployment-wide
//! flip of a feature flag.
//!
//! The three tests below cover the three ways the two sets can differ:
//! same name in both (values must agree), shipped-only, derived-only.

use std::collections::BTreeMap;

use impresspress_core::blocks::all_block_infos;

/// `features::ENABLED_DEFAULTS` exactly as it stood before this PR — the eight
/// rows every deployment's boot seed has been writing.
///
/// This is a historical record, not a specification. It exists so the three
/// tests below can still say what *was* running once the constant itself is
/// gone. Never edit it to agree with a new derivation: an entry that stops
/// matching is a live deployment's `block_settings` row about to be rewritten,
/// and that is a decision to take deliberately (spec 5.6), not a test to fix.
const SHIPPED_DEFAULTS_BEFORE_PR2: &[(&str, bool)] = &[
    ("wafer-run/auth", true),
    ("impresspress/files", true),
    ("impresspress/legalpages", true),
    ("impresspress/tickets", false),
    ("impresspress/messages", true),
    ("impresspress/products", true),
    ("impresspress/system", true),
    ("impresspress/userportal", true),
];

/// The derived set, read from the function the seed callers use.
fn derived_defaults() -> BTreeMap<String, bool> {
    impresspress_core::blocks::block_enabled_defaults()
        .into_iter()
        .collect()
}

/// Every block name the registry knows about, whether or not it is disableable.
fn registered_names() -> BTreeMap<String, bool> {
    all_block_infos()
        .iter()
        .map(|i| (i.name.clone(), i.can_disable))
        .collect()
}

/// A block present in both sets must be seeded to the same value by both, or
/// the switch flips it on every deployment that has not been admin-edited.
#[test]
fn derived_defaults_match_what_shipped() {
    let derived = derived_defaults();
    let mut mismatches = Vec::new();

    for (name, shipped_value) in SHIPPED_DEFAULTS_BEFORE_PR2 {
        if let Some(derived_value) = derived.get(*name) {
            if derived_value != shipped_value {
                mismatches.push(format!(
                    "  {name}: the shipped table says {shipped_value}, \
                     BlockInfo::default_enabled says {derived_value}"
                ));
            }
        }
    }

    assert!(
        mismatches.is_empty(),
        "every block in both the shipped table and the derived set must agree, \
         otherwise switching the seed source re-seeds live rows:\n{}",
        mismatches.join("\n"),
    );
}

/// A name the shipped table seeded but the derivation drops must be safe to
/// drop: it is either not a registered block at all (`wafer-run/auth`, registered
/// through `register_auth` and absent from `all_block_infos()`) or it declares
/// `can_disable == false` (`impresspress/system`, `impresspress/admin`), and in
/// either case it shipped as `true` — which is exactly what
/// `BlockSettings::is_block_enabled` reports for an absent row.
#[test]
fn shipped_only_entries_are_not_can_disable_and_default_true() {
    let derived = derived_defaults();
    let registered = registered_names();

    for (name, shipped_value) in SHIPPED_DEFAULTS_BEFORE_PR2 {
        if derived.contains_key(*name) {
            continue;
        }
        match registered.get(*name) {
            None => { /* not a registered block; the seed row was dead weight */ }
            Some(can_disable) => assert!(
                !can_disable,
                "{name} is registered and disableable but the derivation dropped it — \
                 that would be a real behaviour change, not a fallback"
            ),
        }
        assert!(
            *shipped_value,
            "{name} loses its seed row, so it falls back to enabled; it shipped \
             as {shipped_value}, so dropping the row changes behaviour"
        );
    }
}

/// A name the derivation adds that the shipped table never seeded must be
/// adding a row at `true` — the same value `is_block_enabled` already reported
/// for its absent row. `impresspress/llm` and `impresspress/vector` are the
/// two: the constant excluded them with a comment about a trait refactor that
/// has since landed (`blocks/mod.rs`).
#[test]
fn derived_only_entries_default_true() {
    let shipped: BTreeMap<&str, bool> = SHIPPED_DEFAULTS_BEFORE_PR2
        .iter()
        .map(|(name, value)| (*name, *value))
        .collect();

    for (name, derived_value) in derived_defaults() {
        if shipped.contains_key(name.as_str()) {
            continue;
        }
        assert!(
            derived_value,
            "{name} gains a seed row it never had; only `true` matches the \
             absent-row fallback it has been running on"
        );
    }
}
