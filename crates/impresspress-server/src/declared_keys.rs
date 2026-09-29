//! App-specific config loading (schema-aware, depends on impresspress-core).
//!
//! `filter_to_declared_keys` sits between the library's raw-env-var
//! collection and the SQLite-backed variable seeding, preserving the
//! prior behavior of only persisting env vars that match a declared
//! block/shared config var key.

use std::collections::HashMap;

/// Keep the env vars whose key some block or shared `ConfigVar` declares.
pub fn filter_to_declared_keys(env_vars: HashMap<String, String>) -> Vec<(String, String)> {
    // `config_vars::is_declared_key` owns this rule, over a memoized set.
    // This used to re-derive it by hand — `all_block_infos()` +
    // `collect_all_config_vars()` into a local `HashSet`, rebuilding every
    // block on every call — which is the second copy of a rule that CLAUDE.md
    // forbids and that this change set argues against everywhere else.
    env_vars
        .into_iter()
        .filter(|(key, _)| impresspress_core::config_vars::is_declared_key(key))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use impresspress_core::config_vars::{APP_NAME_KEY, DEPLOY_TOKEN_KEY};

    use super::filter_to_declared_keys;

    fn filtered(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        filter_to_declared_keys(
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
        )
        .into_iter()
        .collect()
    }

    /// A declared shared var survives, and an undeclared key does not. This is
    /// the gate in front of `platform_state::variables::seed_and_load` on
    /// native: whatever it drops, no amount of correctness downstream can
    /// recover.
    #[test]
    fn a_declared_shared_var_survives_and_an_undeclared_key_does_not() {
        const UNDECLARED: &str = "WAFER_RUN_SHARED__NOT_A_REAL_VAR";
        let out = filtered(&[
            (APP_NAME_KEY, "Foo"),
            (UNDECLARED, "x"),
            (DEPLOY_TOKEN_KEY, "tok"),
        ]);
        assert_eq!(out.get(APP_NAME_KEY).map(String::as_str), Some("Foo"));
        assert!(!out.contains_key(UNDECLARED));
        assert!(!out.contains_key(DEPLOY_TOKEN_KEY));
    }

    /// The JWT signing secret does NOT reach the variables seeder from the
    /// process environment, and this test exists so nobody documents that it
    /// does.
    ///
    /// `WAFER_RUN__AUTH__JWT_SECRET` is declared by no `ConfigVar` — neither
    /// `blocks::auth::config::auth_config_vars` nor any `BlockInfo` in
    /// `all_block_infos()` (which does not include `wafer-run/auth` at all) —
    /// so this filter removes it and
    /// `platform_state::variables::seed_jwt_secret` auto-generates one
    /// instead. Exporting the variable has no effect, which is why
    /// `examples/run-tests.sh` exporting it buys nothing.
    ///
    /// The supported channel for pinning the secret is the admin surface:
    /// `admin::ops::reject_runtime_owned_key` deliberately exempts this key
    /// from its refusal. Making the env var work means declaring the key,
    /// which pulls it into the admin config tables, `seed_defaults` and the
    /// auto-generate loop — a design decision with its own blast radius, not
    /// something to smuggle in as a filter exception.
    #[test]
    fn the_jwt_secret_is_not_a_declared_key_and_so_never_reaches_the_seeder() {
        let key = impresspress_core::blocks::auth::JWT_SECRET_KEY;
        let out = filtered(&[(key, "pinned")]);
        assert!(
            !out.contains_key(key),
            "the JWT secret is undeclared, so the native env batch cannot carry it"
        );
    }
}

// Block-settings loading + the #222 hash-gate seed are handled by the shared
// `impresspress_core::platform_state::block_settings::load_and_seed`, and admin-created
// WRAP grants by `impresspress_core::platform_state::wrap_grants::load`, both over
// the platform `DatabaseService` (see `build_native_runtime`).
