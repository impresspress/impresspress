//! Pure functions translating `(table, ListOptions)` and `(table, row)`
//! into KV cache keys. Determines which `DatabaseService` calls qualify
//! for caching and how to derive their key.
//!
//! Consumed by `impresspress-cloudflare::kv_cached_db`. Pure data-mapping
//! logic lives here so it's host-testable; `impresspress-cloudflare` is
//! excluded from `cargo test --workspace`.

use crate::platform_state::{block_settings, variables, wrap_grants};

/// Tables that this wrapper caches in KV.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CachedTable {
    /// `impresspress__admin__variables` — config var rows, keyed by `block` column.
    Variables,
    /// `impresspress__admin__block_settings` — per-block migration state, keyed by `block_name` column.
    BlockSettings,
}

/// Returns Some when `table` is one of the cached tables.
pub fn classify_table(table: &str) -> Option<CachedTable> {
    match table {
        t if t == variables::TABLE => Some(CachedTable::Variables),
        t if t == block_settings::TABLE => Some(CachedTable::BlockSettings),
        _ => None,
    }
}

/// KV key holding the opaque config-generation stamp. The Cloudflare entry
/// compares this against the version its cached runtime was built at and
/// rebuilds on mismatch. Rewritten (not incremented) on every bump.
pub const CONFIG_VERSION_KEY: &str = "cfg:v1:config_version";

/// True when a write to `table` must bump [`CONFIG_VERSION_KEY`] — i.e. the
/// table feeds state that a cached runtime bakes in at build/init time:
/// `variables` (block config consumed at Init), `block_settings` (router
/// enablement map consumed at build), `wrap_grants` (loaded into runtime
/// grants at build). Tables read fresh per request (roles, permissions,
/// user_roles) do NOT bump.
pub fn bumps_config_version(table: &str) -> bool {
    table == variables::TABLE || table == block_settings::TABLE || table == wrap_grants::TABLE
}

use wafer_block::db::{Filter, FilterOp, ListOptions};

/// Minimum `limit` value treated as "all matching rows". Matches the
/// `D1ConfigSource` and admin block list shapes. Anything smaller is
/// treated as paginated and bypasses cache.
const FULL_LIMIT_THRESHOLD: u32 = 10_000;

/// Reserved cache-key value for the full-table `block_settings` read —
/// `platform_state::block_settings::load`'s eager filterless list. Real block names
/// are always `{org}/{block}` (slash-delimited), so this slash-free
/// sentinel can never collide with a per-block key.
const ALL_ROWS_SENTINEL: &str = "__all__";

/// The block-identifying column for each cached table's canonical list
/// query. Single source of truth shared by [`read_key`] (the classifier)
/// and [`block_list_opts`] (the constructor) so the two can't drift.
fn key_column(table: CachedTable) -> &'static str {
    match table {
        CachedTable::Variables => "block",
        CachedTable::BlockSettings => "block_name",
    }
}

/// Build the canonical "load all rows for one block" [`ListOptions`] that
/// [`read_key`] recognizes as cacheable.
///
/// Single source of truth for the cached per-block query shape: a caller that
/// wants a KV-cached per-block read constructs its `ListOptions` here instead
/// of open-coding the shape, so it can't silently drift out of cache coverage.
///
/// NO PRODUCTION CALLER TODAY. `D1ConfigSource` was the only one; it now reads
/// the variables table once via [`full_table_list_opts`] and groups in memory,
/// because one query per block meant one KV read per block on every cold
/// hydration. With no caller issuing that shape, [`read_key`] never matches a
/// variables read, so `cfg:v1:variables:*` keys are no longer written EITHER —
/// any left over from before expire on their 24h TTL. Writes still emit an
/// invalidating `delete` per variables row (see [`invalidate_keys`]),
/// which now targets keys that cannot exist; that is deliberate belt-and-braces
/// for a future cached per-block reader rather than an oversight, but it does
/// spend a KV write op per row and is worth revisiting if the write budget
/// tightens.
/// This constructor stays because the per-block shape is still what
/// [`read_key`] recognizes and [`write_key`] invalidates against, and the
/// round-trip test pins the two together.
pub fn block_list_opts(table: CachedTable, value: &str) -> ListOptions {
    ListOptions {
        filters: vec![Filter {
            field: key_column(table).to_string(),
            operator: FilterOp::Equal,
            value: serde_json::Value::String(value.to_string()),
        }],
        limit: Some(FULL_LIMIT_THRESHOLD),
        offset: 0,
        skip_count: true,
        ..Default::default()
    }
}

/// Build the canonical "load every row in the table" [`ListOptions`].
///
/// Two callers with deliberately different outcomes, both served by this one
/// constructor so the shape cannot drift between them:
///
/// - [`crate::platform_state::block_settings::load`]'s full-table read — recognized
///   by [`read_key`] and cached under the `__all__` sentinel;
/// - `D1ConfigSource`'s single variables snapshot — deliberately NOT cached
///   (see `read_key`'s zero-filter arm, and the test that pins it). One
///   uncached D1 query replaces one KV read per configured block, which on a
///   22-block deployment is 22 KV reads traded for a single unfiltered read
///   of a table of a few dozen rows.
pub fn full_table_list_opts() -> ListOptions {
    ListOptions {
        filters: Vec::new(),
        limit: Some(FULL_LIMIT_THRESHOLD),
        offset: 0,
        skip_count: true,
        ..Default::default()
    }
}

/// Returns Some(kv_key) iff `opts` matches a cacheable read shape:
/// either the canonical "load all rows for one block" single-filter shape,
/// or — for `block_settings` only — the eager filterless full-table read
/// (`platform_state::block_settings::load`).
pub fn read_key(table: CachedTable, opts: &ListOptions) -> Option<String> {
    // Shape gate shared by both read kinds: an unsorted, unpaginated,
    // count-skipping "give me every matching row" list.
    if !opts.skip_count
        || opts.offset != 0
        || !matches!(opts.limit, Some(limit) if limit >= FULL_LIMIT_THRESHOLD)
        || !opts.sort.is_empty()
    {
        return None;
    }
    match opts.filters.len() {
        // Full-table read. For `block_settings` (the eager filterless list
        // `platform_state::block_settings::read_rows` issues, reached from
        // `load` and `load_and_seed`) cache it under the all-rows sentinel.
        // For `variables` REFUSE it: the only filterless
        // variables list is `D1ConfigSource`'s whole-table snapshot, and
        // there is no invalidation story for a whole-table variables key —
        // `invalidate_keys` emits the all-rows key for `block_settings`
        // alone, so a variables write would leave such an entry stale until
        // its TTL.
        0 => match table {
            CachedTable::BlockSettings => Some(format_key(table, ALL_ROWS_SENTINEL)),
            CachedTable::Variables => None,
        },
        // Per-block read keyed on the table's identity column.
        1 => {
            let f = &opts.filters[0];
            if !matches!(f.operator, FilterOp::Equal) {
                return None;
            }
            if f.field != key_column(table) {
                return None;
            }
            let value_str = f.value.as_str()?;
            Some(format_key(table, value_str))
        }
        _ => None,
    }
}

fn format_key(table: CachedTable, value: &str) -> String {
    let tag = match table {
        CachedTable::Variables => "variables",
        CachedTable::BlockSettings => "block_settings",
    };
    format!("cfg:v1:{tag}:{value}")
}

use std::collections::HashMap;

/// Pulls the cache-key column from a row payload. Returns Some(kv_key)
/// when the column is present and string-typed.
pub fn write_key(table: CachedTable, row: &HashMap<String, serde_json::Value>) -> Option<String> {
    let value_str = row.get(key_column(table))?.as_str()?;
    Some(format_key(table, value_str))
}

/// All KV keys a single-row write (create / update / delete) to `row` in
/// `table` must invalidate.
///
/// Always includes the per-row key when the identity column is extractable.
/// For `block_settings` it additionally includes the all-rows key, because
/// `block_settings::read_rows`' cached full-table read depends on every row — so
/// any insert / toggle / delete must drop it. The all-rows key is emitted
/// unconditionally for `block_settings` (even when the per-row key can't be
/// extracted) so the full-table cache can never be left stale.
pub fn invalidate_keys(
    table: CachedTable,
    row: &HashMap<String, serde_json::Value>,
) -> Vec<String> {
    let mut keys = Vec::new();
    if let Some(k) = write_key(table, row) {
        keys.push(k);
    }
    if table == CachedTable::BlockSettings {
        let all = format_key(table, ALL_ROWS_SENTINEL);
        if !keys.contains(&all) {
            keys.push(all);
        }
    }
    keys
}

/// All KV keys an `update` must invalidate: the union of the keys the OLD
/// row would invalidate and the keys the NEW (post-update) row would
/// invalidate, deduplicated.
///
/// A plain single-row `invalidate_keys` call on just the old row (or just
/// the new one) misses the other row's key whenever the update changed the
/// identity column the cache key is derived from (`block` for `Variables`,
/// `block_name` for `BlockSettings`): the row moves to a fresh cache key,
/// and whichever key isn't invalidated keeps serving a stale/mismatched
/// entry until its 24h TTL expires. Invalidating the union covers both the
/// vacated old key and the (possibly not-yet-cached, but worth dropping
/// pre-emptively) new key.
pub fn invalidate_keys_for_update(
    table: CachedTable,
    old_row: &HashMap<String, serde_json::Value>,
    new_row: &HashMap<String, serde_json::Value>,
) -> Vec<String> {
    let mut keys = invalidate_keys(table, old_row);
    for k in invalidate_keys(table, new_row) {
        if !keys.contains(&k) {
            keys.push(k);
        }
    }
    keys
}

/// The `(key_column, sensitive_flag_column)` pair `row_is_sensitive` reads to
/// decide whether a row must never be written to the KV cache. `None` when
/// `table` has no notion of a sensitive row (`block_settings` rows are
/// per-block enablement/migration flags, not secrets).
fn sensitive_check_columns(table: CachedTable) -> Option<(&'static str, &'static str)> {
    match table {
        CachedTable::Variables => Some(("key", "sensitive")),
        CachedTable::BlockSettings => None,
    }
}

/// True when `row` in `table` holds a value that must never be written to
/// the KV cache (Cloudflare KV is a globally replicated, eventually
/// consistent store with no encryption-at-rest guarantee this runtime
/// controls — OAuth/Stripe/email credentials must not be duplicated into
/// it, even for up to the 24h row-cache TTL).
///
/// Reuses the exact SEC-060 rule the admin Variables page and the generic
/// `ConfigVar`-driven settings form use to mask/redact secrets
/// ([`crate::util::is_sensitive_key`]), so the cache-write policy can never
/// drift from the display-masking policy: a row is sensitive when its
/// `sensitive` flag is set OR its key is one the build knows to hold a secret
/// — the `_SECRET`/`_KEY` suffix convention, or a declared `ConfigVar` that is
/// `InputType::Password` or `auto_generate`. The declaration half matters
/// here specifically: `WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_PASSWORD`
/// carries neither suffix, so a legacy row with the flag still clear was
/// judged cacheable and a plaintext admin password was copied into a globally
/// replicated store.
///
/// Uses [`crate::util::flag_is_set`] (not a bare `v.as_i64()`) for the
/// `sensitive` column so this stays in exact parity with the display-masking
/// path: the SQLite service can round-trip a lazily-added column as a TEXT
/// `"1"` string, a bool, or a float, and a flag-only-sensitive row stored any
/// of those ways must still be treated as sensitive here, or it would leak
/// into KV while the display path correctly masks it. See the note in the
/// body on why `json_as_i64` was the wrong decoder for exactly this.
pub fn row_is_sensitive(table: CachedTable, row: &HashMap<String, serde_json::Value>) -> bool {
    let Some((key_col, sensitive_col)) = sensitive_check_columns(table) else {
        return false;
    };
    let key = row.get(key_col).and_then(|v| v.as_str()).unwrap_or("");
    // `flag_is_set`, not `json_as_i64`: that conversion answers `None` for a
    // JSON bool and for the string `"true"`, so a row stored in either shape
    // read as UNFLAGGED here while `RecordExt::bool_field` — which the repair
    // pass and the row codec use — read it as flagged. The row was therefore
    // skipped as "already fine" and cached as "not sensitive" at the same
    // time. One truth table for the column, shared with both.
    let sensitive_flag = i64::from(row.get(sensitive_col).is_some_and(crate::util::flag_is_set));
    crate::util::is_sensitive_key(key, sensitive_flag)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_table_variables() {
        assert_eq!(
            classify_table("impresspress__admin__variables"),
            Some(CachedTable::Variables)
        );
    }

    #[test]
    fn classify_table_block_settings() {
        assert_eq!(
            classify_table("impresspress__admin__block_settings"),
            Some(CachedTable::BlockSettings)
        );
    }

    #[test]
    fn classify_table_unknown_returns_none() {
        assert_eq!(classify_table("wafer_run__auth__users"), None);
        assert_eq!(classify_table(""), None);
        assert_eq!(classify_table("variables"), None);
    }

    use wafer_block::db::{Filter, FilterOp, ListOptions};

    fn canonical_opts(field: &str, value: &str) -> ListOptions {
        ListOptions {
            filters: vec![Filter {
                field: field.into(),
                operator: FilterOp::Equal,
                value: serde_json::Value::String(value.into()),
            }],
            limit: Some(10_000),
            offset: 0,
            skip_count: true,
            ..Default::default()
        }
    }

    #[test]
    fn read_key_variables_canonical_shape() {
        let opts = canonical_opts("block", "WAFER_RUN__AUTH");
        assert_eq!(
            read_key(CachedTable::Variables, &opts),
            Some("cfg:v1:variables:WAFER_RUN__AUTH".to_string())
        );
    }

    #[test]
    fn read_key_block_settings_canonical_shape() {
        let opts = canonical_opts("block_name", "wafer-run/registry");
        assert_eq!(
            read_key(CachedTable::BlockSettings, &opts),
            Some("cfg:v1:block_settings:wafer-run/registry".to_string())
        );
    }

    #[test]
    fn block_list_opts_roundtrips_through_read_key() {
        // The constructor must always produce a shape the classifier
        // recognizes — this is the contract that keeps cached callers (the
        // D1 config source, the CF auto-gen seeder) on the cache fast path.
        for table in [CachedTable::Variables, CachedTable::BlockSettings] {
            let opts = block_list_opts(table, "WAFER_RUN__AUTH");
            assert_eq!(
                read_key(table, &opts),
                Some(format_key(table, "WAFER_RUN__AUTH")),
                "block_list_opts must round-trip through read_key for {table:?}"
            );
        }
    }

    #[test]
    fn read_key_wrong_column_returns_none() {
        let opts = canonical_opts("key", "SOME_VAR");
        assert_eq!(read_key(CachedTable::Variables, &opts), None);
    }

    #[test]
    fn read_key_multiple_filters_returns_none() {
        let mut opts = canonical_opts("block", "WAFER_RUN__AUTH");
        opts.filters.push(Filter {
            field: "key".into(),
            operator: FilterOp::Equal,
            value: serde_json::Value::String("JWT_SECRET".into()),
        });
        assert_eq!(read_key(CachedTable::Variables, &opts), None);
    }

    #[test]
    fn read_key_no_filters_returns_none() {
        let opts = ListOptions {
            limit: Some(10_000),
            skip_count: true,
            ..Default::default()
        };
        assert_eq!(read_key(CachedTable::Variables, &opts), None);
    }

    #[test]
    fn read_key_non_equal_op_returns_none() {
        let mut opts = canonical_opts("block", "WAFER_RUN__AUTH");
        opts.filters[0].operator = FilterOp::NotEqual;
        assert_eq!(read_key(CachedTable::Variables, &opts), None);
    }

    #[test]
    fn read_key_skip_count_false_returns_none() {
        let mut opts = canonical_opts("block", "WAFER_RUN__AUTH");
        opts.skip_count = false;
        assert_eq!(read_key(CachedTable::Variables, &opts), None);
    }

    #[test]
    fn read_key_nonzero_offset_returns_none() {
        let mut opts = canonical_opts("block", "WAFER_RUN__AUTH");
        opts.offset = 100;
        assert_eq!(read_key(CachedTable::Variables, &opts), None);
    }

    #[test]
    fn read_key_small_limit_returns_none() {
        let mut opts = canonical_opts("block", "WAFER_RUN__AUTH");
        opts.limit = Some(50);
        assert_eq!(read_key(CachedTable::Variables, &opts), None);
    }

    #[test]
    fn read_key_non_string_value_returns_none() {
        let mut opts = canonical_opts("block", "WAFER_RUN__AUTH");
        opts.filters[0].value = serde_json::Value::Number(42.into());
        assert_eq!(read_key(CachedTable::Variables, &opts), None);
    }

    #[test]
    fn read_key_sort_set_returns_none() {
        use wafer_block::db::SortField;
        let mut opts = canonical_opts("block", "WAFER_RUN__AUTH");
        opts.sort.push(SortField {
            field: "key".into(),
            desc: false,
        });
        assert_eq!(read_key(CachedTable::Variables, &opts), None);
    }

    use std::collections::HashMap;

    fn row(field: &str, value: serde_json::Value) -> HashMap<String, serde_json::Value> {
        let mut m = HashMap::new();
        m.insert(field.into(), value);
        m
    }

    #[test]
    fn write_key_variables_extracts_block() {
        let r = row("block", serde_json::Value::String("WAFER_RUN__AUTH".into()));
        assert_eq!(
            write_key(CachedTable::Variables, &r),
            Some("cfg:v1:variables:WAFER_RUN__AUTH".to_string())
        );
    }

    #[test]
    fn write_key_block_settings_extracts_block_name() {
        let r = row(
            "block_name",
            serde_json::Value::String("wafer-run/registry".into()),
        );
        assert_eq!(
            write_key(CachedTable::BlockSettings, &r),
            Some("cfg:v1:block_settings:wafer-run/registry".to_string())
        );
    }

    #[test]
    fn write_key_missing_column_returns_none() {
        let r = row("key", serde_json::Value::String("JWT_SECRET".into()));
        assert_eq!(write_key(CachedTable::Variables, &r), None);
    }

    #[test]
    fn write_key_non_string_value_returns_none() {
        let r = row("block", serde_json::Value::Number(42.into()));
        assert_eq!(write_key(CachedTable::Variables, &r), None);
    }

    #[test]
    fn write_key_empty_row_returns_none() {
        let r: HashMap<String, serde_json::Value> = HashMap::new();
        assert_eq!(write_key(CachedTable::Variables, &r), None);
    }

    // --- Full-table block_settings read (`block_settings::read_rows`) ---

    /// The shape `platform_state::block_settings::read_rows` actually issues:
    /// no filter, full limit, skip_count, no offset, no sort.
    fn full_table_opts() -> ListOptions {
        ListOptions {
            offset: 0,
            limit: Some(10_000),
            skip_count: true,
            ..Default::default()
        }
    }

    #[test]
    fn read_key_block_settings_full_table_shape() {
        assert_eq!(
            read_key(CachedTable::BlockSettings, &full_table_opts()),
            Some("cfg:v1:block_settings:__all__".to_string())
        );
    }

    #[test]
    fn read_key_variables_full_table_returns_none() {
        // Variables is always read per-block; a filterless variables list is
        // not a recognized cache shape.
        assert_eq!(read_key(CachedTable::Variables, &full_table_opts()), None);
    }

    /// The shape `D1ConfigSource` actually issues for its one-query snapshot
    /// must stay uncacheable, and that is load-bearing rather than incidental.
    ///
    /// Caching it would need an invalidation story this module does not have:
    /// [`write_key`] derives per-block keys from the written row, so a
    /// variables write would leave an `__all__` entry serving pre-write config
    /// for its whole 24h TTL. It is also what keeps sensitive rows out of KV —
    /// an unfiltered read returns every row, secrets included, and the
    /// `list()` wrapper only reaches its sensitive-row guard for shapes it
    /// caches. If a future change makes this cacheable, both problems have to
    /// be solved first; this test is the tripwire.
    #[test]
    fn the_config_source_snapshot_shape_is_deliberately_uncacheable() {
        assert_eq!(
            read_key(CachedTable::Variables, &full_table_list_opts()),
            None
        );
        // The same constructor IS the recognized block_settings full-table
        // read, so the shape itself is right — only variables opts out.
        assert_eq!(
            read_key(CachedTable::BlockSettings, &full_table_list_opts()),
            Some("cfg:v1:block_settings:__all__".to_string())
        );
    }

    #[test]
    fn read_key_full_table_bad_shape_returns_none() {
        for mutate in [
            |o: &mut ListOptions| o.skip_count = false,
            |o: &mut ListOptions| o.offset = 100,
            |o: &mut ListOptions| o.limit = Some(50),
            |o: &mut ListOptions| {
                o.sort.push(wafer_block::db::SortField {
                    field: "block_name".into(),
                    desc: false,
                })
            },
        ] {
            let mut opts = full_table_opts();
            mutate(&mut opts);
            assert_eq!(read_key(CachedTable::BlockSettings, &opts), None);
        }
    }

    /// The all-rows sentinel must never collide with a real per-block key,
    /// because block names are slash-delimited and the sentinel is not.
    #[test]
    fn full_table_key_distinct_from_per_block_keys() {
        let all = read_key(CachedTable::BlockSettings, &full_table_opts());
        let per_block = read_key(
            CachedTable::BlockSettings,
            &canonical_opts("block_name", "impresspress/admin"),
        );
        assert!(all.is_some() && per_block.is_some());
        assert_ne!(all, per_block);
    }

    // --- invalidate_keys ---

    #[test]
    fn invalidate_keys_variables_only_per_row() {
        let r = row("block", serde_json::Value::String("WAFER_RUN__AUTH".into()));
        assert_eq!(
            invalidate_keys(CachedTable::Variables, &r),
            vec!["cfg:v1:variables:WAFER_RUN__AUTH".to_string()]
        );
    }

    #[test]
    fn invalidate_keys_variables_missing_column_is_empty() {
        let r = row("key", serde_json::Value::String("JWT_SECRET".into()));
        assert!(invalidate_keys(CachedTable::Variables, &r).is_empty());
    }

    #[test]
    fn invalidate_keys_block_settings_includes_per_row_and_all() {
        let r = row(
            "block_name",
            serde_json::Value::String("wafer-run/registry".into()),
        );
        assert_eq!(
            invalidate_keys(CachedTable::BlockSettings, &r),
            vec![
                "cfg:v1:block_settings:wafer-run/registry".to_string(),
                "cfg:v1:block_settings:__all__".to_string(),
            ]
        );
    }

    /// Even when the per-row key can't be extracted, the full-table key must
    /// still be invalidated so the cached `block_settings::read_rows` read
    /// can't go stale.
    #[test]
    fn invalidate_keys_block_settings_missing_column_still_drops_all() {
        let r = row("id", serde_json::Value::String("bs_123".into()));
        assert_eq!(
            invalidate_keys(CachedTable::BlockSettings, &r),
            vec!["cfg:v1:block_settings:__all__".to_string()]
        );
    }

    #[test]
    fn bumps_config_version_covers_runtime_affecting_tables() {
        assert!(bumps_config_version("impresspress__admin__variables"));
        assert!(bumps_config_version("impresspress__admin__block_settings"));
        assert!(bumps_config_version("impresspress__admin__wrap_grants"));
    }

    #[test]
    fn bumps_config_version_false_for_runtime_read_tables() {
        // roles/permissions/user_roles are read fresh from D1 per request —
        // no cached-runtime state depends on them, so no bump.
        assert!(!bumps_config_version("impresspress__admin__roles"));
        assert!(!bumps_config_version("impresspress__admin__permissions"));
        assert!(!bumps_config_version("wafer_run__auth__users"));
        assert!(!bumps_config_version(""));
    }

    // --- invalidate_keys_for_update ---

    #[test]
    fn invalidate_keys_for_update_unions_old_and_new_identity() {
        let old = row("block", serde_json::Value::String("WAFER_RUN__AUTH".into()));
        let new = row(
            "block",
            serde_json::Value::String("WAFER_RUN__STRIPE".into()),
        );
        let keys = invalidate_keys_for_update(CachedTable::Variables, &old, &new);
        assert_eq!(
            keys,
            vec![
                "cfg:v1:variables:WAFER_RUN__AUTH".to_string(),
                "cfg:v1:variables:WAFER_RUN__STRIPE".to_string(),
            ],
            "an identity-field change must invalidate both the vacated old \
             key and the new key, not just one"
        );
    }

    #[test]
    fn invalidate_keys_for_update_dedupes_when_identity_unchanged() {
        let old = row("block", serde_json::Value::String("WAFER_RUN__AUTH".into()));
        let new = old.clone();
        let keys = invalidate_keys_for_update(CachedTable::Variables, &old, &new);
        assert_eq!(keys, vec!["cfg:v1:variables:WAFER_RUN__AUTH".to_string()]);
    }

    #[test]
    fn invalidate_keys_for_update_block_settings_all_rows_key_not_duplicated() {
        let old = row(
            "block_name",
            serde_json::Value::String("wafer-run/registry".into()),
        );
        let new = old.clone();
        let keys = invalidate_keys_for_update(CachedTable::BlockSettings, &old, &new);
        assert_eq!(
            keys,
            vec![
                "cfg:v1:block_settings:wafer-run/registry".to_string(),
                "cfg:v1:block_settings:__all__".to_string(),
            ]
        );
    }

    // --- row_is_sensitive ---

    fn variables_row(key: &str, sensitive: i64) -> HashMap<String, serde_json::Value> {
        let mut m = HashMap::new();
        m.insert("key".into(), serde_json::Value::String(key.into()));
        m.insert("sensitive".into(), serde_json::Value::from(sensitive));
        m
    }

    #[test]
    fn row_is_sensitive_true_when_flag_set() {
        assert!(row_is_sensitive(
            CachedTable::Variables,
            &variables_row("SITE_NAME", 1)
        ));
    }

    #[test]
    fn row_is_sensitive_true_for_secret_suffix_even_if_flag_unset() {
        assert!(row_is_sensitive(
            CachedTable::Variables,
            &variables_row("STRIPE_SECRET", 0)
        ));
    }

    #[test]
    fn row_is_sensitive_true_for_key_suffix_even_if_flag_unset() {
        assert!(row_is_sensitive(
            CachedTable::Variables,
            &variables_row("JWT_KEY", 0)
        ));
    }

    #[test]
    fn row_is_sensitive_true_for_a_declared_password_var_even_if_flag_unset() {
        // `WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_PASSWORD` is declared
        // `InputType::Password` and spelled with neither `_SECRET` nor `_KEY`,
        // so a row an older build stored unflagged was judged KV-cacheable —
        // copying a plaintext admin password into a globally replicated store
        // for up to the 24h row TTL. The declaration is the only thing that
        // knows, so the masking predicate has to ask it.
        let key = crate::blocks::auth::config::BOOTSTRAP_ADMIN_PASSWORD_KEY;
        assert!(
            !crate::config_vars::has_sensitive_suffix(key),
            "the point of this test is a key the suffix rule cannot catch"
        );
        assert!(row_is_sensitive(
            CachedTable::Variables,
            &variables_row(key, 0)
        ));
    }

    #[test]
    fn row_is_sensitive_false_for_plain_row() {
        assert!(!row_is_sensitive(
            CachedTable::Variables,
            &variables_row("SITE_NAME", 0)
        ));
    }

    #[test]
    fn row_is_sensitive_true_when_flag_set_as_string() {
        // A lazily-added column can round-trip as TEXT ("1") rather than a
        // JSON number. `row_is_sensitive` must accept that the same way the
        // display-masking path (`crate::util::flag_is_set`) does, or a
        // string-stored sensitive flag would leak into the KV cache while
        // still being masked on display — see the parity note on
        // `row_is_sensitive`.
        let mut r = variables_row("SITE_NAME", 0);
        r.insert(
            "sensitive".into(),
            serde_json::Value::String("1".to_string()),
        );
        assert!(row_is_sensitive(CachedTable::Variables, &r));
    }

    #[test]
    fn row_is_sensitive_false_for_missing_columns() {
        let r: HashMap<String, serde_json::Value> = HashMap::new();
        assert!(!row_is_sensitive(CachedTable::Variables, &r));
    }

    #[test]
    fn row_is_sensitive_always_false_for_block_settings() {
        // block_settings rows have no "sensitive"/"key" columns at all —
        // they're per-block enablement flags, never secrets.
        let r = row(
            "block_name",
            serde_json::Value::String("wafer-run/registry".into()),
        );
        assert!(!row_is_sensitive(CachedTable::BlockSettings, &r));
    }

    #[test]
    fn config_version_key_is_distinct_from_row_cache_keys() {
        // Row cache keys are "cfg:v1:{variables|block_settings}:{value}".
        assert!(CONFIG_VERSION_KEY.starts_with("cfg:v1:"));
        assert!(!CONFIG_VERSION_KEY.starts_with("cfg:v1:variables:"));
        assert!(!CONFIG_VERSION_KEY.starts_with("cfg:v1:block_settings:"));
    }

    /// Pins the invariant: every table `classify_table` recognizes as
    /// KV-row-cached must also bump the config version, or a cached runtime
    /// could keep serving stale rows forever after a write to that table.
    ///
    /// `classify_table` matches individual table constants rather than
    /// iterating a shared list, so this test can't literally replay its
    /// match arms. Instead it exhaustively matches every `CachedTable`
    /// variant with NO wildcard arm: adding a new variant (i.e. a new
    /// cached table) without updating this test is a compile error here,
    /// not a silent gap. If you just added a table to `classify_table`,
    /// add its variant + constant below and confirm `bumps_config_version`
    /// covers it too.
    #[test]
    fn every_classified_table_bumps_config_version() {
        for table in [CachedTable::Variables, CachedTable::BlockSettings] {
            let table_name = match table {
                CachedTable::Variables => variables::TABLE,
                CachedTable::BlockSettings => block_settings::TABLE,
            };
            assert_eq!(
                classify_table(table_name),
                Some(table),
                "{table_name} no longer classifies as {table:?} — test is stale"
            );
            assert!(
                bumps_config_version(table_name),
                "{table_name} is KV-row-cached ({table:?} via classify_table) but \
                 bumps_config_version() is false — every cached table must bump \
                 the config version so cached runtimes don't serve stale state"
            );
        }
    }
}
