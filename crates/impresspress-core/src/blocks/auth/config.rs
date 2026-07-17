//! Config vars and runtime config struct for the `wafer-run/auth` block.
//!
//! Two complementary surfaces:
//!
//! - [`auth_config_vars`] declares the `ConfigVar`s the block contributes to
//!   `BlockInfo::config_keys`, so the admin UI and validator see them. These
//!   are appended to the existing (legacy JWT-based) vars in `mod.rs`.
//! - [`AuthConfig`] is the runtime view — populated from `wafer-run/config` at
//!   `Init` time (or from a `HashMap` in tests via [`AuthConfig::from_env_for_test`]).
//!   Downstream handlers consume it without re-reading config on every call.
//!
//! Naming follows CLAUDE.md's three-tier convention:
//! - `WAFER_RUN_SHARED__AUTH__*` — shared auth config, admin-writable.

use std::collections::HashMap;

use wafer_core::clients::config as config_client;
use wafer_run::{context::Context, ConfigVar, InputType};

/// `WAFER_RUN_SHARED__AUTH__SESSION_LIFETIME_DAYS` — how many days a freshly
/// issued session cookie stays valid.
pub const SESSION_LIFETIME_DAYS_KEY: &str = "WAFER_RUN_SHARED__AUTH__SESSION_LIFETIME_DAYS";

/// `WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_EMAIL` — email of the admin user to
/// create on first startup.
pub const BOOTSTRAP_ADMIN_EMAIL_KEY: &str = "WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_EMAIL";

/// `WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_PASSWORD` — password for the
/// bootstrap admin. Paired with the email key.
pub const BOOTSTRAP_ADMIN_PASSWORD_KEY: &str = "WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_PASSWORD";

/// `WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_TOKEN` — one-time token used when
/// no email/password is provided. Hashed and stored in `bootstrap_tokens`;
/// the holder redeems it to create the first admin.
pub const BOOTSTRAP_ADMIN_TOKEN_KEY: &str = "WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_TOKEN";

/// `WAFER_RUN_SHARED__AUTH__PASSWORD_MIN_LENGTH` — minimum password length
/// enforced at signup. Existing accounts are not re-validated.
pub const PASSWORD_MIN_LENGTH_KEY: &str = "WAFER_RUN_SHARED__AUTH__PASSWORD_MIN_LENGTH";

/// `WAFER_RUN__AUTH__ACCESS_TOKEN_LIFETIME_SECS` — how many seconds a freshly
/// issued JWT access token stays valid (SEC-042). 30 min default keeps the
/// post-logout exposure window short while remaining forgiving enough that
/// regular use never hits the natural-expiry path on every request.
pub const ACCESS_TOKEN_LIFETIME_SECS_KEY: &str = "WAFER_RUN__AUTH__ACCESS_TOKEN_LIFETIME_SECS";

/// `WAFER_RUN__AUTH__REQUIRE_VERIFICATION` — when `true`, users must verify
/// their email before they can log in. Read by login/signup/refresh.
pub const REQUIRE_VERIFICATION_KEY: &str = "WAFER_RUN__AUTH__REQUIRE_VERIFICATION";

/// `WAFER_RUN__AUTH__ALLOWED_EMAIL_DOMAINS` — comma-separated allowlist of
/// signup email domains. Empty (the default) allows any domain.
pub const ALLOWED_EMAIL_DOMAINS_KEY: &str = "WAFER_RUN__AUTH__ALLOWED_EMAIL_DOMAINS";

/// Default session lifetime when the config var is unset.
pub const SESSION_LIFETIME_DAYS_DEFAULT: u32 = 30;

/// Default value for [`PASSWORD_MIN_LENGTH_KEY`].
pub const PASSWORD_MIN_LENGTH_DEFAULT: u32 = 8;

/// Default access-token lifetime in seconds (30 min). Reduced from the
/// previous hardcoded 24h to limit the SEC-042 exposure window when a JWT
/// is stolen or a user logs out before the natural expiry.
pub const ACCESS_TOKEN_LIFETIME_SECS_DEFAULT: u64 = 1800;

/// Hard upper bound on [`ACCESS_TOKEN_LIFETIME_SECS_KEY`], enforced by
/// [`super::helpers::access_token_lifetime_secs`] regardless of what an
/// admin configures (P2c: CODE_REVIEW_2026-07-16, "Access JWTs outlive
/// account and role changes"). `auth_version` (see `super::current_auth_version`)
/// is the primary fix for stale-JWT exposure — it invalidates an
/// already-issued token on the next request after a bump, bounded only by
/// the short verify-side cache TTL — but it's still one mechanism, checked
/// on one code path. This cap is the belt-and-suspenders backstop: even if
/// `auth_version` were ever bypassed or buggy for some request path, an
/// admin cannot configure a token that stays valid indefinitely by raw
/// expiry alone. 24h matches the pre-SEC-042 default, so existing "long
/// session" deployments aren't silently clamped tighter than they were
/// before that hardening landed.
pub const ACCESS_TOKEN_LIFETIME_SECS_MAX: u64 = 86_400;

/// Config vars contributed by the Plan A2 auth block additions.
///
/// Appended to the existing legacy `config_keys` list; do not duplicate or
/// re-order with the existing vars.
pub fn auth_config_vars() -> Vec<ConfigVar> {
    vec![
        ConfigVar::new(
            SESSION_LIFETIME_DAYS_KEY,
            "Lifetime of a session cookie in days (applied at issuance)",
            &SESSION_LIFETIME_DAYS_DEFAULT.to_string(),
        )
        .name("Session Lifetime (days)"),
        ConfigVar::new(
            BOOTSTRAP_ADMIN_EMAIL_KEY,
            "Email of the admin user created on first startup",
            "",
        )
        .name("Bootstrap Admin Email")
        .optional(),
        ConfigVar::new(
            BOOTSTRAP_ADMIN_PASSWORD_KEY,
            "Password for the bootstrap admin account",
            "",
        )
        .name("Bootstrap Admin Password")
        .input_type(InputType::Password)
        .optional(),
        ConfigVar::new(
            BOOTSTRAP_ADMIN_TOKEN_KEY,
            "One-time token for provisioning the first admin (sha256 stored)",
            "",
        )
        .name("Bootstrap Admin Token")
        .input_type(InputType::Password)
        .optional(),
        ConfigVar::new(
            PASSWORD_MIN_LENGTH_KEY,
            "Minimum password length enforced at signup. Existing accounts are not re-validated.",
            &PASSWORD_MIN_LENGTH_DEFAULT.to_string(),
        )
        .name("Password Minimum Length"),
        ConfigVar::new(
            ACCESS_TOKEN_LIFETIME_SECS_KEY,
            "Lifetime of an issued JWT access token in seconds. Shorter values reduce the SEC-042 exposure window when a JWT leaks; longer values reduce refresh churn. Logout invalidates the in-flight JWT regardless via the blocklist, and password/disable/role changes invalidate it via auth_version. Hard-capped at 86400 (24h) regardless of this value.",
            &ACCESS_TOKEN_LIFETIME_SECS_DEFAULT.to_string(),
        )
        .name("Access Token Lifetime (seconds)"),
    ]
}

/// Block-scoped (`WAFER_RUN__AUTH__*`) auth-identity config vars that the
/// auth flows read directly via the config client (verification gate + signup
/// domain allowlist). Declared here as [`ConfigVar`] metadata so the auth_ui
/// admin settings page renders them through `ui::settings_form` instead of a
/// hand-maintained tuple table. They are not contributed to a `BlockInfo`
/// because there is no standalone `wafer-run/auth` Block — `auth/` is a
/// library module consumed by `auth_ui` — but they remain ConfigVar-declared
/// in one place (no-hardcoded-lists rule).
pub fn auth_identity_config_vars() -> Vec<ConfigVar> {
    vec![
        ConfigVar::new(
            REQUIRE_VERIFICATION_KEY,
            "Require users to verify their email before they can log in.",
            "false",
        )
        .name("Require Email Verification")
        .input_type(InputType::Toggle),
        ConfigVar::new(
            ALLOWED_EMAIL_DOMAINS_KEY,
            "Restrict signup to specific email domains (comma-separated, e.g. \"company.com,org.com\"). Leave empty to allow all.",
            "",
        )
        .name("Allowed Email Domains")
        .input_type(InputType::Text),
    ]
}

/// Runtime view of the auth block's config.
///
/// Populated once at `Init` time from `wafer-run/config` or, in tests, from
/// a `HashMap` via [`AuthConfig::from_env_for_test`]. Consuming handlers read
/// from this struct rather than reaching back to the config client per-call.
#[derive(Debug, Clone)]
pub struct AuthConfig {
    pub session_lifetime_days: u32,
    pub bootstrap_admin_email: Option<String>,
    pub bootstrap_admin_password: Option<String>,
    pub bootstrap_admin_token: Option<String>,
}

impl AuthConfig {
    /// Construct from a fully-populated `HashMap`. Missing keys fall back to
    /// declared defaults; empty strings are treated as absent for the optional
    /// bootstrap vars (so shell exports like `FOO=""` do not accidentally
    /// trigger the bootstrap email+password path).
    pub fn from_map(env: &HashMap<String, String>) -> Self {
        let session_lifetime_days = env
            .get(SESSION_LIFETIME_DAYS_KEY)
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(SESSION_LIFETIME_DAYS_DEFAULT);
        Self {
            session_lifetime_days,
            bootstrap_admin_email: non_empty(env.get(BOOTSTRAP_ADMIN_EMAIL_KEY)),
            bootstrap_admin_password: non_empty(env.get(BOOTSTRAP_ADMIN_PASSWORD_KEY)),
            bootstrap_admin_token: non_empty(env.get(BOOTSTRAP_ADMIN_TOKEN_KEY)),
        }
    }

    /// Build an [`AuthConfig`] by reading from the runtime config client.
    ///
    /// Called once at `Init` time from `AuthBlock::lifecycle`. Each key is
    /// fetched with its declared default via `config_client::get_default` so
    /// the behaviour matches the `BlockInfo::config_keys` declarations above.
    pub async fn from_ctx(ctx: &dyn Context) -> Self {
        let mut env = HashMap::new();
        for key in &[
            SESSION_LIFETIME_DAYS_KEY,
            BOOTSTRAP_ADMIN_EMAIL_KEY,
            BOOTSTRAP_ADMIN_PASSWORD_KEY,
            BOOTSTRAP_ADMIN_TOKEN_KEY,
        ] {
            let val = config_client::get_default(ctx, key, "").await;
            if !val.is_empty() {
                env.insert(key.to_string(), val);
            }
        }
        Self::from_map(&env)
    }

    /// Test helper: build an [`AuthConfig`] from a slice of `(key, value)`
    /// pairs. Unlisted keys pick up the declared defaults, matching what the
    /// config client does in production.
    pub fn from_env_for_test(pairs: &[(&str, &str)]) -> Self {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        Self::from_map(&map)
    }
}

fn non_empty<S: AsRef<str>>(s: Option<S>) -> Option<String> {
    s.map(|s| s.as_ref().to_string()).filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_lifetime_days_defaults_to_30_when_unset() {
        let cfg = AuthConfig::from_env_for_test(&[]);
        assert_eq!(cfg.session_lifetime_days, 30);
    }

    #[test]
    fn session_lifetime_days_parses_int() {
        let cfg = AuthConfig::from_env_for_test(&[(SESSION_LIFETIME_DAYS_KEY, "7")]);
        assert_eq!(cfg.session_lifetime_days, 7);
    }

    #[test]
    fn bootstrap_admin_vars_are_captured() {
        let cfg = AuthConfig::from_env_for_test(&[
            (BOOTSTRAP_ADMIN_EMAIL_KEY, "a@b.c"),
            (BOOTSTRAP_ADMIN_PASSWORD_KEY, "pw"),
        ]);
        assert_eq!(cfg.bootstrap_admin_email.as_deref(), Some("a@b.c"));
        assert_eq!(cfg.bootstrap_admin_password.as_deref(), Some("pw"));
        assert!(cfg.bootstrap_admin_token.is_none());
    }

    #[test]
    fn bootstrap_token_only() {
        let cfg = AuthConfig::from_env_for_test(&[(BOOTSTRAP_ADMIN_TOKEN_KEY, "secret")]);
        assert_eq!(cfg.bootstrap_admin_token.as_deref(), Some("secret"));
        assert!(cfg.bootstrap_admin_email.is_none());
    }

    #[test]
    fn empty_string_bootstrap_vars_are_treated_as_absent() {
        let cfg = AuthConfig::from_env_for_test(&[
            (BOOTSTRAP_ADMIN_EMAIL_KEY, ""),
            (BOOTSTRAP_ADMIN_PASSWORD_KEY, ""),
            (BOOTSTRAP_ADMIN_TOKEN_KEY, ""),
        ]);
        assert!(cfg.bootstrap_admin_email.is_none());
        assert!(cfg.bootstrap_admin_password.is_none());
        assert!(cfg.bootstrap_admin_token.is_none());
    }

    #[test]
    fn auth_config_vars_declares_all_four_keys() {
        let vars = auth_config_vars();
        let keys: Vec<&str> = vars.iter().map(|v| v.key.as_str()).collect();
        assert!(keys.contains(&SESSION_LIFETIME_DAYS_KEY));
        assert!(keys.contains(&BOOTSTRAP_ADMIN_EMAIL_KEY));
        assert!(keys.contains(&BOOTSTRAP_ADMIN_PASSWORD_KEY));
        assert!(keys.contains(&BOOTSTRAP_ADMIN_TOKEN_KEY));
    }

    #[test]
    fn bootstrap_password_is_marked_secret() {
        let var = auth_config_vars()
            .into_iter()
            .find(|v| v.key == BOOTSTRAP_ADMIN_PASSWORD_KEY)
            .expect("password var declared");
        assert!(var.is_sensitive(), "bootstrap password must be sensitive");
        assert!(var.optional, "bootstrap password must be optional");
    }

    #[test]
    fn bootstrap_token_is_marked_secret_and_optional() {
        let var = auth_config_vars()
            .into_iter()
            .find(|v| v.key == BOOTSTRAP_ADMIN_TOKEN_KEY)
            .expect("token var declared");
        assert!(var.is_sensitive());
        assert!(var.optional);
    }

    #[test]
    fn session_lifetime_var_has_default_of_30() {
        let var = auth_config_vars()
            .into_iter()
            .find(|v| v.key == SESSION_LIFETIME_DAYS_KEY)
            .expect("session var declared");
        assert_eq!(var.default, "30");
        // Not optional — session lifetime is always needed and always has a default.
        assert!(!var.optional);
    }

    #[test]
    fn auth_config_vars_declares_password_min_length() {
        let vars = auth_config_vars();
        assert!(vars
            .iter()
            .map(|v| v.key.as_str())
            .any(|k| k == PASSWORD_MIN_LENGTH_KEY));
    }

    #[test]
    fn auth_config_vars_does_not_declare_dead_signup_enabled_key() {
        // WAFER_RUN_SHARED__AUTH__SIGNUP_ENABLED was a dead duplicate of the
        // shared WAFER_RUN_SHARED__ALLOW_SIGNUP toggle (opposite default, no
        // reader) and has been removed. The admin UI must not advertise it.
        let vars = auth_config_vars();
        assert!(
            !vars
                .iter()
                .any(|v| v.key == "WAFER_RUN_SHARED__AUTH__SIGNUP_ENABLED"),
            "dead SIGNUP_ENABLED var must not be advertised"
        );
    }

    #[test]
    fn password_min_length_var_defaults_to_eight() {
        let var = auth_config_vars()
            .into_iter()
            .find(|v| v.key == PASSWORD_MIN_LENGTH_KEY)
            .expect("password_min_length declared");
        assert_eq!(var.default, "8");
    }
}
